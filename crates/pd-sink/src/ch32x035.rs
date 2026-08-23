//! CH32X035 USB-PD PHY adapter.
//!
//! The library owns the PHY error mapping and detach cancellation, while the
//! application supplies [`Ch32x035Port`] using whatever VBUS-present input,
//! load gate, executor primitives, and diagnostics its board uses. No GPIO is
//! assigned by this module.

use core::{future::Future, marker::PhantomData};

use ch32_hal as hal;
use embassy_futures::select::{select, Either};
use hal::usbpd::{Error, Sop, UsbPdPhy};
use hal::{mode, peripherals};
use usbpd::sink::policy_engine::Sink;
use usbpd::timers::Timer as StackTimer;
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

use crate::session::{classify_sink_error, SinkSessionRetry, RESET_RETRY_MS};
use crate::{
    RecoveryInitError, RecoveryIntent, SinkConfig, SinkConfigError, SinkDevice, SinkRuntime, SinkSessionEvent,
    SinkSessionTerminalError,
};

/// Low-level observations useful for diagnostics but irrelevant to policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhyEvent {
    Attached,
    SinkTxAllowed,
    SinkTxDeferred,
}

/// Board-owned port services needed by [`Ch32x035UsbPdDriver`].
///
/// The three VBUS methods expose one active-high, board-defined physical
/// detector predicate. `true` means the detector is available and reports
/// VBUS above the board's minimum-valid threshold. `false` means VBUS is below
/// that threshold or the detector is unavailable. This is only a coarse
/// minimum-VBUS predicate; it does not measure VBUS or prove that VBUS matches
/// the negotiated contract.
///
/// The application must initialize the published predicate to false, qualify
/// a raw high continuously for its documented assertion interval, and publish
/// a raw low or unavailable detector immediately, without detach debounce.
/// Both wait methods must be cancellation-safe. A simple implementation can
/// use Embassy signals fed by a separate GPIO or comparator supervisor task.
/// Board documentation must state detector polarity, nominal rising and
/// falling thresholds, worst-case threshold tolerance, hysteresis, assertion
/// qualification, and maximum deassertion-to-load-off latency. The detector
/// and physical load gate must fail safe/off during reset, loss of either
/// supply, or uncertain detector state.
pub trait Ch32x035Port {
    fn vbus_present(&self) -> bool;
    fn wait_for_vbus_present(&self) -> impl Future<Output = ()>;
    fn wait_for_vbus_absent(&self) -> impl Future<Output = ()>;
    /// Clear any stale detach notification and report the start of a fresh
    /// session. The physical VBUS level has already been checked as present.
    fn begin_session(&self);
    /// This is the PD policy's permission for the application load. Hardware
    /// should still combine it with the user latch, VBUS-present, and health.
    fn set_pd_load_permitted(&self, permitted: bool);
    fn observe_phy(&self, event: PhyEvent);
    /// Observe managed-session lifecycle without owning retry policy. This is
    /// diagnostic only; safety actions never wait for this hook.
    fn observe_session(&self, _event: SinkSessionEvent) {}
}

/// Timer services required by [`Ch32x035SinkSession`].
///
/// This deliberately hides the maintained protocol stack's timer trait from
/// board applications.
pub trait Ch32x035SessionTimer {
    fn now_128ms_ticks() -> u32;
    fn after_millis(milliseconds: u64) -> impl Future<Output = ()>;
}

struct StackTimerAdapter<T>(PhantomData<T>);

impl<T: Ch32x035SessionTimer> StackTimer for StackTimerAdapter<T> {
    fn now_128ms_ticks() -> u32 {
        T::now_128ms_ticks()
    }

    async fn after_millis(milliseconds: u64) {
        T::after_millis(milliseconds).await;
    }
}

/// `usbpd_traits::Driver` implementation for the CH32X035 integrated PHY.
pub struct Ch32x035UsbPdDriver<'d, P: Ch32x035Port> {
    usbpd: UsbPdPhy<'d, peripherals::USBPD, mode::Async>,
    port: P,
    last_sink_tx_ok: Option<bool>,
}

impl<'d, P: Ch32x035Port> Ch32x035UsbPdDriver<'d, P> {
    pub fn new(usbpd: UsbPdPhy<'d, peripherals::USBPD, mode::Async>, port: P) -> Self {
        Self { usbpd, port, last_sink_tx_ok: None }
    }

