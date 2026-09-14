//! Fixed-size USBPD peripheral tracing for timing-sensitive diagnostics.
//!
//! This module is compiled only with `usbpd-driver-trace`. Its callback may
//! run from the USBPD interrupt handler and must therefore do only bounded,
//! nonblocking work, such as copying the record into a fixed RAM ring. It
//! must not format, allocate, wait, perform I/O, or call back into the HAL.

use core::cell::RefCell;

use critical_section::Mutex;

/// USBPD peripheral trace record ABI version.
pub const USBPD_TRACE_ABI_VERSION: u8 = 1;

/// USBPD receive-boundary event category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum UsbPdTraceEventKind {
    /// Receive hardware has been armed.
    RxArmed = 1,
    /// The USBPD interrupt handler observed peripheral status.
    Interrupt = 2,
    /// The receive future returned a result to its caller.
    RxComplete = 3,
    /// The receive future was dropped before returning a result.
    RxCancelled = 4,
}

/// Event-specific code stored in [`UsbPdTraceEvent::code`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum UsbPdTraceCode {
    /// A task armed a new receive operation.
    TaskArm = 1,
    /// TX-end armed receive before waking the transmitting task.
    TxTurnaroundArm = 2,
    /// A receive future consumed a TX-end pre-armed operation.
    PrearmedReceive = 3,
    /// The interrupt record has no additional classification.
    Interrupt = 4,
    /// A complete SOP frame was delivered.
    Success = 5,
    /// Hard Reset signaling ended the receive.
    HardReset = 6,
    /// The peripheral reported a DMA/buffer error.
    BufferError = 7,
    /// The caller's destination was too small for the complete frame.
    BufferTooSmall = 8,
    /// The completed frame had an unsupported or rejected SOP/type.
    Rejected = 9,
    /// The future was cancelled before returning a result.
    Cancelled = 10,
    /// Another receive result not expected from the ordinary async path.
    Other = 255,
}

/// One fixed, formatter-free USBPD peripheral trace record.
///
/// `status` and `config` are raw CH32 USBPD register values. `byte_count` is
/// the raw nine-bit DMA byte count, including the four CRC bytes on a
/// completed ordinary frame. `active_cc` is 1 for CC1 and 2 for CC2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct UsbPdTraceEvent {
    /// Event category.
    pub kind: UsbPdTraceEventKind,
    /// Event-specific [`UsbPdTraceCode`] value.
    pub code: u8,
    /// Raw USBPD STATUS register.
    pub status: u8,
    /// Selected CC input: 1 for CC1 and 2 for CC2.
    pub active_cc: u8,
    /// Raw USBPD BMC byte count.
    pub byte_count: u16,
    /// Raw USBPD CONFIG register.
    pub config: u16,
}

const _: () = assert!(core::mem::size_of::<UsbPdTraceEvent>() == 8);

/// Synchronous USBPD peripheral trace callback.
///
/// The callback may run in interrupt context. It must be bounded and
/// nonblocking and must not format, allocate, wait, perform I/O, or call back
/// into the USBPD HAL.
pub type UsbPdTraceCallback = fn(UsbPdTraceEvent);

static CALLBACK: Mutex<RefCell<Option<UsbPdTraceCallback>>> = Mutex::new(RefCell::new(None));

/// Install or remove the process-wide USBPD peripheral trace callback.
///
/// Registration uses a short critical section for targets without
/// pointer-width atomics. Delivery copies the function pointer in a critical
/// section and invokes it only after leaving that section.
pub fn set_usbpd_trace_callback(callback: Option<UsbPdTraceCallback>) -> Option<UsbPdTraceCallback> {
    critical_section::with(|cs| CALLBACK.borrow(cs).replace(callback))
}

pub(crate) fn emit(event: UsbPdTraceEvent) {
    let callback = critical_section::with(|cs| *CALLBACK.borrow(cs).borrow());
    if let Some(callback) = callback {
        callback(event);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::vec::Vec;

    use super::*;

    static SERIAL: StdMutex<()> = StdMutex::new(());
    static EVENTS: StdMutex<Vec<UsbPdTraceEvent>> = StdMutex::new(Vec::new());

    fn capture(event: UsbPdTraceEvent) {
        EVENTS.lock().unwrap().push(event);
    }

    fn event(kind: UsbPdTraceEventKind, code: UsbPdTraceCode, status: u8) -> UsbPdTraceEvent {
        UsbPdTraceEvent {
            kind,
            code: code as u8,
            status,
            active_cc: 2,
            byte_count: 7,
            config: 0x600c,
        }
    }

    #[test]
    fn fixed_records_are_delivered_in_boundary_order() {
        let _serial = SERIAL.lock().unwrap();
        EVENTS.lock().unwrap().clear();
        let previous = set_usbpd_trace_callback(Some(capture));

        let armed = event(UsbPdTraceEventKind::RxArmed, UsbPdTraceCode::TaskArm, 0x00);
        let interrupt = event(UsbPdTraceEventKind::Interrupt, UsbPdTraceCode::Interrupt, 0x20);
        let complete = event(UsbPdTraceEventKind::RxComplete, UsbPdTraceCode::Success, 0x00);
        let cancelled = event(UsbPdTraceEventKind::RxCancelled, UsbPdTraceCode::Cancelled, 0x00);
        emit(armed);
        emit(interrupt);
        emit(complete);
        emit(cancelled);

        assert_eq!(&*EVENTS.lock().unwrap(), &[armed, interrupt, complete, cancelled]);
        assert_eq!(core::mem::size_of::<UsbPdTraceEvent>(), 8);
        set_usbpd_trace_callback(previous);
    }
}
