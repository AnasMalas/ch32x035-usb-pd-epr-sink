//! Fixed-size numeric tracing for timing-sensitive protocol diagnostics.
//!
//! This module is available only with the `numeric-trace` feature. The trace
//! callback runs synchronously in the protocol task and therefore must perform
//! only bounded, nonblocking work such as copying the record into an
//! application-owned fixed RAM buffer. It must not format, allocate, wait,
//! perform I/O, or call back into the USB-PD stack.

use core::cell::RefCell;

use critical_section::Mutex;

/// Numeric trace record ABI version.
pub const NUMERIC_TRACE_ABI_VERSION: u8 = 1;

/// Value used when an eight-bit trace field is not available for an event.
pub const UNAVAILABLE_U8: u8 = u8::MAX;

/// Value used when a sixteen-bit trace field is not available for an event.
pub const UNAVAILABLE_U16: u16 = u16::MAX;

/// Trace event category.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum NumericTraceEventKind {
    /// An ordinary protocol message is about to be submitted to the driver.
    TxStart = 1,
    /// The software protocol path is about to wait for GoodCRC.
    GoodCrcWait = 2,
    /// GoodCRC was received or verified by retry-capable hardware.
    GoodCrcReceived = 3,
    /// The driver owns retry and GoodCRC verification for this transmission.
    TxHardwareRetry = 4,
    /// The software protocol path will retry the same message.
    TxRetry = 5,
    /// The complete protocol transmission succeeded.
    TxSuccess = 6,
    /// The complete protocol transmission failed.
    TxFailure = 7,
    /// A valid message header was received from the driver.
    RxMessage = 8,
    /// GoodCRC was transmitted or was handled by receive hardware.
    GoodCrcTransmitted = 9,
    /// A repeated receive MessageID was acknowledged and discarded.
    RxRetransmission = 10,
    /// A protocol-layer operation returned an error.
    ProtocolError = 11,
    /// Hard Reset reception, signaling, or policy execution.
    HardReset = 12,
    /// EPR keepalive policy progress.
    EprKeepAlive = 13,
}

/// Path used to complete GoodCRC handling or an ordinary transmission.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum NumericTracePath {
    /// GoodCRC or retry handling was performed by the protocol layer.
    Software = 0,
    /// GoodCRC or retry handling was performed inside the PHY peripheral.
    Hardware = 1,
}

/// Reason for retrying or failing a transmission.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum NumericTraceTxReason {
    /// The driver discarded the transmission.
    DriverDiscarded = 1,
    /// GoodCRC did not arrive before CRCReceiveTimer expired.
    GoodCrcTimeout = 2,
    /// Hard Reset signaling interrupted transmission.
    HardReset = 3,
    /// The Type-C connection or VBUS disappeared.
    Detached = 4,
    /// The protocol retry limit was exhausted.
    RetriesExceeded = 5,
    /// A received GoodCRC carried the wrong MessageID.
    AcknowledgeMismatch = 6,
    /// Another receive or protocol error terminated transmission.
    Other = 255,
}

/// Numeric classification of a returned protocol error.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum NumericTraceProtocolError {
    /// Too many receive frames were discarded.
    RxDiscarded = 1,
    /// The Type-C connection or VBUS disappeared while receiving.
    RxDetached = 2,
    /// A partner-initiated Soft Reset was received.
    RxSoftReset = 3,
    /// Hard Reset signaling was observed while receiving.
    RxHardReset = 4,
    /// A receive timer expired.
    RxTimeout = 5,
    /// The received message is unsupported in this implementation.
    RxUnsupported = 6,
    /// A received frame could not be parsed.
    RxParse = 7,
    /// GoodCRC acknowledged a different MessageID.
    RxAcknowledgeMismatch = 8,
    /// The driver discarded a transmission.
    TxDiscarded = 9,
    /// The Type-C connection or VBUS disappeared while transmitting.
    TxDetached = 10,
    /// Hard Reset signaling interrupted transmission.
    TxHardReset = 11,
    /// A locally constructed message failed validation.
    TxValidation = 12,
    /// The maximum transmission retry count was exceeded.
    TxRetriesExceeded = 13,
    /// A valid but unexpected message was received.
    UnexpectedMessage = 14,
}

/// Hard Reset trace phase stored in [`NumericTraceEvent::code`].
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum NumericTraceHardResetPhase {
    /// Hard Reset signaling was received from the partner.
    Received = 1,
    /// The sink policy engine is about to transmit Hard Reset signaling.
    TransmitStart = 2,
    /// A discarded Hard Reset signaling attempt will be retried.
    TransmitRetry = 3,
    /// Hard Reset signaling completed.
    TransmitComplete = 4,
    /// Hard Reset signaling failed.
    TransmitFailure = 5,
}

/// EPR keepalive phase stored in [`NumericTraceEvent::code`].
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum NumericTraceEprKeepAlivePhase {
    /// The policy engine entered EPR keepalive and is about to send the request.
    Request = 1,
    /// A valid EPR KeepAlive Acknowledgement was received.
    Acknowledged = 2,
    /// SenderResponseTimer expired while waiting for the acknowledgement.
    Timeout = 3,
    /// The response was present but not a valid EPR KeepAlive Acknowledgement.
    UnexpectedResponse = 4,
    /// Another protocol error interrupted the keepalive exchange.
    ProtocolFailure = 5,
}