    /// Reset the integrated PHY before starting or restarting the policy
    /// engine. `CCNotConnected` is expected while no source is attached.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.last_sink_tx_ok = None;
        self.usbpd.reset()
    }

    fn inhibit_load(&self) {
        self.port.set_pd_load_permitted(false);
    }

    fn observe_session(&self, event: SinkSessionEvent) {
        self.port.observe_session(event);
    }

    async fn wait_for_detach_or_timeout<T: Ch32x035SessionTimer>(&self, milliseconds: u32) {
        if self.port.vbus_present() {
            let _ = select(self.port.wait_for_vbus_absent(), T::after_millis(u64::from(milliseconds))).await;
        }
    }

    fn detached_rx(&self) -> DriverRxError {
        self.port.set_pd_load_permitted(false);
        DriverRxError::Detached
    }

    fn detached_tx(&self) -> DriverTxError {
        self.port.set_pd_load_permitted(false);
        DriverTxError::Detached
    }
}

/// Managed CH32X035 USB-PD sink lifecycle.
///
/// The session owns PHY reset, policy-engine construction, bounded recovery,
/// and classification of terminal local failures. Board code still owns
/// peripheral setup, physical safety inputs, the load gate, commands, and
/// product policy through [`Ch32x035Port`] and [`SinkRuntime`].
pub struct Ch32x035SinkSession<'d, P, R, T>
where
    P: Ch32x035Port,
    R: SinkRuntime,
    T: Ch32x035SessionTimer,
{
    sink: Sink<Ch32x035UsbPdDriver<'d, P>, StackTimerAdapter<T>, SinkDevice<R>>,
    first_run: bool,
    startup_recovery_armed: bool,
}

impl<'d, P, R, T> Ch32x035SinkSession<'d, P, R, T>
where
    P: Ch32x035Port,
    R: SinkRuntime,
    T: Ch32x035SessionTimer,
{
    /// Construct a conservative fresh-attachment session.
    pub fn new(
        usbpd: UsbPdPhy<'d, peripherals::USBPD, mode::Async>,
        port: P,
        config: SinkConfig,
        runtime: R,
    ) -> Result<Self, SinkConfigError> {
        let device = SinkDevice::new(config, runtime)?;
        Ok(Self::from_device(usbpd, port, device, false))
    }

    /// Construct a session that attempts caller-authorized warm recovery.
    ///
    /// Unlike later physical reattachments, the first policy-engine run keeps
    /// the DPM's Soft Reset startup state intact.
    pub fn new_recovering(
        usbpd: UsbPdPhy<'d, peripherals::USBPD, mode::Async>,
        port: P,
        config: SinkConfig,
        runtime: R,
        intent: RecoveryIntent,
    ) -> Result<Self, RecoveryInitError> {
        let device = SinkDevice::new_recovering(config, runtime, intent)?;
        Ok(Self::from_device(usbpd, port, device, true))
    }

    fn from_device(
        usbpd: UsbPdPhy<'d, peripherals::USBPD, mode::Async>,
        port: P,
        device: SinkDevice<R>,
        startup_recovery_armed: bool,
    ) -> Self {
        let driver = Ch32x035UsbPdDriver::new(usbpd, port);
        Self { sink: Sink::new(driver, device), first_run: true, startup_recovery_armed }
    }

    /// Run continuously through ordinary detach, PHY, and protocol recovery.
    ///
    /// This returns only if the policy engine unexpectedly stops or a
    /// deterministic local construction error makes an unchanged retry
    /// invalid. The load has been inhibited before the terminal event.
    pub async fn run(&mut self) -> SinkSessionTerminalError {
        loop {
            loop {
                let driver = self.sink.driver_mut();
                driver.inhibit_load();
                let reset = driver.reset();
                match reset {
                    Ok(()) => {
                        self.startup_recovery_armed = false;
                        break;
                    }
                    Err(Error::CCNotConnected) => {
                        if self.startup_recovery_armed {
                            self.sink.restart_unstarted_after_detach();
                            self.startup_recovery_armed = false;
                        }
                    }
                    Err(_) => {
                        if self.startup_recovery_armed {
                            self.sink.restart_unstarted_after_protocol_loss();
                            self.startup_recovery_armed = false;
                        }
                        self.sink
                            .driver_mut()
                            .observe_session(SinkSessionEvent::PhyResetFailed { retry_ms: RESET_RETRY_MS });
                    }
                }
                T::after_millis(u64::from(RESET_RETRY_MS)).await;
            }

            if self.first_run {
                self.first_run = false;
            } else {
                self.sink.restart();
            }
            self.sink.driver_mut().observe_session(SinkSessionEvent::CcDetected);

            let result = self.sink.run().await;
            let retry = match result {
                Ok(()) => {
                    return self.finish(SinkSessionTerminalError::UnexpectedStop);
                }
                Err(error) => match classify_sink_error(error) {
                    Ok(retry) => retry,
                    Err(error) => return self.finish(error),
                },
            };
            self.recover(retry).await;
        }
    }

    fn finish(&mut self, error: SinkSessionTerminalError) -> SinkSessionTerminalError {
        let driver = self.sink.driver_mut();
        driver.inhibit_load();
        driver.observe_session(SinkSessionEvent::Terminal(error));
        error
    }

    async fn recover(&mut self, retry: SinkSessionRetry) {
        let driver = self.sink.driver_mut();
        driver.inhibit_load();
        driver.observe_session(SinkSessionEvent::Recovering {
            reason: retry.reason,
            retry_ms: retry.delay_ms,
            wait_for_detach: retry.wait_for_detach,
        });
        if retry.wait_for_detach {
            self.sink.driver_mut().wait_for_detach_or_timeout::<T>(retry.delay_ms).await;
        } else {
            T::after_millis(u64::from(retry.delay_ms)).await;
        }
    }
}

