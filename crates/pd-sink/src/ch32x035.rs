//! CH32X035 USB-PD PHY adapter.
//!
//! The library owns the PHY error mapping and detach cancellation, while the
//! application supplies [`Ch32x035Port`] using whatever VBUS-present input,
//! load gate, executor primitives, and diagnostics its board uses. No GPIO is
//! assigned by this module.

use core::future::Future;

use ch32_hal as hal;
use embassy_futures::select::{select, Either};
use hal::usbpd::{Error, Sop, UsbPdPhy};
use hal::{mode, peripherals};
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

/// Low-level observations useful for diagnostics but irrelevant to policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhyEvent {
    Attached,
    SinkTxAllowed,
    SinkTxDeferred,
}

/// Board-owned port services needed by [`Ch32x035UsbPdDriver`].
///
/// Both wait methods must be cancellation-safe. A simple implementation can
/// use Embassy signals fed by a separate GPIO supervisor task.
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

    fn detached_rx(&self) -> DriverRxError {
        self.port.set_pd_load_permitted(false);
        DriverRxError::Detached
    }

    fn detached_tx(&self) -> DriverTxError {
        self.port.set_pd_load_permitted(false);
        DriverTxError::Detached
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
