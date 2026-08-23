//! CH32X035 USBFS device support.
//!
//! The product currently uses only the compact, fixed-layout CDC-ACM device
//! in [`cdc`]. Keeping this module specific to the required endpoints avoids
//! carrying an unverified generic USB allocation layer into first hardware
//! bring-up.

pub mod cdc;
mod connection;

const MAX_NR_EP: usize = 4;

pub(crate) const RESPONSE_ACK: u8 = 0;
pub(crate) const RESPONSE_NAK: u8 = 2;
pub(crate) const RESPONSE_STALL: u8 = 3;

pub(crate) const TOKEN_OUT: u8 = 0;
pub(crate) const TOKEN_IN: u8 = 2;
pub(crate) const TOKEN_SETUP: u8 = 3;

#[inline]
pub(crate) fn regs() -> crate::pac::usb::Usbd {
    unsafe { crate::pac::usb::Usbd::from_ptr(crate::pac::USBFS.as_ptr()) }
}

#[inline]
pub(crate) fn endpoint_ctrl(
    ep: usize,
) -> crate::pac::common::Reg<crate::pac::usb::regs::UepCtrl, crate::pac::common::RW> {
    assert!(ep < MAX_NR_EP);
    regs().uep01234_ctrl(ep)
}

#[inline]
pub(crate) fn endpoint_dma(
    ep: usize,
) -> crate::pac::common::Reg<crate::pac::usb::regs::UepDma, crate::pac::common::RW> {
    assert!(ep < MAX_NR_EP);
    regs().uep0123_dma(ep)
}

#[inline]
pub(crate) fn endpoint_t_len(
    ep: usize,
) -> crate::pac::common::Reg<crate::pac::usb::regs::UepTLen, crate::pac::common::RW> {
    assert!(ep < MAX_NR_EP);
    regs().uep01234_t_len(ep)
}

pub(crate) fn configure_usb_pins(enabled: bool) {
    crate::pac::AFIO.ctlr().modify(|w| {
        if enabled {
            // CH32X035 at 3.3 V: use the 1.5 kohm D+ pull-up and 3.3 V PHY.
            // PC16 is D- and PC17 is D+.
            w.set_udm_pue(0);
            w.set_udp_pue(3);
            w.set_usb_phy_v33(true);
            w.set_usb_ioen(true);
        } else {
            w.set_udm_pue(0);
            w.set_udp_pue(0);
            w.set_usb_ioen(false);
        }
    });
}
