//! USBPD, USB Power Delivery
//!
//! Design:
//!
//! - CC Pins:
//! - UsbPdPhy: USBPD PHY layer
//! - UsbPdSniffer: USBPD Sniffer based on PHY layer, no transmit support
//! - [ ] UsbPdSink: USBPD Sink layer
//! - [ ] UsbPdSource: USBPD Source layer

use core::future::poll_fn;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::Poll;

use embassy_sync::waitqueue::AtomicWaker;
use pac::InterruptNumber;

use crate::gpio::Pull;
use crate::mode::{Async, Blocking, Mode};
use crate::pac::usbpd::vals;
use crate::{interrupt, pac, Peri, PeripheralType, RccPeripheral};

#[cfg(feature = "usbpd-driver-trace")]
mod trace;
mod turnaround;

#[cfg(feature = "usbpd-driver-trace")]
pub use trace::{
    set_usbpd_trace_callback, UsbPdTraceCallback, UsbPdTraceCode, UsbPdTraceEvent, UsbPdTraceEventKind,
    USBPD_TRACE_ABI_VERSION,
};
use turnaround::{needs_rx_turnaround, TransferState};

/// Maximum PD message size excluding the four-byte CRC appended by the PHY.
pub const MAX_MESSAGE_BYTES: usize = 30;
const RX_DMA_BYTES: usize = MAX_MESSAGE_BYTES + 4;

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

#[derive(Debug, PartialEq, Eq)]
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
        let usbpd = T::REGS;

        let status = usbpd.status().read();

        if status.if_tx_end() || status.buf_err() {
            // TX has stopped (or faulted), so release the CC transmitter
            // before any ordinary-message receive turnaround is armed.
            T::port_cc_reg(vals::CcSel::CC1).modify(|w| w.set_cc_lve(false));
            T::port_cc_reg(vals::CcSel::CC2).modify(|w| w.set_cc_lve(false));
        }

        if status.if_tx_end() {
            T::REGS.config().modify(|w| w.set_ie_tx_end(false));

            // WCH's sink sequence switches directly from TX to RX. For an
            // ordinary message, do that here before waking the executor so an
            // immediate GoodCRC or Source response cannot begin in a blind
            // window. GoodCRC and Hard Reset transmissions do not request it.
            if !status.buf_err() {
                T::state().transfer.complete_transmit(|| prepare_receive::<T, true>());
                #[cfg(feature = "usbpd-driver-trace")]
                emit_rx_trace::<T>(UsbPdTraceEventKind::RxArmed, UsbPdTraceCode::TxTurnaroundArm, status.0);
            }
        }

        if status.if_rx_act() {
            T::REGS.control().modify(|w| w.set_bmc_start(false)); // stop
            T::REGS.config().modify(|w| w.set_ie_rx_act(false));
        }

        if status.if_rx_reset() {
            T::REGS.config().modify(|w| w.set_ie_rx_reset(false));
        }

        if status.buf_err() {
            // No dedicated IE bit; gated by PD_DMA_EN. Latch and drop IEs
            // so the poller observes the error and returns.
            T::state().buf_err.store(true, Ordering::Release);
            T::REGS.config().modify(|w| {
                w.set_ie_rx_act(false);
                w.set_ie_rx_reset(false);
                w.set_ie_tx_end(false);
            });
        }

        T::REGS.status().write_value(status);

        // Wake the task to clear and re-enabled interrupts.
        T::state().waker.wake();

        #[cfg(feature = "usbpd-driver-trace")]
        emit_rx_trace::<T>(UsbPdTraceEventKind::Interrupt, UsbPdTraceCode::Interrupt, status.0);
    }
}

