//! USBPD, USB Power Delivery
//!
//! The asynchronous PHY owns reception in its interrupt handler. Every
//! accepted SOP frame is copied into a small queue, acknowledged with GoodCRC
//! after the inter-frame gap, and the receiver is re-armed without waiting
//! for the PD task. The task only consumes queued frames and starts its own
//! transmissions, so executor latency cannot delay GoodCRC or leave the
//! receiver disarmed. The PD protocol layer must therefore treat this PHY as
//! a driver with automatic GoodCRC and software retries.
//!
//! Design:
//!
//! - CC Pins:
//! - UsbPdPhy: USBPD PHY layer
//! - UsbPdSniffer: USBPD Sniffer based on PHY layer, no transmit support
//! - [ ] UsbPdSink: USBPD Sink layer
//! - [ ] UsbPdSource: USBPD Source layer

use core::cell::UnsafeCell;
use core::future::poll_fn;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use core::task::Poll;

use embassy_sync::waitqueue::AtomicWaker;
use pac::InterruptNumber;

use crate::gpio::Pull;
use crate::mode::{Async, Blocking, Mode};
use crate::pac::usbpd::vals;
use crate::{interrupt, pac, Peri, PeripheralType, RccPeripheral};

mod rx_queue;
#[cfg(feature = "usbpd-driver-trace")]
mod trace;

use rx_queue::{good_crc_header, is_good_crc, message_length, RxRing};
#[cfg(feature = "usbpd-driver-trace")]
pub use trace::{
    set_usbpd_trace_callback, UsbPdTraceCallback, UsbPdTraceCode, UsbPdTraceEvent, UsbPdTraceEventKind,
    USBPD_TRACE_ABI_VERSION,
};

/// Maximum PD message size excluding the four-byte CRC appended by the PHY.
pub const MAX_MESSAGE_BYTES: usize = rx_queue::MESSAGE_BYTES;
const RX_DMA_BYTES: usize = MAX_MESSAGE_BYTES + 4;
/// Frames buffered between the interrupt handler and the PD task.
const RX_QUEUE_FRAMES: usize = 4;
/// USB PD requires at least 25 us (tInterFrameGap) between frames. WCH's
/// reference sink waits 30 us before answering with GoodCRC; the spec allows
/// up to 195 us (tTransmit).
const GOOD_CRC_GAP_US: u32 = 30;
/// Upper bound for a transmission to reach TX-end. A maximum 30-byte frame
/// occupies the line for about 1.5 ms.
#[cfg(feature = "embassy")]
const TRANSFER_TIMEOUT_MS: u64 = 5;

/// The receiver has not been armed since reset.
const PHASE_IDLE: u8 = 0;
/// The receiver is armed; the interrupt handler owns the line.
const PHASE_RX: u8 = 1;
/// The interrupt handler is transmitting GoodCRC.
const PHASE_ACK: u8 = 2;
/// A task transmission is in progress.
const PHASE_TX: u8 = 3;