impl<P: Ch32x035Port> Driver for Ch32x035UsbPdDriver<'_, P> {
    async fn wait_for_vbus(&mut self) {
        while !self.port.vbus_present() {
            self.port.wait_for_vbus_present().await;
        }
        self.port.begin_session();
        self.port.observe_phy(PhyEvent::Attached);
    }

    fn sink_tx_ok(&mut self) -> bool {
        let sink_tx_ok = self.usbpd.sink_tx_ok();
        if self.last_sink_tx_ok != Some(sink_tx_ok) {
            self.port.observe_phy(if sink_tx_ok { PhyEvent::SinkTxAllowed } else { PhyEvent::SinkTxDeferred });
            self.last_sink_tx_ok = Some(sink_tx_ok);
        }
        sink_tx_ok
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        if !self.port.vbus_present() {
            return Err(self.detached_rx());
        }

        let received = match select(self.usbpd.receive(buffer), self.port.wait_for_vbus_absent()).await {
            Either::First(received) => received,
            Either::Second(()) => return Err(self.detached_rx()),
        };

        if !self.port.vbus_present() {
            return Err(self.detached_rx());
        }

        match received {
            Ok((Sop::Sop, size)) => Ok(size),
            Ok(_) => Err(DriverRxError::Discarded),
            Err(Error::HardReset) => {
                self.port.set_pd_load_permitted(false);
                Err(DriverRxError::HardReset)
            }
            Err(_) => Err(DriverRxError::Discarded),
        }
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        if !self.port.vbus_present() {
            return Err(self.detached_tx());
        }

        let transmitted = match select(self.usbpd.transmit(data), self.port.wait_for_vbus_absent()).await {
            Either::First(transmitted) => transmitted,
            Either::Second(()) => return Err(self.detached_tx()),
        };

        if !self.port.vbus_present() {
            return Err(self.detached_tx());
        }

        transmitted.map_err(|error| match error {
            Error::HardReset => {
                self.port.set_pd_load_permitted(false);
                DriverTxError::HardReset
            }
            _ => DriverTxError::Discarded,
        })
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        self.port.set_pd_load_permitted(false);
        if !self.port.vbus_present() {
            return Err(DriverTxError::Detached);
        }

        match select(self.usbpd.transmit_hardreset(), self.port.wait_for_vbus_absent()).await {
            Either::First(result) => result.map_err(|error| match error {
                Error::HardReset => DriverTxError::HardReset,
                _ => DriverTxError::Discarded,
            }),
            Either::Second(()) => Err(DriverTxError::Detached),
        }
    }
}