/// One fixed, formatter-free numeric trace record.
///
/// The record is eight bytes with C field layout. Field interpretation is
/// selected by [`Self::kind`]:
///
/// - message events place the raw USB-PD Message Header in `header`; extended
///   messages place the raw Extended Header in `detail`, and Extended Control
///   messages place their control type in `code`;
/// - `message_id` is the on-wire MessageID and `counter` is the current retry
///   or Hard Reset count when one is available;
/// - GoodCRC events use [`NumericTracePath`] in `code`;
/// - retry/failure events use [`NumericTraceTxReason`] in `code`;
/// - protocol errors use [`NumericTraceProtocolError`] in `code` and an
///   error-specific numeric value in `detail`;
/// - Hard Reset events use [`NumericTraceHardResetPhase`] in `code` and the
///   `HardResetReason` discriminant in `detail` when reason tracking is enabled;
/// - EPR keepalive events use [`NumericTraceEprKeepAlivePhase`] in `code`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct NumericTraceEvent {
    /// Event category.
    pub kind: NumericTraceEventKind,
    /// Event-specific numeric code.
    pub code: u8,
    /// USB-PD MessageID, or [`UNAVAILABLE_U8`].
    pub message_id: u8,
    /// Retry/Hard Reset counter, or [`UNAVAILABLE_U8`].
    pub counter: u8,
    /// Raw 16-bit USB-PD Message Header, or [`UNAVAILABLE_U16`].
    pub header: u16,
    /// Raw Extended Header or event-specific detail, or [`UNAVAILABLE_U16`].
    pub detail: u16,
}

const _: () = assert!(core::mem::size_of::<NumericTraceEvent>() == 8);

impl NumericTraceEvent {
    pub(crate) const fn new(
        kind: NumericTraceEventKind,
        code: u8,
        message_id: u8,
        counter: u8,
        header: u16,
        detail: u16,
    ) -> Self {
        Self { kind, code, message_id, counter, header, detail }
    }

    pub(crate) fn from_frame(kind: NumericTraceEventKind, frame: &[u8], counter: u8) -> Self {
        let header = if frame.len() >= 2 { u16::from_le_bytes([frame[0], frame[1]]) } else { UNAVAILABLE_U16 };
        let message_id = if header == UNAVAILABLE_U16 { UNAVAILABLE_U8 } else { ((header >> 9) & 0x7) as u8 };
        let extended = header != UNAVAILABLE_U16 && header & (1 << 15) != 0;
        let detail =
            if extended && frame.len() >= 4 { u16::from_le_bytes([frame[2], frame[3]]) } else { UNAVAILABLE_U16 };
        let code = if extended && header & 0x1f == 0x10 && frame.len() >= 5 { frame[4] } else { UNAVAILABLE_U8 };
        Self::new(kind, code, message_id, counter, header, detail)
    }
}

/// Synchronous trace callback type.
///
/// The callback must be bounded and nonblocking. A callback that formats,
/// waits, allocates, performs I/O, or calls the USB-PD stack can violate
/// protocol deadlines.
pub type NumericTraceCallback = fn(NumericTraceEvent);

static CALLBACK: Mutex<RefCell<Option<NumericTraceCallback>>> = Mutex::new(RefCell::new(None));

/// Install or remove the process-wide numeric trace callback.
///
/// Registration uses a short critical section to support targets without
/// pointer-width atomics. The previous callback is returned. Event delivery
/// copies the pointer under a critical section but invokes the callback only
/// after leaving that section.
pub fn set_numeric_trace_callback(callback: Option<NumericTraceCallback>) -> Option<NumericTraceCallback> {
    critical_section::with(|cs| CALLBACK.borrow(cs).replace(callback))
}

pub(crate) fn emit(event: NumericTraceEvent) {
    let callback = critical_section::with(|cs| *CALLBACK.borrow(cs).borrow());
    if let Some(callback) = callback {
        callback(event);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex as StdMutex, MutexGuard};
    use std::thread::ThreadId;
    use std::vec::Vec;

    use super::{NumericTraceCallback, NumericTraceEvent, set_numeric_trace_callback};

    static SERIAL: StdMutex<()> = StdMutex::new(());
    static OWNER: StdMutex<Option<ThreadId>> = StdMutex::new(None);
    static EVENTS: StdMutex<Vec<NumericTraceEvent>> = StdMutex::new(Vec::new());

    fn lock<T>(mutex: &'static StdMutex<T>) -> MutexGuard<'static, T> {
        mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn capture(event: NumericTraceEvent) {
        let current = std::thread::current().id();
        if lock(&OWNER).as_ref() == Some(&current) {
            lock(&EVENTS).push(event);
        }
    }

    pub(crate) struct CaptureGuard {
        previous: Option<NumericTraceCallback>,
        _serial: MutexGuard<'static, ()>,
    }

    impl CaptureGuard {
        pub(crate) fn start() -> Self {
            let serial = lock(&SERIAL);
            lock(&EVENTS).clear();
            *lock(&OWNER) = Some(std::thread::current().id());
            let previous = set_numeric_trace_callback(Some(capture));
            Self { previous, _serial: serial }
        }

        pub(crate) fn events(&self) -> Vec<NumericTraceEvent> {
            lock(&EVENTS).clone()
        }
    }

    impl Drop for CaptureGuard {
        fn drop(&mut self) {
            set_numeric_trace_callback(self.previous);
            *lock(&OWNER) = None;
        }
    }
}