#[derive(Debug)]
pub enum Error {
    Rejected,
    Timeout,
    CCNotConnected,
    NotSupported,
    HardReset,
    /// PHY reported a buffer/DMA error; the current transfer must be considered failed.
    BufferError,
    /// The destination supplied to `receive` cannot hold the complete message.
    ReceiveBufferTooSmall {
        required: usize,
        available: usize,
    },
    /// A PD message cannot be represented by the CH32 USBPD peripheral.
    MessageTooLong {
        length: usize,
        maximum: usize,
    },
    /// Unexpected message type
    Protocol(u8),
    MaxRetry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sop {
    Sop = 0b00_00_00_00,
    SopPrime = 0b00_00_01_01,
    SopDoublePrime = 0b00_01_00_01,
    HardReset = 0b10_10_10_01,
}

/// Interrupt handler.
pub struct InterruptHandler<T: Instance> {
    _phantom: PhantomData<T>,
}

impl<T: Instance> interrupt::typelevel::Handler<T::Interrupt> for InterruptHandler<T> {
    unsafe fn on_interrupt() {
        let regs = T::REGS;
        let state = T::state();

        let status = regs.status().read();
        let byte_count = usize::from(regs.bmc_byte_cnt().read().bmc_byte_cnt());
        // Clear exactly the flags observed; a later event raises its own IRQ.
        regs.status().write_value(status);

        #[cfg(feature = "usbpd-driver-trace")]
        emit_trace::<T>(UsbPdTraceEventKind::Interrupt, UsbPdTraceCode::Interrupt, status.0);

        let phase = state.phase.load(Ordering::Relaxed);
        let transmitting = phase == PHASE_ACK || phase == PHASE_TX;
        let transfer_ended = transmitting && (status.if_tx_end() || status.buf_err());
        if transfer_ended {
            // TX has stopped (or faulted); release the CC transmitter before
            // the receiver is armed again.
            release_cc::<T>();
        }

        // RX-reset detection is disabled during our own transmissions, but
        // the flag may still latch a partner Hard Reset that arrived then.
        // Our own Hard Reset signaling is not a received reset.
        let own_hard_reset = phase == PHASE_TX && state.tx_hard_reset.load(Ordering::Relaxed);
        if status.if_rx_reset() && phase != PHASE_IDLE && !own_hard_reset {
            state.hard_resets.store(
                state.hard_resets.load(Ordering::Relaxed).wrapping_add(1),
                Ordering::Release,
            );
            if transmitting {
                release_cc::<T>();
            }
            if phase == PHASE_TX {
                finish_transmit(state, false);
            }
            arm_receive::<T>();
        } else if transfer_ended {
            if phase == PHASE_TX {
                finish_transmit(state, !status.buf_err());
            }
            arm_receive::<T>();
        } else if phase == PHASE_RX && (status.if_rx_act() || status.buf_err()) {
            receive_complete::<T>(status, byte_count);
        }

        state
            .events
            .store(state.events.load(Ordering::Relaxed).wrapping_add(1), Ordering::Release);
        state.waker.wake();
    }
}

/// Queue a completed frame, answer it with GoodCRC, and keep receiving.
fn receive_complete<T: Instance>(status: pac::usbpd::regs::Status, byte_count: usize) {
    let state = T::state();
    if status.if_rx_act() && !status.buf_err() && status.bmc_aux() == vals::BmcAux::SOP0 {
        // SAFETY: DMA into the staging buffer has completed, and only this
        // handler re-arms it (below or after GoodCRC).
        let frame = unsafe { &(*state.rx.get()).data };
        if let Some(length) = message_length(frame, byte_count) {
            let header = u16::from_le_bytes([frame[0], frame[1]]);
            if state.ring.push(&frame[..length]) {
                if !is_good_crc(header) {
                    #[cfg(feature = "usbpd-driver-trace")]
                    emit_trace::<T>(UsbPdTraceEventKind::IsrFrame, UsbPdTraceCode::GoodCrcSent, status.0);
                    send_good_crc::<T>(good_crc_header(header));
                    return;
                }
                #[cfg(feature = "usbpd-driver-trace")]
                emit_trace::<T>(UsbPdTraceEventKind::IsrFrame, UsbPdTraceCode::GoodCrcQueued, status.0);
            } else {
                // The partner retries an unacknowledged frame once the task
                // has drained the queue.
                #[cfg(feature = "usbpd-driver-trace")]
                emit_trace::<T>(UsbPdTraceEventKind::IsrFrame, UsbPdTraceCode::QueueFull, status.0);
            }
        } else {
            #[cfg(feature = "usbpd-driver-trace")]
            emit_trace::<T>(UsbPdTraceEventKind::IsrFrame, UsbPdTraceCode::Dropped, status.0);
        }
    } else {
        #[cfg(feature = "usbpd-driver-trace")]
        emit_trace::<T>(UsbPdTraceEventKind::IsrFrame, UsbPdTraceCode::Dropped, status.0);
    }
    arm_receive::<T>();
}

fn send_good_crc<T: Instance>(header: u16) {
    let state = T::state();
    // SAFETY: only this handler writes the acknowledgement buffer, and no
    // GoodCRC is in flight while the receiver is armed.
    let ack = unsafe { &mut *state.ack.get() };
    ack.data[..2].copy_from_slice(&header.to_le_bytes());
    let address = ack.address();

    // Busy-wait instead of using SysTick `Delay`, which task code may be
    // using when this interrupt preempts it.
    qingke::riscv::asm::delay(GOOD_CRC_GAP_US * hclk_mhz());
    state.phase.store(PHASE_ACK, Ordering::Relaxed);
    start_transmit::<T>(Sop::Sop, address, 2);
}

/// Record the end of a task transmission. Called only from the interrupt
/// handler or from task code inside a critical section.
fn finish_transmit(state: &State, success: bool) {
    state.tx_success.store(success, Ordering::Relaxed);
    state
        .tx_done
        .store(state.tx_done.load(Ordering::Relaxed).wrapping_add(1), Ordering::Release);
}

fn release_cc<T: Instance>() {
    T::port_cc_reg(vals::CcSel::CC1).modify(|w| w.set_cc_lve(false));
    T::port_cc_reg(vals::CcSel::CC2).modify(|w| w.set_cc_lve(false));
}

/// Arm the receiver into the staging buffer with interrupts enabled. Called
/// only from the interrupt handler or from task code inside a critical
/// section.
fn arm_receive<T: Instance>() {
    let state = T::state();
    state.phase.store(PHASE_RX, Ordering::Relaxed);
    // SAFETY: the staging buffer's address is stable; DMA ownership passes
    // back to the peripheral here.
    let address = unsafe { &*state.rx.get() }.address();
    prepare_receive::<T, true>(address);
    #[cfg(feature = "usbpd-driver-trace")]
    emit_trace::<T>(
        UsbPdTraceEventKind::RxArmed,
        UsbPdTraceCode::IsrArm,
        T::REGS.status().read().0,
    );
}

/// Configure the peripheral to receive into `address`.
///
/// The asynchronous path enables receive interrupts; the blocking path polls
/// the same hardware sequence with the USBPD interrupt disabled.
fn prepare_receive<T: Instance, const ENABLE_INTERRUPTS: bool>(address: u16) {
    let usbpd = T::REGS;

    usbpd.config().modify(|w| w.set_pd_all_clr(true));
    usbpd.config().modify(|w| {
        w.set_pd_all_clr(false);
        if ENABLE_INTERRUPTS {
            w.set_ie_tx_end(false);
            w.set_ie_rx_act(true);
            w.set_ie_rx_reset(true);
        }
    });

    usbpd.dma().write_value(address);
    usbpd.control().modify(|w| w.set_pd_tx_en(false));
    usbpd.bmc_clk_cnt().modify(|w| w.set_bmc_clk_cnt(calc_bmc_clk_for_rx()));
    usbpd.control().modify(|w| w.set_bmc_start(true));
}

/// Start transmitting `length` bytes at DMA `address` (0 for Hard Reset).
fn start_transmit<T: Instance>(sop: Sop, address: u16, length: usize) {
    let regs = T::REGS;
    T::port_cc_reg(regs.config().read().cc_sel()).modify(|w| w.set_cc_lve(true));
    regs.bmc_clk_cnt().write(|w| w.set_bmc_clk_cnt(calc_bmc_clk_for_tx()));
    regs.dma().write_value(address);
    regs.tx_sel().write(|w| w.0 = sop as u8);
    regs.bmc_tx_sz().write(|w| w.set_bmc_tx_sz(length as _));
    regs.control().modify(|w| w.set_pd_tx_en(true));
    regs.status().write(|w| {
        w.set_if_tx_end(true);
        w.set_if_rx_reset(true);
        w.set_if_rx_act(true);
        w.set_if_rx_byte(true);
        w.set_if_rx_bit(true);
        w.set_buf_err(true);
    });
    regs.config().modify(|w| {
        w.set_ie_rx_act(false);
        w.set_ie_rx_reset(false);
        w.set_ie_tx_end(true);
    });
    regs.control().modify(|w| w.set_bmc_start(true));
}

#[cfg(feature = "usbpd-driver-trace")]
fn emit_trace<T: Instance>(kind: UsbPdTraceEventKind, code: UsbPdTraceCode, status: u8) {
    let config = T::REGS.config().read();
    trace::emit(UsbPdTraceEvent {
        kind,
        code: code as u8,
        status,
        active_cc: config.cc_sel().to_bits() + 1,
        byte_count: T::REGS.bmc_byte_cnt().read().bmc_byte_cnt(),
        config: config.0,
    });
}

#[cfg(feature = "usbpd-driver-trace")]
fn receive_result_code(result: &Result<(Sop, usize), Error>) -> UsbPdTraceCode {
    match result {
        Ok(_) => UsbPdTraceCode::Success,
        Err(Error::HardReset) => UsbPdTraceCode::HardReset,
        Err(Error::BufferError) => UsbPdTraceCode::BufferError,
        Err(Error::ReceiveBufferTooSmall { .. }) => UsbPdTraceCode::BufferTooSmall,
        Err(Error::Rejected | Error::Protocol(_)) => UsbPdTraceCode::Rejected,
        Err(_) => UsbPdTraceCode::Other,
    }
}

fn hclk_mhz() -> u32 {
    crate::rcc::clocks().hclk.0 / 1_000_000
}

/// Interrupt handler for the USB-PD port-level wake input.
///
/// The CH32X0 reference manual documents a wake level selected by
/// `WAKE_POLAR`; it has no corresponding status flag. The
/// handler therefore masks the source before waking the waiter. The waiter
/// samples the comparator level and re-arms it when necessary. Board-level
/// behavior still requires validation because WCH does not document the
/// interrupt's timing or filtering.
pub struct CcWakeInterruptHandler<T: Instance> {
    _phantom: PhantomData<T>,
}

impl<T: Instance> interrupt::typelevel::Handler<T::WakeupInterrupt> for CcWakeInterruptHandler<T> {
    unsafe fn on_interrupt() {
        T::REGS.config().modify(|w| w.set_ie_pd_io(false));
        let count = T::cc_state().wake_count.load(Ordering::Relaxed);
        T::cc_state().wake_count.store(count.wrapping_add(1), Ordering::Relaxed);
        T::cc_state().waker.wake();
    }
}

/// A 4-byte-aligned DMA buffer, as required by the peripheral.
#[repr(align(4))]
struct DmaBuffer<const N: usize> {
    data: [u8; N],
}

impl<const N: usize> DmaBuffer<N> {
    const fn new() -> Self {
        Self { data: [0u8; N] }
    }