#[cfg(feature = "usbpd-driver-trace")]
fn emit_rx_trace<T: Instance>(kind: UsbPdTraceEventKind, code: UsbPdTraceCode, status: u8) {
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

#[cfg(feature = "usbpd-driver-trace")]
struct ReceiveTraceGuard<T: Instance> {
    finished: bool,
    _marker: PhantomData<T>,
}

#[cfg(feature = "usbpd-driver-trace")]
impl<T: Instance> ReceiveTraceGuard<T> {
    fn new() -> Self {
        Self {
            finished: false,
            _marker: PhantomData,
        }
    }

    fn finish(&mut self, result: &Result<(Sop, usize), Error>) {
        emit_rx_trace::<T>(
            UsbPdTraceEventKind::RxComplete,
            receive_result_code(result),
            T::REGS.status().read().0,
        );
        self.finished = true;
    }
}

#[cfg(feature = "usbpd-driver-trace")]
impl<T: Instance> Drop for ReceiveTraceGuard<T> {
    fn drop(&mut self) {
        if !self.finished {
            emit_rx_trace::<T>(
                UsbPdTraceEventKind::RxCancelled,
                UsbPdTraceCode::Cancelled,
                T::REGS.status().read().0,
            );
        }
    }
}

/// Switch the peripheral from TX to RX using its stable singleton DMA buffer.
///
/// The asynchronous path enables receive interrupts; the blocking path polls
/// the same hardware sequence with the USBPD interrupt disabled.
fn prepare_receive<T: Instance, const ENABLE_INTERRUPTS: bool>() {
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

    // SAFETY: State owns the stable, aligned singleton buffer. The transfer
    // lifecycle serializes DMA, ISR, and task access to it.
    let buffer = unsafe { &mut *T::state().transfer.buffer_ptr() };
    usbpd.dma().write_value(buffer.mut_address());
    usbpd.control().modify(|w| w.set_pd_tx_en(false));
    usbpd.bmc_clk_cnt().modify(|w| w.set_bmc_clk_cnt(calc_bmc_clk_for_rx()));
    usbpd.control().modify(|w| w.set_bmc_start(true));
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

#[repr(align(4))]
struct UsbPdMsg {
    pub data: [u8; RX_DMA_BYTES],
}
impl UsbPdMsg {
    const fn new() -> Self {
        Self {
            data: [0u8; RX_DMA_BYTES],
        }
    }

    fn to_slice(&self) -> &[u8] {
        &self.data
    }

    fn address(&self) -> u16 {
        self.data.as_ptr() as u16
    }

    fn mut_address(&mut self) -> u16 {
        self.data.as_mut_ptr() as u16
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

    fn enable_tx_interrupt(&mut self) {
        // Clear stale BUF_ERR (HW W1C, then latch) before re-arming IEs.
        T::REGS.status().write(|w| w.set_buf_err(true));
        T::state().buf_err.store(false, Ordering::Release);
        T::REGS.config().modify(|w| {
            w.set_ie_rx_act(false); // Receive completion interrupt disable
            w.set_ie_rx_reset(false); // Receive reset interrupt disable
            w.set_ie_tx_end(true); // End-of-transmit interrupt enable
        });
    }

    /// Receives a PD message into the provided buffer.
    ///
    /// Returns the SOP and number of received bytes, or an error.
    pub async fn receive(&mut self, buf: &mut [u8]) -> Result<(Sop, usize), Error> {
        let prearmed = T::state().transfer.take_prearmed_receive();
        if !prearmed {
            // Clear stale BUF_ERR (HW W1C, then latch) before re-arming RX.
            #[cfg(feature = "usbpd-driver-trace")]
            let status_before_arm = T::REGS.status().read().0;
            T::REGS.status().write(|w| w.set_buf_err(true));
            T::state().buf_err.store(false, Ordering::Release);
            prepare_receive::<T, true>();
            #[cfg(feature = "usbpd-driver-trace")]
            emit_rx_trace::<T>(UsbPdTraceEventKind::RxArmed, UsbPdTraceCode::TaskArm, status_before_arm);
        } else {
            #[cfg(feature = "usbpd-driver-trace")]
            emit_rx_trace::<T>(
                UsbPdTraceEventKind::RxArmed,
                UsbPdTraceCode::PrearmedReceive,
                T::REGS.status().read().0,
            );
        }

        #[cfg(feature = "usbpd-driver-trace")]
        let mut trace_guard = ReceiveTraceGuard::<T>::new();
        let result = poll_fn(|cx| {
            T::state().waker.register(cx.waker());

            if T::state().buf_err.load(Ordering::Acquire) {
                return Poll::Ready(Err(Error::BufferError));
            }

            if !T::REGS.config().read().ie_rx_reset() {
                return Poll::Ready(Err(Error::HardReset));
            }

            if !T::REGS.config().read().ie_rx_act() {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        })
        .await
        .and_then(|()| self.post_receive(buf));
        #[cfg(feature = "usbpd-driver-trace")]
        trace_guard.finish(&result);
        result
    }

    pub async fn transmit(&mut self, buf: &[u8]) -> Result<(), Error> {
        validate_message_length(buf.len())?;
        T::state().transfer.begin_transmit(false);
        self.enable_tx_interrupt();
        self.transmit_inner(Sop::Sop, buf);
        let result = self.wait_for_tx_complete().await;

        if result.is_err() {
            T::state().transfer.cancel();
        }

        result
    }

    /// Transmit an ordinary PD message and arm RX in the TX-end interrupt so
    /// the following [`Self::receive`] can capture its immediate GoodCRC or
    /// Source response. An exact GoodCRC automatically skips turnaround
    /// because it is never acknowledged.
    pub async fn transmit_with_rx_turnaround(&mut self, buf: &[u8]) -> Result<(), Error> {
        validate_message_length(buf.len())?;
        T::state().transfer.begin_transmit(needs_rx_turnaround(buf));
        self.enable_tx_interrupt();
        self.transmit_inner(Sop::Sop, buf);
        let result = self.wait_for_tx_complete().await;

        if result.is_err() {
            T::state().transfer.cancel();
        }

        result
    }

    /// Transmit a hard reset.
    pub async fn transmit_hardreset(&mut self) -> Result<(), Error> {
        T::state().transfer.cancel();
        self.enable_tx_interrupt();
        self.transmit_inner(Sop::HardReset, &[]);
        let result = self.wait_for_tx_complete().await;

        result
    }

    async fn wait_for_tx_complete(&mut self) -> Result<(), Error> {
        poll_fn(|cx| {
            T::state().waker.register(cx.waker());

            if T::state().buf_err.load(Ordering::Acquire) {
                return Poll::Ready(Err(Error::BufferError));
            }

            if !T::REGS.config().read().ie_tx_end() {
                return Poll::Ready(Ok(()));
            }
            Poll::Pending
        })
        .await
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
        #[cfg(feature = "usbpd-driver-trace")]
        let status_before_arm = T::REGS.status().read().0;
        prepare_receive::<T, false>();
        #[cfg(feature = "usbpd-driver-trace")]
        emit_rx_trace::<T>(UsbPdTraceEventKind::RxArmed, UsbPdTraceCode::TaskArm, status_before_arm);

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
        let result = outcome.and_then(|()| self.post_receive(buf));
        #[cfg(feature = "usbpd-driver-trace")]
        emit_rx_trace::<T>(
            UsbPdTraceEventKind::RxComplete,
            receive_result_code(&result),
            T::REGS.status().read().0,
        );
        result
    }

    pub fn transmit(&mut self, buf: &[u8]) -> Result<(), Error> {
        validate_message_length(buf.len())?;
        unsafe {
            qingke::pfic::disable_interrupt(interrupt::USBPD.number() as _);
        }
        self.transmit_inner(Sop::Sop, buf);

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

        T::port_cc_reg(vals::CcSel::CC1).modify(|w| w.set_cc_lve(false));
        T::port_cc_reg(vals::CcSel::CC2).modify(|w| w.set_cc_lve(false));

        unsafe {
            qingke::pfic::enable_interrupt(interrupt::USBPD.number() as _);
        }

        result
    }
}

impl<'d, T: Instance + PeripheralType, M: Mode> UsbPdPhy<'d, T, M> {
    /// Create a new USB-PD driver.
    fn new_inner(_peri: Peri<'d, T>, cc1: Peri<'d, impl CcPin<T>>, cc2: Peri<'d, impl CcPin<T>>) -> Self {
        assert!(cc1.port_sel() != cc2.port_sel(), "CC1 and CC2 should be different");

        #[allow(unused)]
        let afio = crate::pac::AFIO;

        T::enable_and_reset();

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

    pub fn reset(&mut self) -> Result<(), Error> {
        T::state().transfer.cancel();
        T::enable_and_reset();

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

    /// Return whether the attached Source currently advertises SinkTxOK.
    ///
    /// This is meaningful only after an Explicit Contract, when PD collision
    /// avoidance maps SinkTxNG to the 1.5 A Rp level and SinkTxOK to the 3 A
    /// Rp level. With a compliant external Rd, the CH32X035's 1.23 V
    /// comparator threshold lies between the two Type-C voltage ranges. The
    /// normal 0.66 V receive threshold is restored after sampling.
    pub fn sink_tx_ok(&self) -> bool {
        let active_cc = T::REGS.config().read().cc_sel();
        let cc = T::port_cc_reg(active_cc);
        cc.modify(|w| w.set_cc_ce(vals::PortCcCe::V1_23));
        crate::delay::Delay.delay_us(2);
        let allowed = cc.read().pa_cc_ai();
        cc.modify(|w| w.set_cc_ce(vals::PortCcCe::V0_66));
        allowed
    }

    /// Decodes the received PD message and returns a tuple (Sop, length) or an error.
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
        let received = unsafe { &*T::state().transfer.buffer_ptr() };
        buf[..byte_count].copy_from_slice(&received.to_slice()[..byte_count]);
        match T::REGS.status().read().bmc_aux() {
            vals::BmcAux::SOP0 => Ok((Sop::Sop, byte_count)),
            vals::BmcAux::SOP1 => Ok((Sop::SopPrime, byte_count)),
            vals::BmcAux::SOP2 => Ok((Sop::SopDoublePrime, byte_count)),
            _ => Err(Error::Rejected),
        }
    }

    fn transmit_inner(&mut self, sop: Sop, buf: &[u8]) {
        debug_assert!(buf.len() <= MAX_MESSAGE_BYTES);
        T::port_cc_reg(T::REGS.config().read().cc_sel()).modify(|w| w.set_cc_lve(true));

        T::REGS
            .bmc_clk_cnt()
            .write(|w| w.set_bmc_clk_cnt(calc_bmc_clk_for_tx()));

        if buf.is_empty() {
            T::REGS.dma().write_value(0);
        } else {
            // We use our own buffer to ensure it is 4-byte aligned, as required by the hardware.
            // SAFETY: the singleton peripheral serializes all transfers.
            let transmit = unsafe { &mut *T::state().transfer.buffer_ptr() };
            transmit.data[..buf.len()].copy_from_slice(buf);
            T::REGS.dma().write_value(transmit.address());
        }

        T::REGS.tx_sel().write(|w| w.0 = sop as u8);

        T::REGS.bmc_tx_sz().write(|w| w.set_bmc_tx_sz(buf.len() as _));
        T::REGS.control().modify(|w| w.set_pd_tx_en(true)); // TX

        T::REGS.status().write(|w| {
            w.set_if_tx_end(true);
            w.set_if_rx_reset(true);
            w.set_if_rx_act(true);
            w.set_if_rx_byte(true);
            w.set_if_rx_bit(true);
            w.set_buf_err(true);
        });

        T::REGS.control().modify(|w| w.set_bmc_start(true));
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

struct State {
    waker: AtomicWaker,
    // Set by the ISR on BUF_ERR; cleared at the start of each transfer.
    buf_err: AtomicBool,
    transfer: TransferState<UsbPdMsg>,
}

impl State {
    pub const fn new() -> Self {
        Self {
            waker: AtomicWaker::new(),
            buf_err: AtomicBool::new(false),
            transfer: TransferState::new(UsbPdMsg::new()),
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