    /// The peripheral's 16-bit SRAM address of this buffer.
    fn address(&self) -> u16 {
        self.data.as_ptr() as u16
    }
}

pub struct UsbPdPhy<'d, T: Instance, M: Mode> {
    _marker: PhantomData<(&'d mut T, M)>,
    cc1: vals::CcSel,
    cc2: vals::CcSel,
}

/// Read-only active-CC observation paired with a [`UsbPdPhy`].
///
/// This handle does not decide whether a level means detach. BMC traffic
/// intentionally toggles CC, and the comparator threshold is temporarily
/// changed by [`UsbPdPhy::sink_tx_ok`]. Applications must debounce observations
/// and retain an independent VBUS-present safety path.
pub struct UsbPdCcMonitor<'d, T: Instance> {
    _marker: PhantomData<&'d T>,
}

impl<T: Instance> UsbPdCcMonitor<'_, T> {
    /// Return the number of PD-port wake interrupts observed since boot.
    ///
    /// This wrapping counter is intended for diagnostics. It includes wakeups
    /// caused by pulses that ended before the waiting task could sample them.
    pub fn wake_interrupt_count(&self) -> u32 {
        T::cc_state().wake_count.load(Ordering::Relaxed)
    }

    /// Return the current output of the comparator on the selected CC pin.
    ///
    /// The normal receive threshold is 0.66 V. This method never changes the
    /// threshold and is safe to call while the PHY is receiving.
    pub fn active_cc_high(&self) -> bool {
        let active_cc = T::REGS.config().read().cc_sel();
        T::port_cc_reg(active_cc).read().pa_cc_ai()
    }

    /// Wait until the selected CC comparator reads low.
    ///
    /// This future is cancellation-safe: dropping it masks `IE_PD_IO`. A low
    /// level is only an observation, not proof of Type-C detach.
    pub async fn wait_for_active_cc_low(&mut self) {
        let mut guard = CcWakeGuard::<T>::new();

        poll_fn(|cx| {
            T::cc_state().waker.register(cx.waker());

            if !self.active_cc_high() {
                guard.disarm();
                return Poll::Ready(());
            }

            // The ISR masks the level source before waking us. If the pulse
            // ended before this task ran, arm it again and keep waiting.
            if guard.armed && !T::REGS.config().read().ie_pd_io() {
                guard.armed = false;
            }

            if !guard.armed {
                use crate::interrupt::typelevel::Interrupt;

                critical_section::with(|_| {
                    T::WakeupInterrupt::unpend();
                    T::REGS.config().modify(|w| {
                        w.set_wake_polar(false);
                        w.set_ie_pd_io(true);
                    });
                });
                guard.armed = true;

                // Close the race between the first sample and arming the wake
                // source even if a future silicon revision treats it as an edge.
                if !self.active_cc_high() {
                    guard.disarm();
                    return Poll::Ready(());
                }
            }

            Poll::Pending
        })
        .await
    }
}

struct CcWakeGuard<T: Instance> {
    armed: bool,
    _marker: PhantomData<T>,
}

impl<T: Instance> CcWakeGuard<T> {
    fn new() -> Self {
        Self {
            armed: false,
            _marker: PhantomData,
        }
    }

    fn disarm(&mut self) {
        T::REGS.config().modify(|w| w.set_ie_pd_io(false));
        self.armed = false;
    }
}

impl<T: Instance> Drop for CcWakeGuard<T> {
    fn drop(&mut self) {
        self.disarm();
    }
}

enum TxStart {
    Started(u8),
    Busy,
    HardReset,
}

impl<'d, T: Instance + PeripheralType> UsbPdPhy<'d, T, Async> {
    pub fn new_async(
        peri: Peri<'d, T>,
        cc1: Peri<'d, impl CcPin<T>>,
        cc2: Peri<'d, impl CcPin<T>>,
        _irq: impl interrupt::typelevel::Binding<T::Interrupt, InterruptHandler<T>> + 'd,
    ) -> UsbPdPhy<'d, T, Async> {
        unsafe {
            use crate::interrupt::typelevel::Interrupt;
            T::Interrupt::enable();
        };

        Self::new_inner(peri, cc1, cc2)
    }

    /// Create an asynchronous PHY and an interrupt-assisted active-CC monitor.
    ///
    /// The ordinary [`Self::new_async`] constructor remains appropriate when
    /// the application only uses VBUS-based detach detection.
    pub fn new_async_with_cc_monitor(
        peri: Peri<'d, T>,
        cc1: Peri<'d, impl CcPin<T>>,
        cc2: Peri<'d, impl CcPin<T>>,
        _irqs: impl interrupt::typelevel::Binding<T::Interrupt, InterruptHandler<T>>
            + interrupt::typelevel::Binding<T::WakeupInterrupt, CcWakeInterruptHandler<T>>
            + 'd,
    ) -> (UsbPdPhy<'d, T, Async>, UsbPdCcMonitor<'d, T>) {
        use crate::interrupt::typelevel::Interrupt;

        let phy = Self::new_inner(peri, cc1, cc2);

        T::Interrupt::unpend();
        T::WakeupInterrupt::unpend();
        unsafe {
            T::Interrupt::enable();
            T::WakeupInterrupt::enable();
        }

        (phy, UsbPdCcMonitor { _marker: PhantomData })
    }

    /// Receive the next SOP message acknowledged by the interrupt handler.
    ///
    /// Returns the SOP and number of received bytes, or an error. GoodCRC
    /// frames are delivered too, so the caller can match them against its
    /// own transmissions; they are never acknowledged. This future is
    /// cancellation-safe: a frame leaves the queue only when it is returned.
    pub async fn receive(&mut self, buf: &mut [u8]) -> Result<(Sop, usize), Error> {
        critical_section::with(|_| {
            if T::state().phase.load(Ordering::Relaxed) == PHASE_IDLE {
                arm_receive::<T>();
            }
        });

        loop {
            if let Some(result) = Self::take_received(buf) {
                #[cfg(feature = "usbpd-driver-trace")]
                emit_trace::<T>(
                    UsbPdTraceEventKind::RxComplete,
                    receive_result_code(&result),
                    T::REGS.status().read().0,
                );
                return result;
            }
            Self::wait_for_event().await;
        }
    }

    /// Transmit an ordinary SOP message.
    ///
    /// Transmission starts once any GoodCRC in progress has finished. The
    /// receiver is re-armed in the TX-end interrupt, so an immediate GoodCRC
    /// or response is captured without task involvement.
    pub async fn transmit(&mut self, buf: &[u8]) -> Result<(), Error> {
        validate_message_length(buf.len())?;
        Self::transmit_frame(Sop::Sop, buf).await
    }

    /// Transmit a hard reset.
    pub async fn transmit_hardreset(&mut self) -> Result<(), Error> {
        // Frames received before the reset are obsolete.
        T::state().ring.clear();
        Self::transmit_frame(Sop::HardReset, &[]).await
    }

    /// Return whether the attached Source currently advertises SinkTxOK.
    ///
    /// This is meaningful only after an Explicit Contract, when PD collision
    /// avoidance maps SinkTxNG to the 1.5 A Rp level and SinkTxOK to the 3 A
    /// Rp level. With a compliant external Rd, the CH32X035's 1.23 V
    /// comparator threshold lies between the two Type-C voltage ranges.
    ///
    /// Sampling briefly raises the threshold of the comparator the receiver
    /// also uses, so this returns `false` (defer) without sampling while a
    /// GoodCRC or transmission is in progress or a received frame is still
    /// queued. Callers must not poll it faster than every few milliseconds:
    /// a sample that coincides with an incoming frame corrupts that frame and
    /// the partner must retry it.
    pub fn sink_tx_ok(&self) -> bool {
        let state = T::state();
        let phase = state.phase.load(Ordering::Relaxed);
        if phase == PHASE_ACK || phase == PHASE_TX || !state.ring.is_empty() {
            return false;
        }
        self.sample_sink_tx_ok()
    }

    fn take_hard_reset() -> bool {
        let state = T::state();
        let received = state.hard_resets.load(Ordering::Acquire);
        if received == state.hard_resets_seen.load(Ordering::Relaxed) {
            return false;
        }
        state.hard_resets_seen.store(received, Ordering::Relaxed);
        // Frames that preceded the reset are obsolete.
        state.ring.clear();
        true
    }

    fn take_received(buf: &mut [u8]) -> Option<Result<(Sop, usize), Error>> {
        if Self::take_hard_reset() {
            return Some(Err(Error::HardReset));
        }
        Some(match T::state().ring.pop(buf)? {
            Ok(length) => Ok((Sop::Sop, length)),
            Err(required) => Err(Error::ReceiveBufferTooSmall {
                required,
                available: buf.len(),
            }),
        })
    }

    /// Wait for the next interrupt, a queued frame, or a Hard Reset.
    ///
    /// While a GoodCRC or transmission is in flight this also bounds the wait
    /// and recovers the receiver if the transfer never signals TX-end.
    async fn wait_for_event() {
        let state = T::state();
        let seen = state.events.load(Ordering::Acquire);
        let phase = state.phase.load(Ordering::Relaxed);
        let event = poll_fn(|cx| {
            state.waker.register(cx.waker());
            if state.events.load(Ordering::Acquire) != seen
                || !state.ring.is_empty()
                || state.hard_resets.load(Ordering::Acquire) != state.hard_resets_seen.load(Ordering::Relaxed)
            {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });

        #[cfg(feature = "embassy")]
        if phase == PHASE_ACK || phase == PHASE_TX {
            let timeout = embassy_time::Duration::from_millis(TRANSFER_TIMEOUT_MS);
            if embassy_time::with_timeout(timeout, event).await.is_err() {
                critical_section::with(|_| {
                    let phase = state.phase.load(Ordering::Relaxed);
                    if state.events.load(Ordering::Relaxed) == seen && (phase == PHASE_ACK || phase == PHASE_TX) {
                        Self::abort_transfer(phase);
                    }
                });
            }
            return;
        }
        #[cfg(not(feature = "embassy"))]
        let _ = phase;

        event.await
    }

    /// Stop a transfer that never reached TX-end and resume receiving. Call
    /// only inside a critical section.
    fn abort_transfer(phase: u8) {
        T::REGS.control().modify(|w| w.set_bmc_start(false));
        release_cc::<T>();
        if phase == PHASE_TX {
            finish_transmit(T::state(), false);
        }
        arm_receive::<T>();
    }

    /// Start a transmission unless the line is busy. Call only inside a
    /// critical section.
    fn try_start_transmit(sop: Sop, buf: &[u8]) -> TxStart {
        let state = T::state();
        if Self::take_hard_reset() {
            return TxStart::HardReset;
        }
        let phase = state.phase.load(Ordering::Relaxed);
        if phase == PHASE_ACK || phase == PHASE_TX {
            return TxStart::Busy;
        }
        // A completed frame whose interrupt is still pending must be queued
        // and acknowledged before the line is used.
        if phase == PHASE_RX && T::REGS.status().read().if_rx_act() {
            return TxStart::Busy;
        }

        let address = if buf.is_empty() {
            0
        } else {
            // SAFETY: the TX buffer is only written here, inside a critical
            // section, while no task transmission is in flight.
            let tx = unsafe { &mut *state.tx.get() };
            tx.data[..buf.len()].copy_from_slice(buf);
            tx.address()
        };
        state.tx_hard_reset.store(sop == Sop::HardReset, Ordering::Relaxed);
        state.phase.store(PHASE_TX, Ordering::Relaxed);
        let ticket = state.tx_done.load(Ordering::Relaxed).wrapping_add(1);
        start_transmit::<T>(sop, address, buf.len());
        TxStart::Started(ticket)
    }

    async fn transmit_frame(sop: Sop, buf: &[u8]) -> Result<(), Error> {
        let state = T::state();
        // Registering before each attempt means a handler that frees the
        // line after a busy check always wakes this future again.
        let start = poll_fn(|cx| {
            state.waker.register(cx.waker());
            match critical_section::with(|_| Self::try_start_transmit(sop, buf)) {
                TxStart::Started(ticket) => Poll::Ready(Ok(ticket)),
                TxStart::HardReset => Poll::Ready(Err(Error::HardReset)),
                TxStart::Busy => Poll::Pending,
            }
        });

        #[cfg(feature = "embassy")]
        let ticket = {
            let timeout = embassy_time::Duration::from_millis(TRANSFER_TIMEOUT_MS);
            match embassy_time::with_timeout(timeout, start).await {
                Ok(started) => started?,
                Err(_) => {
                    // A GoodCRC or an abandoned transmission never reached
                    // TX-end; recover the receiver and let the protocol
                    // layer retry.
                    critical_section::with(|_| {
                        let phase = state.phase.load(Ordering::Relaxed);
                        if phase == PHASE_ACK || phase == PHASE_TX {
                            Self::abort_transfer(phase);
                        }
                    });
                    return Err(Error::Timeout);
                }
            }
        };
        #[cfg(not(feature = "embassy"))]
        let ticket = start.await?;

        let done = poll_fn(|cx| {
            state.waker.register(cx.waker());
            if state.tx_done.load(Ordering::Acquire) == ticket {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });

        #[cfg(feature = "embassy")]
        {
            let timeout = embassy_time::Duration::from_millis(TRANSFER_TIMEOUT_MS);
            if embassy_time::with_timeout(timeout, done).await.is_err() {
                let aborted = critical_section::with(|_| {
                    let stuck = state.tx_done.load(Ordering::Relaxed) != ticket
                        && state.phase.load(Ordering::Relaxed) == PHASE_TX;
                    if stuck {
                        Self::abort_transfer(PHASE_TX);
                    }
                    stuck
                });
                if aborted {
                    return Err(Error::Timeout);
                }
            }
        }
        #[cfg(not(feature = "embassy"))]
        done.await;

        if state.tx_success.load(Ordering::Relaxed) {
            Ok(())
        } else if Self::take_hard_reset() {
            Err(Error::HardReset)
        } else {
            Err(Error::BufferError)
        }
    }
}

impl<'d, T: Instance + PeripheralType> UsbPdPhy<'d, T, Blocking> {
    pub fn new_blocking(
        peri: Peri<'d, T>,
        cc1: Peri<'d, impl CcPin<T>>,
        cc2: Peri<'d, impl CcPin<T>>,
    ) -> UsbPdPhy<'d, T, Blocking> {
        Self::new_inner(peri, cc1, cc2)
    }

    pub fn receive(&mut self, buf: &mut [u8]) -> Result<(Sop, usize), Error> {
        unsafe {
            qingke::pfic::disable_interrupt(interrupt::USBPD.number() as _);
        }
        // SAFETY: the interrupt handler is disabled; blocking mode owns the
        // staging buffer.
        let rx = unsafe { &*T::state().rx.get() };
        prepare_receive::<T, false>(rx.address());

        let outcome = loop {
            let status = T::REGS.status().read();
            if status.buf_err() {
                break Err(Error::BufferError);
            }
            if status.if_rx_act() {
                break Ok(());
            }
            if status.if_rx_reset() {
                break Err(Error::HardReset);
            }
            core::hint::spin_loop();
        };

        unsafe {
            qingke::pfic::enable_interrupt(interrupt::USBPD.number() as _);
        }
        outcome.and_then(|()| self.post_receive(buf))
    }

    pub fn transmit(&mut self, buf: &[u8]) -> Result<(), Error> {
        validate_message_length(buf.len())?;
        unsafe {
            qingke::pfic::disable_interrupt(interrupt::USBPD.number() as _);
        }
        // SAFETY: the interrupt handler is disabled; blocking mode owns the
        // TX buffer.
        let tx = unsafe { &mut *T::state().tx.get() };
        tx.data[..buf.len()].copy_from_slice(buf);
        start_transmit::<T>(Sop::Sop, tx.address(), buf.len());

        let result = loop {
            let status = T::REGS.status().read();
            if status.buf_err() {
                break Err(Error::BufferError);
            }
            if status.if_tx_end() {
                break Ok(());
            }
            core::hint::spin_loop();
        };

        release_cc::<T>();
        T::REGS.config().modify(|w| w.set_ie_tx_end(false));

        unsafe {
            qingke::pfic::enable_interrupt(interrupt::USBPD.number() as _);
        }

        result
    }

    /// Decodes a message received in blocking mode and returns a tuple
    /// (Sop, length) or an error.
    fn post_receive(&self, buf: &mut [u8]) -> Result<(Sop, usize), Error> {
        if T::REGS.status().read().if_rx_reset() {
            return Err(Error::HardReset);
        }
        let dma_byte_count = T::REGS.bmc_byte_cnt().read().bmc_byte_cnt() as usize;
        if !(4..=RX_DMA_BYTES).contains(&dma_byte_count) {
            return Err(Error::BufferError);
        }
        let byte_count = dma_byte_count - 4; // Strip the four-byte CRC written by the peripheral.
        if byte_count > buf.len() {
            return Err(Error::ReceiveBufferTooSmall {
                required: byte_count,
                available: buf.len(),
            });
        }
        // SAFETY: DMA reception completed before this method is called.
        let received = unsafe { &*T::state().rx.get() };
        buf[..byte_count].copy_from_slice(&received.data[..byte_count]);
        match T::REGS.status().read().bmc_aux() {
            vals::BmcAux::SOP0 => Ok((Sop::Sop, byte_count)),
            vals::BmcAux::SOP1 => Ok((Sop::SopPrime, byte_count)),
            vals::BmcAux::SOP2 => Ok((Sop::SopDoublePrime, byte_count)),
            _ => Err(Error::Rejected),
        }
    }
}

impl<'d, T: Instance + PeripheralType, M: Mode> UsbPdPhy<'d, T, M> {
    /// Create a new USB-PD driver.
    fn new_inner(_peri: Peri<'d, T>, cc1: Peri<'d, impl CcPin<T>>, cc2: Peri<'d, impl CcPin<T>>) -> Self {
        assert!(cc1.port_sel() != cc2.port_sel(), "CC1 and CC2 should be different");

        #[allow(unused)]
        let afio = crate::pac::AFIO;

        T::enable_and_reset();
        Self::forget_transfers();

        cc1.set_as_input(Pull::None);
        cc2.set_as_input(Pull::None);

        // PD 引脚 PC14/PC15 高阈值输入模式
        // PD 收发器 PHY 上拉限幅配置位: USBPD_PHY_V33
        #[cfg(ch32x0)]
        afio.ctlr().modify(|w| {
            w.set_usbpd_in_hvt(true);
            w.set_usbpd_phy_v33(true);
        });
        #[cfg(ch32l1)]
        afio.cr().modify(|w| {
            // PD pin PB6/PD7 High threshold input mode.
            w.set_usbpd_in_hvt(true);
        });

        T::REGS.config().write(|w| {
            w.set_pd_dma_en(true);
            //    w.set_pd_filt_en(true);
            //  w.set_pd_rst_en(true);
        });
        T::REGS.status().write(|w| {
            w.set_if_tx_end(true);
            w.set_if_rx_reset(true);
            w.set_if_rx_act(true);
            w.set_if_rx_byte(true);
            w.set_if_rx_bit(true);
            w.set_buf_err(true);
        });

        // pd_phy_reset

        T::port_cc_reg(cc1.port_sel()).write(|w| w.set_cc_ce(vals::PortCcCe::V0_66));
        T::port_cc_reg(cc2.port_sel()).write(|w| w.set_cc_ce(vals::PortCcCe::V0_66));

        let this = Self {
            _marker: PhantomData,
            cc1: cc1.port_sel(),
            cc2: cc2.port_sel(),
        };

        this
    }

    /// Discard queued frames, pending Hard Reset notifications, and the
    /// receiver phase after a peripheral reset.
    fn forget_transfers() {
        critical_section::with(|_| {
            let state = T::state();
            state.phase.store(PHASE_IDLE, Ordering::Relaxed);
            state
                .hard_resets_seen
                .store(state.hard_resets.load(Ordering::Relaxed), Ordering::Relaxed);
            state.ring.clear();
        });
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        critical_section::with(|_| {
            T::enable_and_reset();
            Self::forget_transfers();
        });

        T::REGS.config().write(|w| {
            w.set_pd_dma_en(true);
            //    w.set_pd_filt_en(true);
        });
        T::REGS.status().write(|w| {
            w.set_if_tx_end(true);
            w.set_if_rx_reset(true);
            w.set_if_rx_act(true);
            w.set_if_rx_byte(true);
            w.set_if_rx_bit(true);
            w.set_buf_err(true);
        });

        T::port_cc_reg(self.cc1).modify(|w| w.set_cc_lve(false));
        T::port_cc_reg(self.cc2).modify(|w| w.set_cc_lve(false));

        self.detect_cc()?;

        // pd_phy_reset
        T::port_cc_reg(self.cc1).write(|w| w.set_cc_ce(vals::PortCcCe::V0_66));
        T::port_cc_reg(self.cc2).write(|w| w.set_cc_ce(vals::PortCcCe::V0_66));

        Ok(())
    }

    pub fn detect_cc(&self) -> Result<(), Error> {
        // CH32X035 has no internal CC pull down support
        // The detection voltage is 0.22V, sufficient to detect the default power(500mA/900mA)

        T::port_cc_reg(self.cc1).modify(|w| w.set_cc_ce(vals::PortCcCe::V0_22));
        crate::delay::Delay.delay_us(2);

        if T::port_cc_reg(self.cc1).read().pa_cc_ai() {
            // CC1 is connected
            T::REGS.config().modify(|w| w.set_cc_sel(vals::CcSel::CC1));
            Ok(())
        } else {
            T::port_cc_reg(self.cc2).modify(|w| w.set_cc_ce(vals::PortCcCe::V0_22));
            crate::delay::Delay.delay_us(2);

            if T::port_cc_reg(self.cc2).read().pa_cc_ai() {
                // CC2 is connected
                T::REGS.config().modify(|w| w.set_cc_sel(vals::CcSel::CC2));
                Ok(())
            } else {
                Err(Error::CCNotConnected)
            }
        }
    }

    /// Sample the active CC at the 1.23 V threshold and restore the normal
    /// 0.66 V receive threshold.
    fn sample_sink_tx_ok(&self) -> bool {
        let active_cc = T::REGS.config().read().cc_sel();
        let cc = T::port_cc_reg(active_cc);
        cc.modify(|w| w.set_cc_ce(vals::PortCcCe::V1_23));
        // Busy-wait so a preempting interrupt cannot share SysTick with this
        // settling delay.
        qingke::riscv::asm::delay(2 * hclk_mhz());
        let allowed = cc.read().pa_cc_ai();
        cc.modify(|w| w.set_cc_ce(vals::PortCcCe::V0_66));
        allowed
    }
}

fn validate_message_length(length: usize) -> Result<(), Error> {
    if length > MAX_MESSAGE_BYTES {
        Err(Error::MessageTooLong {
            length,
            maximum: MAX_MESSAGE_BYTES,
        })
    } else {
        Ok(())
    }
}

/// Shared state of the singleton USBPD peripheral.
///
/// Each atomic has one writer: the interrupt handler, or task code running
/// inside a critical section (which the handler cannot preempt). `ring`
/// follows its own single-producer/single-consumer contract.
struct State {
    waker: AtomicWaker,
    /// One of the `PHASE_*` values.
    phase: AtomicU8,
    /// Wrapping count of USBPD interrupts, used to detect stalled transfers.
    events: AtomicU8,
    /// Wrapping count of partner Hard Resets observed by the handler.
    hard_resets: AtomicU8,
    /// `hard_resets` value already reported to the task.
    hard_resets_seen: AtomicU8,
    /// Wrapping count of completed task transmissions.
    tx_done: AtomicU8,
    /// Whether the last completed task transmission reached TX-end.
    tx_success: AtomicBool,
    /// Whether the task transmission in flight is Hard Reset signaling.
    tx_hard_reset: AtomicBool,
    ring: RxRing<RX_QUEUE_FRAMES>,
    rx: UnsafeCell<DmaBuffer<RX_DMA_BYTES>>,
    tx: UnsafeCell<DmaBuffer<RX_DMA_BYTES>>,
    ack: UnsafeCell<DmaBuffer<4>>,
}

// SAFETY: the DMA buffers are accessed only by the phase owner documented at
// each access, and all other fields are atomics or internally synchronized.
unsafe impl Sync for State {}

impl State {
    pub const fn new() -> Self {
        Self {
            waker: AtomicWaker::new(),
            phase: AtomicU8::new(PHASE_IDLE),
            events: AtomicU8::new(0),
            hard_resets: AtomicU8::new(0),
            hard_resets_seen: AtomicU8::new(0),
            tx_done: AtomicU8::new(0),
            tx_success: AtomicBool::new(false),
            tx_hard_reset: AtomicBool::new(false),
            ring: RxRing::new(),
            rx: UnsafeCell::new(DmaBuffer::new()),
            tx: UnsafeCell::new(DmaBuffer::new()),
            ack: UnsafeCell::new(DmaBuffer::new()),
        }
    }
}

struct CcState {
    waker: AtomicWaker,
    wake_count: AtomicU32,
}

impl CcState {
    const fn new() -> Self {
        Self {
            waker: AtomicWaker::new(),
            wake_count: AtomicU32::new(0),
        }
    }
}

trait SealedInstance {
    const REGS: crate::pac::usbpd::Usbpd;

    fn state() -> &'static State;
    fn cc_state() -> &'static CcState;
}

#[allow(private_bounds)]
pub trait Instance: SealedInstance + RccPeripheral {
    type Interrupt: crate::interrupt::typelevel::Interrupt;
    type WakeupInterrupt: crate::interrupt::typelevel::Interrupt;

    #[allow(dead_code)]
    fn port_cc_reg(cc: vals::CcSel) -> pac::common::Reg<pac::usbpd::regs::PortCc, pac::common::RW> {
        match cc {
            vals::CcSel::CC1 => Self::REGS.port_cc(0),
            vals::CcSel::CC2 => Self::REGS.port_cc(2),
            #[cfg(ch641)]
            vals::CcSel::CC3 => Self::REGS.port_cc(3),
            #[allow(unreachable_patterns)]
            _ => panic!("Invalid CC"),
        }
    }
}

// catch GLOBAL irq
foreach_interrupt!(
    ($inst:ident, usbpd, USBPD, GLOBAL, $irq:ident) => {
        impl SealedInstance for crate::peripherals::$inst {
            const REGS: crate::pac::usbpd::Usbpd = crate::pac::$inst;

            fn state() -> &'static State {
                static STATE: State = State::new();
                &STATE
            }

            fn cc_state() -> &'static CcState {
                static STATE: CcState = CcState::new();
                &STATE
            }
        }

        impl Instance for crate::peripherals::$inst {
            type Interrupt = crate::interrupt::typelevel::$irq;
            type WakeupInterrupt = crate::_generated::peripheral_interrupts::$inst::WKUP;
        }
    };
);

pub trait CcPin<T: Instance>: crate::gpio::Pin {
    fn port_sel(&self) -> pac::usbpd::vals::CcSel;
}

#[cfg(ch32x0)]
mod _cc_pin_ch32x0 {
    use super::*;

    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PC14 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC1
        }
    }
    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PC15 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC2
        }
    }
}

#[cfg(ch32l1)]
mod _cc_pin_ch32l1 {
    use super::*;

    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PB6 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC1
        }
    }
    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PB7 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC2
        }
    }
}

#[cfg(ch641)]
mod _cc_pin_ch641 {
    use super::*;

    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PB0 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC1
        }
    }
    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PB1 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC2
        }
    }
    impl CcPin<crate::peripherals::USBPD> for crate::peripherals::PB9 {
        #[inline(always)]
        fn port_sel(&self) -> pac::usbpd::vals::CcSel {
            pac::usbpd::vals::CcSel::CC3
        }
    }
}

#[inline]
fn calc_bmc_clk_for_tx() -> u16 {
    (crate::rcc::clocks().hclk.0 / 1_000_000 * 80 / 48 - 1) as u16
}

#[inline]
fn calc_bmc_clk_for_rx() -> u16 {
    (crate::rcc::clocks().hclk.0 / 1_000_000 * 120 / 48 - 1) as u16
}
