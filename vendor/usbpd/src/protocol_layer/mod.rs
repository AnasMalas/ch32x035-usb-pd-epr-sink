//! The protocol layer is controlled by the policy engine, and commands the PHY layer.
//!
//! Handles
//! - construction of messages,
//! - message timers and timeouts,
//! - message retry counters,
//! - reset operation,
//! - error handling,
//! - state behaviour.
//!
//! Extended-message receive supports chunk assembly. Transmit supports one
//! legal chunk and rejects larger payloads until a general TX chunk state
//! machine is added.

pub mod message;

use core::future::Future;
use core::marker::PhantomData;

use byteorder::{ByteOrder, LittleEndian};
use embassy_futures::select::{Either, select};
use heapless::Vec;
use message::Message;
use message::data::source_capabilities::SourceCapabilities;
use message::data::{Data, request};
use message::extended::extended_control::{ExtendedControl, ExtendedControlMessageType};
use message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

use crate::PowerRole;
use crate::counters::{Counter, CounterType, Error as CounterError};
use crate::protocol_layer::message::data::epr_mode::EprModeDataObject;
use crate::protocol_layer::message::extended::Extended;
use crate::protocol_layer::message::extended::sink_capabilities_extended::SinkCapabilitiesExtended;
use crate::protocol_layer::message::{ParseError, Payload};
use crate::timers::{Timer, TimerType};

/// Maximum on-wire PD frame: two-byte Header plus seven Data Objects.
const MAX_PD_FRAME_SIZE: usize = 30;

/// Maximum assembled Extended Message payload.
const MAX_EXTENDED_MESSAGE_SIZE: usize = message::extended::chunked::MAX_EXTENDED_MSG_LEN;

/// Size of the message header in bytes.
const MSG_HEADER_SIZE: usize = 2;

/// Size of the extended message header in bytes.
const EXT_HEADER_SIZE: usize = 2;

/// Prevent a driver that repeatedly returns a retryable error from spinning the
/// protocol task forever without returning control to the policy engine.
const MAX_DRIVER_DISCARDS: u8 = 8;

const SOURCE_CAPABILITY_MESSAGE_TYPES: [MessageType; 2] = [
    MessageType::Data(DataMessageType::SourceCapabilities),
    MessageType::Extended(ExtendedMessageType::EprSourceCapabilities),
];

/// Compact representation of a message received by the sink policy engine.
///
/// Source Capabilities are kept in protocol-owned storage and represented by
/// a marker here so the 48-byte PDO list is not copied through every async
/// receive/select result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SinkMessage {
    pub(crate) header: Header,
    pub(crate) payload: SinkPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SinkPayload {
    None,
    SourceCapabilities,
    EprMode(EprModeDataObject),
    SourceInfo(message::data::source_info::SourceInfo),
    Alert(message::data::alert::AlertDataObject),
    Status(message::extended::status::Status),
    PpsStatus(message::extended::pps_status::PpsStatus),
    ExtendedControl(ExtendedControl),
    Unknown,
}

/// Errors that can occur in the protocol layer.
#[derive(thiserror::Error, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ProtocolError {
    /// An error occured during data reception.
    #[error("RX error")]
    RxError(#[from] RxError),
    /// An error occured during data transmission.
    #[error("TX error")]
    TxError(#[from] TxError),
    /// The locally constructed message cannot be transmitted correctly.
    #[error("locally invalid TX message: {0}")]
    TxValidation(#[from] TxValidationError),
    /// Transmission failed after the maximum number of allowed retries.
    #[error("transmit retries (`{0}`) exceeded")]
    TransmitRetriesExceeded(u8),
    /// An unexpected message was received.
    #[error("unexpected message")]
    UnexpectedMessage,
}

/// Errors that can occur during reception of data.
#[derive(thiserror::Error, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum RxError {
    /// Too many consecutive frames were discarded by the PHY.
    #[error("excessive discarded frames")]
    Discarded,
    /// The Type-C connection or VBUS was removed.
    #[error("detached")]
    Detached,
    /// Port partner requested soft reset.
    #[error("soft reset")]
    SoftReset,
    /// Driver reported a hard reset.
    #[error("hard reset")]
    HardReset,
    /// A timeout during message reception.
    #[error("receive timeout")]
    ReceiveTimeout,
    /// An unsupported message was received.
    #[error("unsupported message")]
    UnsupportedMessage,
    /// A message parsing error occured.
    #[error("parse error")]
    ParseError(#[from] ParseError),
    /// The received acknowledgement does not match the last transmitted message's ID.
    #[error("wrong tx id `{0}` acknowledged")]
    AcknowledgeMismatch(u8),
}

/// Errors that can occur during transmission of data.
#[derive(thiserror::Error, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TxError {
    /// The driver could not transmit because the line was busy or noisy.
    #[error("transmit discarded")]
    Discarded,
    /// The Type-C connection or VBUS was removed.
    #[error("detached")]
    Detached,
    /// Driver reported a hard reset.
    #[error("hard reset")]
    HardReset,
}

/// Errors found while validating a locally constructed outgoing message.
///
/// These are deterministic caller/stack errors. Retrying them on the wire
/// cannot succeed, and the driver is never invoked for them.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TxValidationError {
    /// unchunked_extended_messages_supported must be false (library uses chunked mode).
    #[error("unchunked extended messages not supported")]
    UnchunkedExtendedMessagesNotSupported,
    /// AVS voltage LSB 2 bits must be zero per USB PD 3.2 Table 6.26.
    #[error("AVS voltage alignment invalid")]
    AvsVoltageAlignmentInvalid,
    /// A payload larger than one 26-byte chunk needs the extended-message TX
    /// state machine, which is not implemented yet.
    #[error("extended payload requires multi-chunk transmission")]
    ExtendedMessageChunkingRequired,
}

#[cfg(feature = "numeric-trace")]
fn emit_frame_event(kind: crate::numeric_trace::NumericTraceEventKind, frame: &[u8], counter: u8, code: Option<u8>) {
    let mut event = crate::numeric_trace::NumericTraceEvent::from_frame(kind, frame, counter);
    if let Some(code) = code {
        event.code = code;
    }
    crate::numeric_trace::emit(event);
}

#[cfg(feature = "numeric-trace")]
fn emit_protocol_error(error: &ProtocolError) {
    use crate::numeric_trace::{NumericTraceEvent, NumericTraceEventKind, NumericTraceProtocolError, UNAVAILABLE_U8};

    let (code, detail) = match error {
        ProtocolError::RxError(RxError::Discarded) => (NumericTraceProtocolError::RxDiscarded, 0),
        ProtocolError::RxError(RxError::Detached) => (NumericTraceProtocolError::RxDetached, 0),
        ProtocolError::RxError(RxError::SoftReset) => (NumericTraceProtocolError::RxSoftReset, 0),
        ProtocolError::RxError(RxError::HardReset) => (NumericTraceProtocolError::RxHardReset, 0),
        ProtocolError::RxError(RxError::ReceiveTimeout) => (NumericTraceProtocolError::RxTimeout, 0),
        ProtocolError::RxError(RxError::UnsupportedMessage) => (NumericTraceProtocolError::RxUnsupported, 0),
        ProtocolError::RxError(RxError::ParseError(_)) => (NumericTraceProtocolError::RxParse, 0),
        ProtocolError::RxError(RxError::AcknowledgeMismatch(message_id)) => {
            (NumericTraceProtocolError::RxAcknowledgeMismatch, u16::from(*message_id))
        }
        ProtocolError::TxError(TxError::Discarded) => (NumericTraceProtocolError::TxDiscarded, 0),
        ProtocolError::TxError(TxError::Detached) => (NumericTraceProtocolError::TxDetached, 0),
        ProtocolError::TxError(TxError::HardReset) => (NumericTraceProtocolError::TxHardReset, 0),
        ProtocolError::TxValidation(validation) => {
            let detail = match validation {
                TxValidationError::UnchunkedExtendedMessagesNotSupported => 1,
                TxValidationError::AvsVoltageAlignmentInvalid => 2,
                TxValidationError::ExtendedMessageChunkingRequired => 3,
            };
            (NumericTraceProtocolError::TxValidation, detail)
        }
        ProtocolError::TransmitRetriesExceeded(count) => {
            (NumericTraceProtocolError::TxRetriesExceeded, u16::from(*count))
        }
        ProtocolError::UnexpectedMessage => (NumericTraceProtocolError::UnexpectedMessage, 0),
    };
    crate::numeric_trace::emit(NumericTraceEvent::new(
        NumericTraceEventKind::ProtocolError,
        code as u8,
        UNAVAILABLE_U8,
        UNAVAILABLE_U8,
        crate::numeric_trace::UNAVAILABLE_U16,
        detail,
    ));
}

#[cfg(feature = "numeric-trace")]
fn emit_tx_failure(frame: &[u8], counter: u8, reason: crate::numeric_trace::NumericTraceTxReason) {
    emit_frame_event(crate::numeric_trace::NumericTraceEventKind::TxFailure, frame, counter, Some(reason as u8));
}

#[derive(Debug)]
struct Counters {
    _busy: Counter,
    _caps: Counter, // Unused, optional.
    _discover_identity: Counter,
    rx_message: Option<Counter>,
    tx_message: Counter,
    retry: Counter,
}

impl Default for Counters {
    fn default() -> Self {
        Counters {
            _busy: Counter::new(CounterType::Busy),
            _caps: Counter::new(CounterType::Caps),
            _discover_identity: Counter::new(CounterType::DiscoverIdentity),
            rx_message: None,
            tx_message: Counter::new(CounterType::MessageId),
            retry: Counter::new(CounterType::Retry),
        }
    }
}

/// The USB PD protocol layer.
#[derive(Debug)]
pub(crate) struct ProtocolLayer<DRIVER: Driver, TIMER: Timer> {
    driver: DRIVER,
    counters: Counters,
    default_header: Header,
    rx_buffer: [u8; MAX_PD_FRAME_SIZE],
    sink_source_capabilities: Option<SourceCapabilities>,
    extended_rx_buffer: Vec<u8, MAX_EXTENDED_MESSAGE_SIZE>,
    extended_rx_expected: Option<(ExtendedMessageType, u16, u8)>,
    _timer: PhantomData<TIMER>,
}

impl<DRIVER: Driver, TIMER: Timer> ProtocolLayer<DRIVER, TIMER> {
    /// Create a new protocol layer from a driver and default header.
    pub fn new(driver: DRIVER, default_header: Header) -> Self {
        Self {
            driver,
            counters: Default::default(),
            default_header,
            rx_buffer: [0; MAX_PD_FRAME_SIZE],
            sink_source_capabilities: None,
            extended_rx_buffer: Vec::new(),
            extended_rx_expected: None,
            _timer: PhantomData,
        }
    }

    /// Reset the protocol layer.
    pub fn reset(&mut self) {
        self.counters = Default::default();
        self.sink_source_capabilities = None;
        self.reset_chunked_rx();
    }

    /// Access the physical transport for port-level recovery.
    pub(crate) fn driver_mut(&mut self) -> &mut DRIVER {
        &mut self.driver
    }

    /// Backwards-compatible test helper.
    #[cfg(test)]
    pub fn driver(&mut self) -> &mut DRIVER {
        self.driver_mut()
    }

    /// Access the default header directly.
    #[cfg_attr(all(not(feature = "source"), not(test)), allow(dead_code))]
    pub fn header(&self) -> &Header {
        &self.default_header
    }

    /// Change the header's data role after a data role swap
    /// FIXME: Use this after a data role swap
    #[allow(unused)]
    pub fn set_header_data_role(&mut self, role: crate::DataRole) {
        self.default_header.set_port_data_role(role);
    }

    fn get_message_buffer() -> [u8; MAX_PD_FRAME_SIZE] {
        [0u8; MAX_PD_FRAME_SIZE]
    }

    /// Receive one complete wire frame into protocol-owned storage.
    ///
    /// Keeping the buffer in the protocol layer lets every receive path share
    /// the driver retry/length/header checks and prepares the sink path to pass
    /// only compact metadata across its async boundary.
    async fn receive_frame(&mut self) -> Result<(Header, usize), RxError> {
        let mut discarded = 0;
        loop {
            let length = match self.driver.receive(&mut self.rx_buffer).await {
                Ok(length) => length,
                Err(DriverRxError::Discarded) => {
                    discarded += 1;
                    if discarded >= MAX_DRIVER_DISCARDS {
                        return Err(RxError::Discarded);
                    }
                    continue;
                }
                Err(DriverRxError::HardReset) => return Err(RxError::HardReset),
                Err(DriverRxError::Detached) => return Err(RxError::Detached),
            };

            if length > self.rx_buffer.len() {
                return Err(ParseError::InvalidLength { expected: self.rx_buffer.len(), found: length }.into());
            }
            if length < MSG_HEADER_SIZE {
                return Err(ParseError::InvalidLength { expected: MSG_HEADER_SIZE, found: length }.into());
            }

            let header = Header::from_bytes(&self.rx_buffer[..MSG_HEADER_SIZE])?;
            Message::validate_frame_length(&self.rx_buffer[..length], header)?;
            numeric_trace!(crate::numeric_trace::NumericTraceEvent::from_frame(
                crate::numeric_trace::NumericTraceEventKind::RxMessage,
                &self.rx_buffer[..length],
                crate::numeric_trace::UNAVAILABLE_U8,
            ));
            return Ok((header, length));
        }
    }

    /// Get a timer future for a given type.
    pub fn get_timer(timer_type: TimerType) -> impl Future<Output = ()> {
        TimerType::get_timer::<TIMER>(timer_type)
    }

    /// Receive a simple (non-chunked) message from the driver.
    /// Used by wait_for_good_crc to avoid recursion with chunked message handling.
    async fn receive_simple(&mut self) -> Result<Header, RxError> {
        let (header, _) = self.receive_frame().await?;
        Ok(header)
    }

    /// Wait until a GoodCrc message is received, or a timeout occurs.
    async fn wait_for_good_crc(&mut self) -> Result<(), RxError> {
        trace!("Wait for GoodCrc");

        let timeout_fut = Self::get_timer(TimerType::CRCReceive);
        let receive_fut = async {
            let header = self.receive_simple().await?;

            if matches!(header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)) {
                numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                    crate::numeric_trace::NumericTraceEventKind::GoodCrcReceived,
                    crate::numeric_trace::NumericTracePath::Software as u8,
                    header.message_id(),
                    self.counters.retry.value(),
                    header.0,
                    crate::numeric_trace::UNAVAILABLE_U16,
                ));
                trace!(
                    "Received GoodCrc, TX message count: {}, expected: {}",
                    header.message_id(),
                    self.counters.tx_message.value()
                );
                if header.message_id() == self.counters.tx_message.value() {
                    // See spec, [6.7.1.1]
                    self.counters.retry.reset();
                    _ = self.counters.tx_message.increment();
                    Ok(())
                } else {
                    Err(RxError::AcknowledgeMismatch(header.message_id()))
                }
            } else if matches!(header.message_type(), MessageType::Control(_)) {
                Err(ParseError::InvalidControlMessageType(header.message_type_raw()).into())
            } else {
                Err(ParseError::InvalidMessageType(header.message_type_raw()).into())
            }
        };

        match select(timeout_fut, receive_fut).await {
            Either::First(_) => Err(RxError::ReceiveTimeout),
            Either::Second(receive_result) => receive_result,
        }
    }

    /// Validate an outgoing message for spec compliance.
    ///
    /// This catches common mistakes when constructing messages:
    /// - unchunked_extended_messages_supported should always be false
    /// - AVS voltage LSB 2 bits should be zero (per USB PD 3.2 Table 6.26)
    ///
    /// Only validates outgoing messages - never called when parsing received data.
    /// Returns an error if validation fails, allowing the caller to handle it appropriately.
    fn validate_outgoing_message(message: &Message) -> Result<(), TxValidationError> {
        if let Some(Payload::Extended(extended)) = &message.payload
            && extended.data_size() > 26
        {
            return Err(TxValidationError::ExtendedMessageChunkingRequired);
        }

        if let Some(Payload::Data(message::data::Data::Request(power_source))) = &message.payload {
            use message::data::request::PowerSource;
            match power_source {
                PowerSource::FixedVariableSupply(rdo) => {
                    if rdo.unchunked_extended_messages_supported() {
                        return Err(TxValidationError::UnchunkedExtendedMessagesNotSupported);
                    }
                }
                PowerSource::Pps(rdo) => {
                    if rdo.unchunked_extended_messages_supported() {
                        return Err(TxValidationError::UnchunkedExtendedMessagesNotSupported);
                    }
                }
                PowerSource::Avs(rdo) => {
                    if rdo.unchunked_extended_messages_supported() {
                        return Err(TxValidationError::UnchunkedExtendedMessagesNotSupported);
                    }
                    if rdo.raw_output_voltage() & 0x3 != 0 {
                        return Err(TxValidationError::AvsVoltageAlignmentInvalid);
                    }
                }
                PowerSource::EprRequest(epr) => {
                    // Check the raw RDO for validation
                    let rdo_bits = epr.rdo;
                    let unchunked = (rdo_bits >> 23) & 1 == 1;
                    if unchunked {
                        return Err(TxValidationError::UnchunkedExtendedMessagesNotSupported);
                    }

                    // The selected PDO, not the RDO's object-position bits,
                    // determines the RDO format. APDO type 01 is EPR AVS and
                    // type 10 is SPR AVS.
                    let raw_pdo = epr.pdo;
                    let apdo_type = (raw_pdo >> 28) & 0x3;
                    let is_avs = (raw_pdo >> 30) & 0x3 == 0x3 && matches!(apdo_type, 0x1 | 0x2);
                    if is_avs {
                        let voltage = (rdo_bits >> 9) & 0xFFF;
                        if (voltage as u16) & 0x3 != 0 {
                            return Err(TxValidationError::AvsVoltageAlignmentInvalid);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn transmit_inner(&mut self, buffer: &[u8]) -> Result<(), TxError> {
        match self.driver.transmit(buffer).await {
            Ok(_) => Ok(()),
            Err(DriverTxError::HardReset) => Err(TxError::HardReset),
            Err(DriverTxError::Discarded) => Err(TxError::Discarded),
            Err(DriverTxError::Detached) => Err(TxError::Detached),
        }
    }

    /// Transmit a message.
    ///
    // GoodCrc message transmission is handled separately.
    // See `transmit_good_crc()` instead.
    pub async fn transmit(&mut self, message: Message) -> Result<(), ProtocolError> {
        assert_ne!(message.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC));

        // Validate outgoing message for spec compliance before entering the driver path.
        #[cfg(not(feature = "numeric-trace"))]
        Self::validate_outgoing_message(&message)?;
        #[cfg(feature = "numeric-trace")]
        {
            if let Err(validation) = Self::validate_outgoing_message(&message) {
                let error = ProtocolError::TxValidation(validation);
                emit_protocol_error(&error);
                return Err(error);
            }
        }

        trace!("Transmit message: {:?}", message);

        let mut buffer = Self::get_message_buffer();
        let size = message.to_bytes(&mut buffer);

        if DRIVER::HAS_AUTO_RETRY {
            // Hardware handles retries and verifies GoodCRC reception.
            // Call driver.transmit() directly (not transmit_inner()) because
            // Discarded here means all hardware retries exhausted — no point
            // retrying in software.
            numeric_trace!(crate::numeric_trace::NumericTraceEvent::from_frame(
                crate::numeric_trace::NumericTraceEventKind::TxStart,
                &buffer[..size],
                self.counters.retry.value(),
            ));
            #[cfg(feature = "numeric-trace")]
            emit_frame_event(
                crate::numeric_trace::NumericTraceEventKind::TxHardwareRetry,
                &buffer[..size],
                self.counters.retry.max_value(),
                Some(crate::numeric_trace::NumericTracePath::Hardware as u8),
            );
            match self.driver.transmit(&buffer[..size]).await {
                Ok(()) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_frame_event(
                        crate::numeric_trace::NumericTraceEventKind::GoodCrcReceived,
                        &buffer[..size],
                        self.counters.retry.value(),
                        Some(crate::numeric_trace::NumericTracePath::Hardware as u8),
                    );
                    self.counters.retry.reset();
                    _ = self.counters.tx_message.increment();
                    #[cfg(feature = "numeric-trace")]
                    emit_frame_event(
                        crate::numeric_trace::NumericTraceEventKind::TxSuccess,
                        &buffer[..size],
                        0,
                        Some(crate::numeric_trace::NumericTracePath::Hardware as u8),
                    );
                    trace!("Transmit success (hardware retry)");
                    Ok(())
                }
                Err(DriverTxError::HardReset) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..size],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::HardReset,
                    );
                    let error = ProtocolError::from(TxError::HardReset);
                    #[cfg(feature = "numeric-trace")]
                    emit_protocol_error(&error);
                    Err(error)
                }
                Err(DriverTxError::Detached) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..size],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::Detached,
                    );
                    let error = ProtocolError::from(TxError::Detached);
                    #[cfg(feature = "numeric-trace")]
                    emit_protocol_error(&error);
                    Err(error)
                }
                Err(DriverTxError::Discarded) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..size],
                        self.counters.retry.max_value(),
                        crate::numeric_trace::NumericTraceTxReason::RetriesExceeded,
                    );
                    let error = ProtocolError::TransmitRetriesExceeded(self.counters.retry.max_value());
                    #[cfg(feature = "numeric-trace")]
                    emit_protocol_error(&error);
                    Err(error)
                }
            }
        } else {
            // Software retry loop
            self.counters.retry.reset();

            loop {
                numeric_trace!(crate::numeric_trace::NumericTraceEvent::from_frame(
                    crate::numeric_trace::NumericTraceEventKind::TxStart,
                    &buffer[..size],
                    self.counters.retry.value(),
                ));
                match self.transmit_inner(&buffer[..size]).await {
                    Ok(_) => {
                        #[cfg(feature = "numeric-trace")]
                        emit_frame_event(
                            crate::numeric_trace::NumericTraceEventKind::GoodCrcWait,
                            &buffer[..size],
                            self.counters.retry.value(),
                            Some(crate::numeric_trace::NumericTracePath::Software as u8),
                        );
                        match self.wait_for_good_crc().await {
                            Ok(()) => {
                                #[cfg(feature = "numeric-trace")]
                                emit_frame_event(
                                    crate::numeric_trace::NumericTraceEventKind::TxSuccess,
                                    &buffer[..size],
                                    0,
                                    Some(crate::numeric_trace::NumericTracePath::Software as u8),
                                );
                                trace!("Transmit success");
                                return Ok(());
                            }
                            Err(RxError::ReceiveTimeout) => match self.counters.retry.increment() {
                                Ok(_) => {
                                    #[cfg(feature = "numeric-trace")]
                                    emit_frame_event(
                                        crate::numeric_trace::NumericTraceEventKind::TxRetry,
                                        &buffer[..size],
                                        self.counters.retry.value(),
                                        Some(crate::numeric_trace::NumericTraceTxReason::GoodCrcTimeout as u8),
                                    );
                                    // Retry transmission, until the retry counter is exceeded.
                                }
                                Err(CounterError::Exceeded) => {
                                    #[cfg(feature = "numeric-trace")]
                                    emit_tx_failure(
                                        &buffer[..size],
                                        self.counters.retry.max_value(),
                                        crate::numeric_trace::NumericTraceTxReason::RetriesExceeded,
                                    );
                                    let error = ProtocolError::TransmitRetriesExceeded(self.counters.retry.max_value());
                                    #[cfg(feature = "numeric-trace")]
                                    emit_protocol_error(&error);
                                    return Err(error);
                                }
                            },
                            Err(other) => {
                                #[cfg(feature = "numeric-trace")]
                                let reason = match &other {
                                    RxError::AcknowledgeMismatch(_) => {
                                        crate::numeric_trace::NumericTraceTxReason::AcknowledgeMismatch
                                    }
                                    RxError::HardReset => crate::numeric_trace::NumericTraceTxReason::HardReset,
                                    RxError::Detached => crate::numeric_trace::NumericTraceTxReason::Detached,
                                    _ => crate::numeric_trace::NumericTraceTxReason::Other,
                                };
                                #[cfg(feature = "numeric-trace")]
                                emit_tx_failure(&buffer[..size], self.counters.retry.value(), reason);
                                let error = ProtocolError::from(other);
                                #[cfg(feature = "numeric-trace")]
                                emit_protocol_error(&error);
                                return Err(error);
                            }
                        }
                    }
                    Err(TxError::Discarded) => match self.counters.retry.increment() {
                        Ok(_) => {
                            #[cfg(feature = "numeric-trace")]
                            emit_frame_event(
                                crate::numeric_trace::NumericTraceEventKind::TxRetry,
                                &buffer[..size],
                                self.counters.retry.value(),
                                Some(crate::numeric_trace::NumericTraceTxReason::DriverDiscarded as u8),
                            );
                        }
                        Err(CounterError::Exceeded) => {
                            #[cfg(feature = "numeric-trace")]
                            emit_tx_failure(
                                &buffer[..size],
                                self.counters.retry.max_value(),
                                crate::numeric_trace::NumericTraceTxReason::RetriesExceeded,
                            );
                            let error = ProtocolError::TransmitRetriesExceeded(self.counters.retry.max_value());
                            #[cfg(feature = "numeric-trace")]
                            emit_protocol_error(&error);
                            return Err(error);
                        }
                    },
                    Err(other) => {
                        #[cfg(feature = "numeric-trace")]
                        let reason = match &other {
                            TxError::HardReset => crate::numeric_trace::NumericTraceTxReason::HardReset,
                            TxError::Detached => crate::numeric_trace::NumericTraceTxReason::Detached,
                            TxError::Discarded => crate::numeric_trace::NumericTraceTxReason::DriverDiscarded,
                        };
                        #[cfg(feature = "numeric-trace")]
                        emit_tx_failure(&buffer[..size], self.counters.retry.value(), reason);
                        let error = ProtocolError::from(other);
                        #[cfg(feature = "numeric-trace")]
                        emit_protocol_error(&error);
                        return Err(error);
                    }
                }
            }
        }
    }

    /// Send a GoodCrc message to the port partner.
    async fn transmit_good_crc(&mut self) -> Result<(), ProtocolError> {
        trace!("Transmit message GoodCrc for RX message count: {}", self.counters.rx_message.unwrap().value());

        let mut buffer = Self::get_message_buffer();

        let size = Message::new(Header::new_control(
            self.default_header,
            self.counters.rx_message.unwrap(), // A message must have been received before.
            ControlMessageType::GoodCRC,
        ))
        .to_bytes(&mut buffer);

        #[cfg(not(feature = "numeric-trace"))]
        {
            Ok(self.transmit_inner(&buffer[..size]).await?)
        }
        #[cfg(feature = "numeric-trace")]
        {
            let result = self.transmit_inner(&buffer[..size]).await;
            match result {
                Ok(()) => {
                    emit_frame_event(
                        crate::numeric_trace::NumericTraceEventKind::GoodCrcTransmitted,
                        &buffer[..size],
                        self.counters.retry.value(),
                        Some(crate::numeric_trace::NumericTracePath::Software as u8),
                    );
                    Ok(())
                }
                Err(error) => {
                    let error = ProtocolError::from(error);
                    emit_protocol_error(&error);
                    Err(error)
                }
            }
        }
    }

    /// Handle acknowledgement and retransmission detection for a received message.
    ///
    /// Returns `Ok(true)` if this was a retransmission (caller should continue to next message),
    /// `Ok(false)` if this is a new message to process, or `Err` on failure.
    async fn handle_rx_ack(&mut self, header: Header) -> Result<bool, RxError> {
        let is_good_crc = matches!(header.message_type(), MessageType::Control(ControlMessageType::GoodCRC));

        let is_retransmission = if is_good_crc { false } else { self.update_rx_message_counter(header) };

        if !DRIVER::HAS_AUTO_GOOD_CRC && !is_good_crc {
            match self.transmit_good_crc().await {
                Ok(()) => {}
                Err(ProtocolError::TxError(TxError::HardReset)) => return Err(RxError::HardReset),
                Err(ProtocolError::TxError(TxError::Detached)) => return Err(RxError::Detached),
                Err(_) => return Err(RxError::UnsupportedMessage),
            }
        } else if DRIVER::HAS_AUTO_GOOD_CRC && !is_good_crc {
            numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                crate::numeric_trace::NumericTraceEventKind::GoodCrcTransmitted,
                crate::numeric_trace::NumericTracePath::Hardware as u8,
                header.message_id(),
                crate::numeric_trace::UNAVAILABLE_U8,
                header.0,
                crate::numeric_trace::UNAVAILABLE_U16,
            ));
        }

        if is_retransmission {
            numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                crate::numeric_trace::NumericTraceEventKind::RxRetransmission,
                crate::numeric_trace::UNAVAILABLE_U8,
                header.message_id(),
                crate::numeric_trace::UNAVAILABLE_U8,
                header.0,
                crate::numeric_trace::UNAVAILABLE_U16,
            ));
        }

        Ok(is_retransmission)
    }

    /// Reset chunked message reception state.
    fn reset_chunked_rx(&mut self) {
        self.extended_rx_buffer.clear();
        self.extended_rx_expected = None;
    }

    /// Receive a message, assembling chunked extended messages as needed.
    #[cfg(any(feature = "source", test))]
    async fn receive_message_inner(&mut self) -> Result<Message, RxError> {
        loop {
            let (header, length) = self.receive_frame().await?;
            let message_type = header.message_type();

            if matches!(message_type, MessageType::Extended(_)) {
                let ext_header_end = MSG_HEADER_SIZE + EXT_HEADER_SIZE;
                let ext_header =
                    message::extended::ExtendedHeader::from_bytes(&self.rx_buffer[MSG_HEADER_SIZE..ext_header_end]);
                let payload_len = length - ext_header_end;
                let total_size = ext_header.data_size();
                let chunked = ext_header.chunked();
                let chunk_number = ext_header.chunk_number();
                let msg_type = match message_type {
                    MessageType::Extended(mt) => mt,
                    _ => unreachable!(),
                };

                // Update specification revision, based on the received frame.
                self.default_header = self.default_header.with_spec_revision(header.spec_revision()?);

                if chunked {
                    // This implementation never starts a multi-chunk TX, so a
                    // partner has no valid reason to request one of our chunks.
                    if ext_header.request_chunk() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    if total_size as usize > self.extended_rx_buffer.capacity() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    trace!(
                        "Received chunked extended message {:?}, chunk {}, size {}",
                        message_type, chunk_number, payload_len
                    );

                    // Update RX counters and acknowledge.
                    if self.handle_rx_ack(header).await? {
                        continue; // Retransmission
                    }

                    let (expected_total, expected_next) = match self.extended_rx_expected {
                        Some((ty, total, next)) if ty == msg_type && total == total_size => (total, next),
                        Some(_) => {
                            self.reset_chunked_rx();
                            return Err(RxError::UnsupportedMessage);
                        }
                        None if chunk_number == 0 => (total_size, 0),
                        None => {
                            self.reset_chunked_rx();
                            return Err(RxError::UnsupportedMessage);
                        }
                    };

                    // Ensure chunks arrive in order.
                    if expected_next != 0 && chunk_number != expected_next {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }

                    if chunk_number == 0 || expected_next == 0 {
                        self.extended_rx_buffer.clear();
                        self.extended_rx_expected = Some((msg_type, total_size, 1));
                    } else {
                        self.extended_rx_expected = Some((msg_type, expected_total, expected_next + 1));
                    }

                    // Every non-final chunk carries 26 bytes. The final
                    // chunk carries exactly the remaining data plus only the
                    // padding needed to complete its last Data Object.
                    let remaining = (total_size as usize).saturating_sub(self.extended_rx_buffer.len());
                    let expected_payload_len = if remaining > message::extended::chunked::MAX_EXTENDED_MSG_CHUNK_LEN {
                        message::extended::chunked::MAX_EXTENDED_MSG_CHUNK_LEN
                    } else {
                        (EXT_HEADER_SIZE + remaining).div_ceil(4) * 4 - EXT_HEADER_SIZE
                    };
                    if payload_len != expected_payload_len {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }

                    if self.extended_rx_buffer.len() + payload_len > self.extended_rx_buffer.capacity() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    let payload = &self.rx_buffer[ext_header_end..length];
                    if self.extended_rx_buffer.extend_from_slice(payload).is_err() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }

                    if self.extended_rx_buffer.len() < total_size as usize {
                        // Need more chunks - send chunk request per spec 6.12.2.1.2.4
                        let next_chunk = self.extended_rx_expected.as_ref().map(|(_, _, next)| *next).unwrap_or(1);
                        self.transmit_chunk_request(msg_type, next_chunk).await?;
                        continue;
                    }

                    // All chunks received, parse payload.
                    let ext_payload = &self.extended_rx_buffer[..total_size as usize];
                    let parsed_payload = Message::parse_extended_payload(msg_type, ext_payload);
                    self.reset_chunked_rx();
                    let mut message = Message::new(header);
                    message.payload = Some(Payload::Extended(parsed_payload?));

                    trace!("Received assembled extended message {:?}", message);
                    return Ok(message);
                }
            }

            // Non-extended or unchunked extended messages.
            let message = Message::from_bytes(&self.rx_buffer[..length])?;

            // Update specification revision, based on the received frame.
            self.default_header = self.default_header.with_spec_revision(message.header.spec_revision()?);

            match message.header.message_type() {
                MessageType::Control(ControlMessageType::Reserved) | MessageType::Data(DataMessageType::Reserved) => {
                    trace!("Unsupported message type in header: {:?}", message.header);
                    return Err(RxError::UnsupportedMessage);
                }
                _ => (),
            }

            // Handle GoodCRC and retransmissions.
            if self.handle_rx_ack(message.header).await? {
                continue; // Retransmission
            }

            // A partner-initiated Soft Reset is still an ordinary received
            // packet at the Protocol Layer: acknowledge it and record its RX
            // MessageID before asking the Policy Engine to reset protocol
            // state and respond with Accept.
            if matches!(message.header.message_type(), MessageType::Control(ControlMessageType::SoftReset)) {
                return Err(RxError::SoftReset);
            }

            trace!("Received message {:?}", message);
            return Ok(message);
        }
    }

    fn parse_sink_extended_payload(
        message_type: ExtendedMessageType,
        payload: &[u8],
    ) -> Result<(SinkPayload, Option<SourceCapabilities>), ParseError> {
        let parsed = match message_type {
            ExtendedMessageType::Status => {
                let status =
                    message::extended::status::Status::from_bytes(payload).ok_or(ParseError::InvalidLength {
                        expected: message::extended::status::Status::DATA_SIZE,
                        found: payload.len(),
                    })?;
                (SinkPayload::Status(status), None)
            }
            ExtendedMessageType::PpsStatus => {
                let status =
                    message::extended::pps_status::PpsStatus::from_bytes(payload).ok_or(ParseError::InvalidLength {
                        expected: message::extended::pps_status::PpsStatus::DATA_SIZE,
                        found: payload.len(),
                    })?;
                (SinkPayload::PpsStatus(status), None)
            }
            ExtendedMessageType::ExtendedControl => {
                if payload.len() != 2 {
                    return Err(ParseError::InvalidLength { expected: 2, found: payload.len() });
                }
                (SinkPayload::ExtendedControl(ExtendedControl::from_bytes(payload)), None)
            }
            ExtendedMessageType::EprSourceCapabilities => {
                use message::data::source_capabilities::MAX_EPR_SOURCE_PDOS;

                if !payload.len().is_multiple_of(4) {
                    return Err(ParseError::Other("EPR Source Capabilities contains a partial PDO"));
                }
                if payload.len() > MAX_EPR_SOURCE_PDOS * 4 {
                    return Err(ParseError::InvalidLength { expected: MAX_EPR_SOURCE_PDOS * 4, found: payload.len() });
                }

                let mut pdos = Vec::new();
                for bytes in payload.chunks_exact(4) {
                    pdos.push(LittleEndian::read_u32(bytes))
                        .map_err(|_| ParseError::Other("too many EPR Source Capability PDOs"))?;
                }
                (SinkPayload::SourceCapabilities, Some(SourceCapabilities(pdos)))
            }
            _ => (SinkPayload::Unknown, None),
        };
        Ok(parsed)
    }

    fn parse_sink_frame(header: Header, data: &[u8]) -> Result<(SinkMessage, Option<SourceCapabilities>), ParseError> {
        let payload = &data[MSG_HEADER_SIZE..];
        let (payload, capabilities) = match header.message_type() {
            MessageType::Control(_) => (SinkPayload::None, None),
            MessageType::Data(message_type) => match message_type {
                DataMessageType::SourceCapabilities => {
                    let pdos = payload
                        .chunks_exact(core::mem::size_of::<u32>())
                        .take(header.num_objects())
                        .map(LittleEndian::read_u32)
                        .collect();
                    (SinkPayload::SourceCapabilities, Some(SourceCapabilities(pdos)))
                }
                DataMessageType::EprMode if payload.len() == core::mem::size_of::<u32>() => {
                    (SinkPayload::EprMode(EprModeDataObject(LittleEndian::read_u32(payload))), None)
                }
                DataMessageType::SourceInfo => (
                    message::data::source_info::SourceInfo::from_bytes(payload, header.num_objects())
                        .map_or(SinkPayload::Unknown, SinkPayload::SourceInfo),
                    None,
                ),
                DataMessageType::Alert => (
                    message::data::alert::AlertDataObject::from_bytes(payload)
                        .map_or(SinkPayload::Unknown, SinkPayload::Alert),
                    None,
                ),
                _ => (SinkPayload::Unknown, None),
            },
            MessageType::Extended(message_type) => {
                let extended_header = message::extended::ExtendedHeader::from_bytes(payload);
                let data_size = extended_header.data_size() as usize;
                if payload.len() < EXT_HEADER_SIZE + data_size {
                    return Err(ParseError::InvalidLength {
                        expected: EXT_HEADER_SIZE + data_size,
                        found: payload.len(),
                    });
                }
                Self::parse_sink_extended_payload(message_type, &payload[EXT_HEADER_SIZE..EXT_HEADER_SIZE + data_size])?
            }
        };
        Ok((SinkMessage { header, payload }, capabilities))
    }

    /// Receive the subset of parsed message data consumed by a Sink.
    ///
    /// Large Source Capability lists remain in `sink_source_capabilities` and
    /// are taken synchronously by the policy engine after this future resolves.
    async fn receive_sink_message_inner(&mut self) -> Result<SinkMessage, RxError> {
        // A previous receive may have been cancelled while acknowledging a
        // frame. Capabilities are meaningful only alongside the marker
        // returned by this invocation.
        self.sink_source_capabilities = None;

        loop {
            let (header, length) = self.receive_frame().await?;
            let message_type = header.message_type();

            if let MessageType::Extended(extended_message_type) = message_type {
                let ext_header_end = MSG_HEADER_SIZE + EXT_HEADER_SIZE;
                let ext_header =
                    message::extended::ExtendedHeader::from_bytes(&self.rx_buffer[MSG_HEADER_SIZE..ext_header_end]);
                let payload_len = length - ext_header_end;
                let total_size = ext_header.data_size();
                let chunked = ext_header.chunked();
                let chunk_number = ext_header.chunk_number();

                self.default_header = self.default_header.with_spec_revision(header.spec_revision()?);

                if chunked {
                    if ext_header.request_chunk() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    if total_size as usize > self.extended_rx_buffer.capacity() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    trace!(
                        "Received chunked extended message {:?}, chunk {}, size {}",
                        message_type, chunk_number, payload_len
                    );

                    if self.handle_rx_ack(header).await? {
                        continue;
                    }

                    let (expected_total, expected_next) = match self.extended_rx_expected {
                        Some((ty, total, next)) if ty == extended_message_type && total == total_size => (total, next),
                        Some(_) => {
                            self.reset_chunked_rx();
                            return Err(RxError::UnsupportedMessage);
                        }
                        None if chunk_number == 0 => (total_size, 0),
                        None => {
                            self.reset_chunked_rx();
                            return Err(RxError::UnsupportedMessage);
                        }
                    };

                    if expected_next != 0 && chunk_number != expected_next {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }

                    if chunk_number == 0 || expected_next == 0 {
                        self.extended_rx_buffer.clear();
                        self.extended_rx_expected = Some((extended_message_type, total_size, 1));
                    } else {
                        self.extended_rx_expected = Some((extended_message_type, expected_total, expected_next + 1));
                    }

                    let remaining = (total_size as usize).saturating_sub(self.extended_rx_buffer.len());
                    let expected_payload_len = if remaining > message::extended::chunked::MAX_EXTENDED_MSG_CHUNK_LEN {
                        message::extended::chunked::MAX_EXTENDED_MSG_CHUNK_LEN
                    } else {
                        (EXT_HEADER_SIZE + remaining).div_ceil(4) * 4 - EXT_HEADER_SIZE
                    };
                    if payload_len != expected_payload_len {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    if self.extended_rx_buffer.len() + payload_len > self.extended_rx_buffer.capacity() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }
                    let payload = &self.rx_buffer[ext_header_end..length];
                    if self.extended_rx_buffer.extend_from_slice(payload).is_err() {
                        self.reset_chunked_rx();
                        return Err(RxError::UnsupportedMessage);
                    }

                    if self.extended_rx_buffer.len() < total_size as usize {
                        let next_chunk = self.extended_rx_expected.as_ref().map(|(_, _, next)| *next).unwrap_or(1);
                        self.transmit_chunk_request(extended_message_type, next_chunk).await?;
                        continue;
                    }

                    let parsed = Self::parse_sink_extended_payload(
                        extended_message_type,
                        &self.extended_rx_buffer[..total_size as usize],
                    );
                    self.reset_chunked_rx();
                    let (payload, capabilities) = parsed?;
                    self.sink_source_capabilities = capabilities;
                    let message = SinkMessage { header, payload };
                    trace!("Received assembled sink message {:?}", message);
                    return Ok(message);
                }
            }

            let parsed = Self::parse_sink_frame(header, &self.rx_buffer[..length]);
            let (message, capabilities) = parsed?;
            self.default_header = self.default_header.with_spec_revision(header.spec_revision()?);

            match message_type {
                MessageType::Control(ControlMessageType::Reserved) | MessageType::Data(DataMessageType::Reserved) => {
                    return Err(RxError::UnsupportedMessage);
                }
                _ => {}
            }

            self.sink_source_capabilities = capabilities;
            match self.handle_rx_ack(header).await {
                Ok(true) => {
                    self.sink_source_capabilities = None;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    self.sink_source_capabilities = None;
                    return Err(error);
                }
            }

            if matches!(message_type, MessageType::Control(ControlMessageType::SoftReset)) {
                self.sink_source_capabilities = None;
                return Err(RxError::SoftReset);
            }

            trace!("Received sink message {:?}", message);
            return Ok(message);
        }
    }

    /// Receive a message.
    #[cfg(any(feature = "source", test))]
    pub async fn receive_message(&mut self) -> Result<Message, ProtocolError> {
        #[cfg(not(feature = "numeric-trace"))]
        {
            self.receive_message_inner().await.map_err(|err| err.into())
        }
        #[cfg(feature = "numeric-trace")]
        {
            let result = self.receive_message_inner().await.map_err(|err| err.into());
            if let Err(error) = &result {
                emit_protocol_error(error);
            }
            result
        }
    }

    async fn receive_sink_message(&mut self) -> Result<SinkMessage, ProtocolError> {
        #[cfg(not(feature = "numeric-trace"))]
        {
            self.receive_sink_message_inner().await.map_err(Into::into)
        }
        #[cfg(feature = "numeric-trace")]
        {
            let result = self.receive_sink_message_inner().await.map_err(Into::into);
            if let Err(error) = &result {
                emit_protocol_error(error);
            }
            result
        }
    }

    fn take_sink_source_capabilities(&mut self) -> Option<SourceCapabilities> {
        self.sink_source_capabilities.take()
    }

    /// Updates the received message counter.
    ///
    /// If receiving the first message after protocol layer reset, copy its ID.
    /// Otherwise, compare the received ID with the stored ID. If they are equal, this is a retransmission.
    ///
    /// Returns `true`, if this was a retransmission.
    fn update_rx_message_counter(&mut self, header: Header) -> bool {
        match self.counters.rx_message.as_mut() {
            None => {
                trace!(
                    "Received first message after protocol layer reset with RX counter value: {}",
                    header.message_id()
                );
                self.counters.rx_message = Some(Counter::new_from_value(CounterType::MessageId, header.message_id()));
                false
            }
            Some(counter) => {
                if header.message_id() == counter.value() {
                    trace!("Received retransmission of RX counter value: {}", counter.value());
                    true
                } else {
                    counter.set(header.message_id());
                    false
                }
            }
        }
    }

    /// Wait until a message of one of the chosen types is received, or a timeout occurs.
    #[cfg(any(feature = "source", test))]
    pub fn receive_message_type<'a>(
        &'a mut self,
        message_types: &'a [MessageType],
        timer_type: TimerType,
    ) -> impl Future<Output = Result<Message, ProtocolError>> + 'a {
        self.receive_message_type_with_timeout(message_types, timer_type, None)
    }

    #[cfg(any(feature = "source", test))]
    async fn receive_message_type_with_timeout(
        &mut self,
        message_types: &[MessageType],
        timer_type: TimerType,
        milliseconds: Option<u32>,
    ) -> Result<Message, ProtocolError> {
        // GoodCrc message reception is handled separately.
        // See `wait_for_good_crc()` instead.
        for message_type in message_types {
            assert_ne!(*message_type, MessageType::Control(ControlMessageType::GoodCRC));
        }

        let timeout_fut = async move {
            match milliseconds {
                Some(milliseconds) => TIMER::after_millis(u64::from(milliseconds)).await,
                None => Self::get_timer(timer_type).await,
            }
        };
        let receive_fut = async {
            loop {
                match self.receive_message_inner().await {
                    Ok(message) => {
                        if matches!(message.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)) {
                            continue;
                        }
                        return if message_types.contains(&message.header.message_type()) {
                            Ok(message)
                        } else {
                            Err(ProtocolError::UnexpectedMessage)
                        };
                    }
                    Err(other) => return Err(other.into()),
                }
            }
        };

        #[cfg(not(feature = "numeric-trace"))]
        {
            match select(timeout_fut, receive_fut).await {
                Either::First(_) => Err(RxError::ReceiveTimeout.into()),
                Either::Second(receive_result) => receive_result,
            }
        }
        #[cfg(feature = "numeric-trace")]
        {
            let result = match select(timeout_fut, receive_fut).await {
                Either::First(_) => Err(RxError::ReceiveTimeout.into()),
                Either::Second(receive_result) => receive_result,
            };
            if let Err(error) = &result {
                emit_protocol_error(error);
            }
            result
        }
    }

    /// Perform a hard-reset procedure.
    ///
    // See spec, [6.7.1.1]
    pub async fn hard_reset(&mut self) -> Result<(), ProtocolError> {
        self.counters.tx_message.reset();
        self.counters.retry.reset();

        loop {
            match self.driver.transmit_hard_reset().await {
                Ok(_) | Err(DriverTxError::HardReset) => break,
                Err(DriverTxError::Detached) => return Err(TxError::Detached.into()),
                Err(DriverTxError::Discarded) => match self.counters.retry.increment() {
                    Ok(_) => {
                        numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                            crate::numeric_trace::NumericTraceEventKind::HardReset,
                            crate::numeric_trace::NumericTraceHardResetPhase::TransmitRetry as u8,
                            crate::numeric_trace::UNAVAILABLE_U8,
                            self.counters.retry.value(),
                            crate::numeric_trace::UNAVAILABLE_U16,
                            crate::numeric_trace::UNAVAILABLE_U16,
                        ));
                    }
                    Err(CounterError::Exceeded) => {
                        return Err(ProtocolError::TransmitRetriesExceeded(self.counters.retry.max_value()));
                    }
                },
            }
        }

        trace!("Performed hard reset");
        Ok(())
    }

    /// Wait for VBUS to be available.
    pub fn wait_for_vbus(&mut self) -> impl Future<Output = ()> + '_ {
        self.driver.wait_for_vbus()
    }

    /// Return whether the Sink may currently initiate an AMS.
    ///
    /// Rp-based collision avoidance was introduced by PD 3.0. Earlier Port
    /// Partners retain their ordinary Type-C current advertisement after the
    /// Explicit Contract, so interpreting 1.5 A Rp as SinkTxNG would block
    /// them indefinitely.
    pub fn sink_tx_ok(&mut self) -> bool {
        !matches!(self.default_header.spec_revision(), Ok(SpecificationRevision::R3_X)) || self.driver.sink_tx_ok()
    }

    /// Transmit a control message of the provided type.
    pub fn transmit_control_message(
        &mut self,
        message_type: ControlMessageType,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        let message = Message::new(Header::new_control(self.default_header, self.counters.tx_message, message_type));

        self.transmit(message)
    }

    /// Transmit an extended control message of the provided type.
    pub fn transmit_extended_control_message(
        &mut self,
        message_type: ExtendedControlMessageType,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        // Per USB PD spec 6.2.1.1.2: for extended messages, num_objects must be non-zero.
        // ExtendedControl = 2-byte extended header + 2-byte data = 4 bytes = 1 data object.
        let mut message = Message::new(Header::new_extended(
            self.default_header,
            self.counters.tx_message,
            ExtendedMessageType::ExtendedControl,
            1,
        ));

        message.payload = Some(Payload::Extended(Extended::ExtendedControl(
            message::extended::extended_control::ExtendedControl::default().with_message_type(message_type),
        )));

        self.transmit(message)
    }

    /// Transmit an EPR mode data message.
    pub fn transmit_epr_mode(
        &mut self,
        action: message::data::epr_mode::Action,
        data: u8,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        let header = Header::new_data(self.default_header, self.counters.tx_message, DataMessageType::EprMode, 1);

        let mdo = EprModeDataObject::default().with_action(action).with_data(data);

        self.transmit(Message::new_with_data(header, Data::EprMode(mdo)))
    }

    /// Transmit a chunk request message per USB PD spec 6.12.2.1.2.4.
    ///
    /// A chunk request is an extended message with:
    /// - The same message type as the chunked message being received
    /// - Extended header with: chunked=1, request_chunk=1, chunk_number=requested_chunk, data_size=0
    async fn transmit_chunk_request(
        &mut self,
        message_type: ExtendedMessageType,
        chunk_number: u8,
    ) -> Result<(), RxError> {
        trace!("Transmit chunk request for {:?} chunk {}", message_type, chunk_number);

        // Build extended header for chunk request
        let ext_header = message::extended::ExtendedHeader::default()
            .with_chunked(true)
            .with_request_chunk(true)
            .with_chunk_number(chunk_number);

        // Build message header - num_objects = 1 for the extended header word
        let header = Header::new_extended(self.default_header, self.counters.tx_message, message_type, 1);

        // Build message bytes manually
        let mut buffer = Self::get_message_buffer();
        let mut offset = header.to_bytes(&mut buffer);
        offset += ext_header.to_bytes(&mut buffer[offset..]);
        // Pad to 4-byte Data Object boundary per USB PD spec.
        // Extended header is 2 bytes, so add 2 bytes padding to complete the Data Object.
        // Buffer is already zeroed, so just advance offset.
        offset += 2;

        // Transmit and wait for GoodCRC
        numeric_trace!(crate::numeric_trace::NumericTraceEvent::from_frame(
            crate::numeric_trace::NumericTraceEventKind::TxStart,
            &buffer[..offset],
            self.counters.retry.value(),
        ));
        if DRIVER::HAS_AUTO_RETRY {
            #[cfg(feature = "numeric-trace")]
            emit_frame_event(
                crate::numeric_trace::NumericTraceEventKind::TxHardwareRetry,
                &buffer[..offset],
                self.counters.retry.max_value(),
                Some(crate::numeric_trace::NumericTracePath::Hardware as u8),
            );
            match self.driver.transmit(&buffer[..offset]).await {
                Ok(()) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_frame_event(
                        crate::numeric_trace::NumericTraceEventKind::GoodCrcReceived,
                        &buffer[..offset],
                        self.counters.retry.value(),
                        Some(crate::numeric_trace::NumericTracePath::Hardware as u8),
                    );
                    self.counters.retry.reset();
                    _ = self.counters.tx_message.increment();
                    #[cfg(feature = "numeric-trace")]
                    emit_frame_event(
                        crate::numeric_trace::NumericTraceEventKind::TxSuccess,
                        &buffer[..offset],
                        0,
                        Some(crate::numeric_trace::NumericTracePath::Hardware as u8),
                    );
                    Ok(())
                }
                Err(DriverTxError::HardReset) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..offset],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::HardReset,
                    );
                    Err(RxError::HardReset)
                }
                Err(DriverTxError::Detached) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..offset],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::Detached,
                    );
                    Err(RxError::Detached)
                }
                Err(DriverTxError::Discarded) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..offset],
                        self.counters.retry.max_value(),
                        crate::numeric_trace::NumericTraceTxReason::RetriesExceeded,
                    );
                    Err(RxError::ReceiveTimeout)
                }
            }
        } else {
            match self.transmit_inner(&buffer[..offset]).await {
                Ok(_) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_frame_event(
                        crate::numeric_trace::NumericTraceEventKind::GoodCrcWait,
                        &buffer[..offset],
                        self.counters.retry.value(),
                        Some(crate::numeric_trace::NumericTracePath::Software as u8),
                    );
                    let result = self.wait_for_good_crc().await;
                    if result.is_ok() {
                        #[cfg(feature = "numeric-trace")]
                        emit_frame_event(
                            crate::numeric_trace::NumericTraceEventKind::TxSuccess,
                            &buffer[..offset],
                            0,
                            Some(crate::numeric_trace::NumericTracePath::Software as u8),
                        );
                    }
                    #[cfg(feature = "numeric-trace")]
                    if let Err(error) = &result {
                        let reason = match error {
                            RxError::ReceiveTimeout => crate::numeric_trace::NumericTraceTxReason::GoodCrcTimeout,
                            RxError::AcknowledgeMismatch(_) => {
                                crate::numeric_trace::NumericTraceTxReason::AcknowledgeMismatch
                            }
                            RxError::HardReset => crate::numeric_trace::NumericTraceTxReason::HardReset,
                            RxError::Detached => crate::numeric_trace::NumericTraceTxReason::Detached,
                            _ => crate::numeric_trace::NumericTraceTxReason::Other,
                        };
                        emit_tx_failure(&buffer[..offset], self.counters.retry.value(), reason);
                    }
                    result
                }
                Err(TxError::HardReset) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..offset],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::HardReset,
                    );
                    Err(RxError::HardReset)
                }
                Err(TxError::Detached) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..offset],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::Detached,
                    );
                    Err(RxError::Detached)
                }
                Err(TxError::Discarded) => {
                    #[cfg(feature = "numeric-trace")]
                    emit_tx_failure(
                        &buffer[..offset],
                        self.counters.retry.value(),
                        crate::numeric_trace::NumericTraceTxReason::DriverDiscarded,
                    );
                    Err(RxError::ReceiveTimeout)
                }
            }
        }
    }

    /// Transmit sink capabilities in response to Get_Sink_Cap.
    ///
    /// Per USB PD Spec R3.2 Section 6.4.1.6, sinks respond to Get_Sink_Cap messages
    /// with a Sink_Capabilities message containing PDOs describing what power levels
    /// the sink can operate at.
    pub fn transmit_sink_capabilities(
        &mut self,
        capabilities: message::data::sink_capabilities::SinkCapabilities,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        let num_objects = capabilities.num_objects();
        let header = Header::new_data(
            self.default_header,
            self.counters.tx_message,
            DataMessageType::SinkCapabilities,
            num_objects,
        );

        self.transmit(Message::new_with_data(header, Data::SinkCapabilities(capabilities)))
    }

    /// Transmit EPR sink capabilities in response to EPR_Get_Sink_Cap.
    ///
    /// Per USB PD Spec R3.2 Section 8.3.3.3.10, sinks respond to EPR_Get_Sink_Cap
    /// messages with an EPR_Sink_Capabilities message.
    pub fn transmit_epr_sink_capabilities(
        &mut self,
        capabilities: message::data::sink_capabilities::SinkCapabilities,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        // Convert SinkCapabilities PDOs to the extended message format
        let pdos: heapless::Vec<_, 7> = capabilities.0.iter().cloned().collect();
        let extended_payload = message::extended::Extended::EprSinkCapabilities(pdos);

        let header = Header::new_extended(
            self.default_header,
            self.counters.tx_message,
            ExtendedMessageType::EprSinkCapabilities,
            0, // Message serialization derives the chunk's padded object count.
        );

        let mut message = Message::new(header);
        message.payload = Some(Payload::Extended(extended_payload));

        self.transmit(message)
    }

    /// Transmit the 24-byte Sink Capabilities Extended Data Block in response
    /// to `Get_Sink_Cap_Extended`.
    pub fn transmit_sink_capabilities_extended(
        &mut self,
        capabilities: SinkCapabilitiesExtended,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        let header = Header::new_extended(
            self.default_header,
            self.counters.tx_message,
            ExtendedMessageType::SinkCapabilitiesExtended,
            0, // Message serialization derives the chunk's padded object count.
        );
        let mut message = Message::new(header);
        message.payload = Some(Payload::Extended(Extended::SinkCapabilitiesExtended(capabilities)));
        self.transmit(message)
    }

    /// Transmit the device's Source Capabilities
    ///
    /// Could be sent from a Source or Dual Role Device
    #[cfg(any(feature = "source", test))]
    pub async fn transmit_source_capabilities(
        &mut self,
        source_capabilities: &SourceCapabilities,
    ) -> Result<(), ProtocolError> {
        // Only sources can send capabilities
        debug_assert!(matches!(self.default_header.port_power_role(), PowerRole::Source));
        if source_capabilities.has_epr_pdo_in_spr_positions() {
            return Err(ProtocolError::TxError(TxError::HardReset));
        }

        let header = Header::new_data(
            self.default_header,
            self.counters.tx_message,
            DataMessageType::SourceCapabilities,
            source_capabilities.0.len() as u8, // Raw cast OK since since len has domain of [0, 8]
        );

        let message = Message::new_with_data(header, Data::SourceCapabilities(source_capabilities.clone()));

        self.transmit(message).await
    }

    /// Transmit the device's EPR Source Capabilities
    ///
    /// Could be sent from a Source or Dual Role Device
    #[cfg(any(feature = "source", test))]
    pub async fn transmit_epr_source_capabilities(
        &mut self,
        source_capabilities: &SourceCapabilities,
    ) -> Result<(), ProtocolError> {
        debug_assert!(matches!(self.default_header.port_power_role(), PowerRole::Source));

        let pdos: heapless::Vec<_, { message::data::source_capabilities::MAX_EPR_SOURCE_PDOS }> =
            source_capabilities.0.iter().cloned().collect();
        let extended_payload = message::extended::Extended::EprSourceCapabilities(pdos);

        let header = Header::new_extended(
            self.default_header,
            self.counters.tx_message,
            ExtendedMessageType::EprSourceCapabilities,
            0, // Message serialization derives the chunk's padded object count.
        );

        let mut message = Message::new(header);
        message.payload = Some(Payload::Extended(extended_payload));

        self.transmit(message).await
    }
}

#[repr(transparent)]
#[derive(Debug)]
/// The USB PD Protocol Layer for a `Sink`
pub(crate) struct SinkProtocolLayer<DRIVER: Driver, TIMER: Timer>(ProtocolLayer<DRIVER, TIMER>);

impl<DRIVER: Driver, TIMER: Timer> core::ops::Deref for SinkProtocolLayer<DRIVER, TIMER> {
    type Target = ProtocolLayer<DRIVER, TIMER>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<DRIVER: Driver, TIMER: Timer> core::ops::DerefMut for SinkProtocolLayer<DRIVER, TIMER> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<DRIVER: Driver, TIMER: Timer> SinkProtocolLayer<DRIVER, TIMER> {
    /// Create a new protocol layer from a driver and default header.
    pub fn new(driver: DRIVER, default_header: Header) -> Self {
        Self(ProtocolLayer::new(driver, default_header))
    }

    /// Receive a compact message containing only fields consumed by the sink.
    pub fn receive_message(&mut self) -> impl Future<Output = Result<SinkMessage, ProtocolError>> + '_ {
        self.0.receive_sink_message()
    }

    /// Wait until one of the selected sink message types is received.
    pub fn receive_message_type<'a>(
        &'a mut self,
        message_types: &'a [MessageType],
        timer_type: TimerType,
    ) -> impl Future<Output = Result<SinkMessage, ProtocolError>> + 'a {
        self.receive_message_type_with_timeout(message_types, timer_type, None)
    }

    async fn receive_message_type_with_timeout(
        &mut self,
        message_types: &[MessageType],
        timer_type: TimerType,
        milliseconds: Option<u32>,
    ) -> Result<SinkMessage, ProtocolError> {
        for message_type in message_types {
            assert_ne!(*message_type, MessageType::Control(ControlMessageType::GoodCRC));
        }

        let timeout_fut = async move {
            match milliseconds {
                Some(milliseconds) => TIMER::after_millis(u64::from(milliseconds)).await,
                None => ProtocolLayer::<DRIVER, TIMER>::get_timer(timer_type).await,
            }
        };
        let receive_fut = async {
            loop {
                match self.0.receive_sink_message_inner().await {
                    Ok(message) => {
                        if matches!(message.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)) {
                            continue;
                        }
                        return if message_types.contains(&message.header.message_type()) {
                            Ok(message)
                        } else {
                            Err(ProtocolError::UnexpectedMessage)
                        };
                    }
                    Err(other) => return Err(other.into()),
                }
            }
        };

        #[cfg(not(feature = "numeric-trace"))]
        {
            match select(timeout_fut, receive_fut).await {
                Either::First(_) => Err(RxError::ReceiveTimeout.into()),
                Either::Second(receive_result) => receive_result,
            }
        }
        #[cfg(feature = "numeric-trace")]
        {
            let result = match select(timeout_fut, receive_fut).await {
                Either::First(_) => Err(RxError::ReceiveTimeout.into()),
                Either::Second(receive_result) => receive_result,
            };
            if let Err(error) = &result {
                emit_protocol_error(error);
            }
            result
        }
    }

    pub fn take_source_capabilities(&mut self) -> Option<SourceCapabilities> {
        self.0.take_sink_source_capabilities()
    }

    /// Wait for the source to provide its capabilities.
    pub fn wait_for_source_capabilities(
        &mut self,
        recovery_ms: Option<u32>,
    ) -> impl Future<Output = Result<SinkMessage, ProtocolError>> + '_ {
        // Only sinks can await capabilities.
        debug_assert!(matches!(self.default_header.port_power_role(), PowerRole::Sink));

        self.receive_message_type_with_timeout(&SOURCE_CAPABILITY_MESSAGE_TYPES, TimerType::SinkWaitCap, recovery_ms)
    }

    /// Request a certain power level from the source.
    pub fn request_power(
        &mut self,
        power_source_request: request::PowerSource,
    ) -> impl Future<Output = Result<(), ProtocolError>> + '_ {
        // Only sinks can request from a supply.
        debug_assert!(matches!(self.default_header.port_power_role(), PowerRole::Sink));

        let message_type = power_source_request.message_type();
        let num_objects = power_source_request.num_objects();
        let header = Header::new_data(self.default_header, self.counters.tx_message, message_type, num_objects);

        self.transmit(Message::new_with_data(header, Data::Request(power_source_request)))
    }
}

#[repr(transparent)]
#[derive(Debug)]
/// The USB PD Protocol Layer for a `Source`
#[cfg(any(feature = "source", test))]
pub(crate) struct SourceProtocolLayer<DRIVER: Driver, TIMER: Timer>(ProtocolLayer<DRIVER, TIMER>);

#[cfg(any(feature = "source", test))]
impl<DRIVER: Driver, TIMER: Timer> core::ops::Deref for SourceProtocolLayer<DRIVER, TIMER> {
    type Target = ProtocolLayer<DRIVER, TIMER>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(any(feature = "source", test))]
impl<DRIVER: Driver, TIMER: Timer> core::ops::DerefMut for SourceProtocolLayer<DRIVER, TIMER> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[cfg(any(feature = "source", test))]
impl<DRIVER: Driver, TIMER: Timer> SourceProtocolLayer<DRIVER, TIMER> {
    /// Create a new protocol layer from a driver and default header.
    pub fn new(driver: DRIVER, default_header: Header) -> Self {
        Self(ProtocolLayer::new(driver, default_header))
    }

    /// Wait for the sink to request a capability with a Request Message.
    pub async fn wait_for_request(&mut self) -> Result<Message, ProtocolError> {
        // Only sources await a sink power request
        debug_assert!(matches!(self.default_header.port_power_role(), PowerRole::Source));

        self.receive_message_type(
            &[MessageType::Data(message::header::DataMessageType::Request)],
            TimerType::SenderResponse,
        )
        .await
    }

    /// Wait for the sink to request a capability with an EPR_Request Message
    pub async fn wait_for_epr_request(&mut self) -> Result<Message, ProtocolError> {
        // Only sources await a sink power request
        debug_assert!(matches!(self.default_header.port_power_role(), PowerRole::Source));

        self.receive_message_type(
            &[MessageType::Data(message::header::DataMessageType::EprRequest)],
            TimerType::SenderResponse,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use byteorder::{ByteOrder, LittleEndian};

    use super::message::Message;
    use super::message::Payload;
    use super::message::data::Data;
    use super::message::data::request::{FixedVariableSupply, PowerSource};
    use super::message::extended::{Extended, ExtendedHeader};
    use super::message::header::{
        ControlMessageType, DataMessageType, ExtendedMessageType, Header, SpecificationRevision,
    };
    use super::{ProtocolError, ProtocolLayer, SinkPayload, SinkProtocolLayer, TxValidationError};
    use crate::counters::{Counter, CounterType};
    use crate::dummy::{
        DUMMY_CAPABILITIES, DummyDriver, DummyTimer, MAX_DATA_MESSAGE_SIZE, get_dummy_source_capabilities,
    };

    #[cfg(feature = "numeric-trace")]
    fn assert_event_kinds(
        events: &[crate::numeric_trace::NumericTraceEvent],
        expected: &[crate::numeric_trace::NumericTraceEventKind],
    ) {
        assert!(events.iter().map(|event| event.kind).eq(expected.iter().copied()));
    }

    fn get_protocol_layer() -> SinkProtocolLayer<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer> {
        SinkProtocolLayer(ProtocolLayer::new(
            DummyDriver::new(),
            Header::new_template(
                crate::DataRole::Ufp,
                crate::PowerRole::Sink,
                super::message::header::SpecificationRevision::R3_X,
            ),
        ))
    }

    #[tokio::test]
    async fn test_it() {
        let mut protocol_layer = get_protocol_layer();

        protocol_layer.driver.inject_received_data(&DUMMY_CAPABILITIES);
        let message = protocol_layer.receive_message().await.unwrap();

        if matches!(message.payload, SinkPayload::SourceCapabilities) {
            let capabilities = protocol_layer.take_source_capabilities().unwrap();
            for (cap, dummy_cap) in core::iter::zip(capabilities.0, get_dummy_source_capabilities()) {
                assert_eq!(cap, dummy_cap.to_raw());
            }
        } else {
            panic!()
        }
        assert!(protocol_layer.take_source_capabilities().is_none());
    }

    #[tokio::test]
    async fn compact_capability_storage_is_one_shot_and_cancellation_safe() {
        use core::future::Future;
        use core::task::{Context, Poll, Waker};

        let mut protocol_layer = get_protocol_layer();
        protocol_layer.driver.inject_received_data(&DUMMY_CAPABILITIES);
        let message = protocol_layer.receive_message().await.unwrap();
        assert_eq!(message.payload, SinkPayload::SourceCapabilities);

        // Starting a new receive invalidates an untaken capability marker. A
        // Ready-state select may cancel that receive before another frame
        // arrives, so cleanup cannot depend on successful completion.
        {
            let mut pending_receive = Box::pin(protocol_layer.receive_message());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(pending_receive.as_mut().poll(&mut context), Poll::Pending));
        }
        assert!(protocol_layer.take_source_capabilities().is_none());

        // A retransmitted capability frame must not leak its list into the
        // following non-capability result.
        protocol_layer.driver.inject_received_data(&DUMMY_CAPABILITIES);
        let accept = Message::new(Header::new_control(
            source_header(),
            Counter::new_from_value(CounterType::MessageId, 1),
            ControlMessageType::Accept,
        ));
        let mut accept_frame = [0; MAX_DATA_MESSAGE_SIZE];
        let accept_length = accept.to_bytes(&mut accept_frame);
        protocol_layer.driver.inject_received_data(&accept_frame[..accept_length]);
        let message = protocol_layer.receive_message().await.unwrap();
        assert_eq!(message.payload, SinkPayload::None);
        assert!(protocol_layer.take_source_capabilities().is_none());

        protocol_layer.driver.inject_received_data(&DUMMY_CAPABILITIES);
        let message = protocol_layer.receive_message().await.unwrap();
        assert_eq!(message.payload, SinkPayload::SourceCapabilities);
        protocol_layer.reset();
        assert!(protocol_layer.take_source_capabilities().is_none());
    }

    fn source_header() -> Header {
        Header::new_template(crate::DataRole::Dfp, crate::PowerRole::Source, SpecificationRevision::R3_X)
    }

    fn data_frame(message_type: DataMessageType, payload: &[u8]) -> ([u8; MAX_DATA_MESSAGE_SIZE], usize) {
        assert!(payload.len().is_multiple_of(4));
        let mut frame = [0; MAX_DATA_MESSAGE_SIZE];
        let header = Header::new_data(
            source_header(),
            Counter::new_from_value(CounterType::MessageId, 3),
            message_type,
            (payload.len() / 4) as u8,
        );
        header.to_bytes(&mut frame);
        frame[2..2 + payload.len()].copy_from_slice(payload);
        (frame, 2 + payload.len())
    }

    fn extended_frame(message_type: ExtendedMessageType, payload: &[u8]) -> ([u8; MAX_DATA_MESSAGE_SIZE], usize) {
        let body_size = 2 + payload.len();
        let object_count = body_size.div_ceil(4);
        let length = 2 + object_count * 4;
        let mut frame = [0; MAX_DATA_MESSAGE_SIZE];
        let header = Header::new_extended(
            source_header(),
            Counter::new_from_value(CounterType::MessageId, 5),
            message_type,
            object_count as u8,
        );
        header.to_bytes(&mut frame);
        ExtendedHeader::new(payload.len() as u16).with_chunked(true).to_bytes(&mut frame[2..]);
        frame[4..4 + payload.len()].copy_from_slice(payload);
        (frame, length)
    }

    fn assert_compact_sink_parser_matches_general(frame: &[u8]) {
        let general = Message::from_bytes(frame).unwrap();
        let header = Header::from_bytes(&frame[..2]).unwrap();
        let (compact, capabilities) =
            ProtocolLayer::<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer>::parse_sink_frame(header, frame).unwrap();
        assert_eq!(compact.header, general.header);

        match (general.payload, compact.payload) {
            (None, SinkPayload::None) => assert!(capabilities.is_none()),
            (Some(Payload::Data(Data::SourceCapabilities(expected))), SinkPayload::SourceCapabilities) => {
                assert_eq!(capabilities.unwrap().0, expected.0);
            }
            (Some(Payload::Data(Data::EprMode(expected))), SinkPayload::EprMode(actual)) => {
                assert_eq!(actual, expected);
                assert!(capabilities.is_none());
            }
            (Some(Payload::Data(Data::SourceInfo(expected))), SinkPayload::SourceInfo(actual)) => {
                assert_eq!(actual, expected);
                assert!(capabilities.is_none());
            }
            (Some(Payload::Data(Data::Alert(expected))), SinkPayload::Alert(actual)) => {
                assert_eq!(actual, expected);
                assert!(capabilities.is_none());
            }
            (Some(Payload::Data(Data::Unknown)), SinkPayload::Unknown)
            | (Some(Payload::Extended(Extended::Unknown)), SinkPayload::Unknown) => {
                assert!(capabilities.is_none());
            }
            (Some(Payload::Extended(Extended::Status(expected))), SinkPayload::Status(actual)) => {
                assert_eq!(actual, expected);
                assert!(capabilities.is_none());
            }
            (Some(Payload::Extended(Extended::PpsStatus(expected))), SinkPayload::PpsStatus(actual)) => {
                assert_eq!(actual, expected);
                assert!(capabilities.is_none());
            }
            (Some(Payload::Extended(Extended::ExtendedControl(expected))), SinkPayload::ExtendedControl(actual)) => {
                assert_eq!(actual, expected);
                assert!(capabilities.is_none());
            }
            (expected, actual) => panic!("generic payload {expected:?} did not match compact payload {actual:?}"),
        }
    }

    #[test]
    fn compact_sink_parser_matches_general_parser_for_consumed_messages() {
        let accept = Message::new(Header::new_control(
            source_header(),
            Counter::new_from_value(CounterType::MessageId, 1),
            ControlMessageType::Accept,
        ));
        let mut control = [0; MAX_DATA_MESSAGE_SIZE];
        let length = accept.to_bytes(&mut control);
        assert_compact_sink_parser_matches_general(&control[..length]);

        assert_compact_sink_parser_matches_general(&DUMMY_CAPABILITIES);

        for (message_type, payload) in [
            (DataMessageType::EprMode, [0, 0, 0x8c, 0x03].as_slice()),
            (DataMessageType::SourceInfo, [0xf0, 0x96, 0xf0, 0x00].as_slice()),
            (DataMessageType::Alert, [0x05, 0, 0, 0xfc].as_slice()),
            // Invalid EPR Mode length follows the general parser's Unknown path.
            (DataMessageType::EprMode, [0; 8].as_slice()),
            // This sink deliberately treats VDM content as opaque/unknown.
            (DataMessageType::VendorDefined, [0x44, 0x33, 0x22, 0x11].as_slice()),
        ] {
            let (frame, length) = data_frame(message_type, payload);
            assert_compact_sink_parser_matches_general(&frame[..length]);
        }

        for (message_type, payload) in [
            (ExtendedMessageType::Status, [1, 2, 3, 4, 5, 6, 7].as_slice()),
            (ExtendedMessageType::PpsStatus, [0x80, 0x02, 0x3c, 0x08].as_slice()),
            (ExtendedMessageType::ExtendedControl, [4, 0].as_slice()),
            (ExtendedMessageType::ManufacturerInfo, [1, 2, 3].as_slice()),
        ] {
            let (frame, length) = extended_frame(message_type, payload);
            assert_compact_sink_parser_matches_general(&frame[..length]);
        }
    }

    #[test]
    fn compact_sink_extended_parser_matches_general_epr_capabilities_and_errors() {
        let raw_pdos = [
            0x0a91_912c,
            0x0012_d12c,
            0x0013_c12c,
            0x0014_b12c,
            0x0016_41f4,
            0xc9a4_3264,
            0,
            0x0018_c1f4,
            0x001b_41f4,
            0x001f_01f4,
            0xd7c0_96f0,
        ];
        let mut payload = [0; 44];
        for (index, raw) in raw_pdos.into_iter().enumerate() {
            LittleEndian::write_u32(&mut payload[index * 4..], raw);
        }

        let general = Message::parse_extended_payload(ExtendedMessageType::EprSourceCapabilities, &payload).unwrap();
        let (compact, capabilities) =
            ProtocolLayer::<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer>::parse_sink_extended_payload(
                ExtendedMessageType::EprSourceCapabilities,
                &payload,
            )
            .unwrap();
        let Extended::EprSourceCapabilities(expected) = general else { panic!() };
        assert_eq!(compact, SinkPayload::SourceCapabilities);
        assert_eq!(capabilities.unwrap().0, expected);

        for (message_type, malformed) in [
            (ExtendedMessageType::Status, [0; 6].as_slice()),
            (ExtendedMessageType::ExtendedControl, [0; 1].as_slice()),
            (ExtendedMessageType::EprSourceCapabilities, [0; 3].as_slice()),
        ] {
            let general = Message::parse_extended_payload(message_type, malformed).unwrap_err();
            let compact = ProtocolLayer::<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer>::parse_sink_extended_payload(
                message_type,
                malformed,
            )
            .unwrap_err();
            assert_eq!(compact, general);
        }
    }

    #[test]
    fn every_local_tx_validation_error_has_a_distinct_protocol_class() {
        for expected in [
            TxValidationError::UnchunkedExtendedMessagesNotSupported,
            TxValidationError::AvsVoltageAlignmentInvalid,
            TxValidationError::ExtendedMessageChunkingRequired,
        ] {
            let ProtocolError::TxValidation(actual) = ProtocolError::from(expected) else {
                panic!("local TX validation was classified as a wire error")
            };
            assert_eq!(actual, expected);
        }
    }

    #[tokio::test]
    async fn invalid_local_request_never_reaches_the_driver() {
        #[cfg(feature = "numeric-trace")]
        let capture = crate::numeric_trace::test_support::CaptureGuard::start();
        let mut protocol_layer = get_protocol_layer();
        let invalid_request = PowerSource::FixedVariableSupply(FixedVariableSupply((1 << 28) | (1 << 23)));

        assert!(matches!(
            protocol_layer.request_power(invalid_request).await,
            Err(ProtocolError::TxValidation(TxValidationError::UnchunkedExtendedMessagesNotSupported))
        ));
        assert!(!protocol_layer.driver().has_transmitted_data());
        #[cfg(feature = "numeric-trace")]
        {
            let events = capture.events();
            assert_event_kinds(&events, &[crate::numeric_trace::NumericTraceEventKind::ProtocolError]);
            assert_eq!(events[0].code, crate::numeric_trace::NumericTraceProtocolError::TxValidation as u8);
            assert_eq!(events[0].detail, 1);
        }
    }

    #[cfg(feature = "numeric-trace")]
    #[tokio::test]
    async fn numeric_trace_orders_software_tx_and_good_crc() {
        use crate::counters::{Counter, CounterType};
        use crate::numeric_trace::{
            NumericTraceEventKind as Kind, NumericTracePath, UNAVAILABLE_U16, test_support::CaptureGuard,
        };
        use crate::protocol_layer::message::Message;
        use crate::protocol_layer::message::header::ControlMessageType;

        let mut protocol_layer = get_protocol_layer();
        let good_crc = Message::new(Header::new_control(
            *protocol_layer.header(),
            Counter::new_from_value(CounterType::MessageId, 0),
            ControlMessageType::GoodCRC,
        ));
        let mut buffer = [0; MAX_DATA_MESSAGE_SIZE];
        let length = good_crc.to_bytes(&mut buffer);
        protocol_layer.driver.inject_received_data(&buffer[..length]);

        let capture = CaptureGuard::start();
        protocol_layer.transmit_control_message(ControlMessageType::GetSourceCap).await.unwrap();
        let events = capture.events();

        assert_event_kinds(
            &events,
            &[Kind::TxStart, Kind::GoodCrcWait, Kind::RxMessage, Kind::GoodCrcReceived, Kind::TxSuccess],
        );
        assert_eq!(events[0].header & 0x1f, ControlMessageType::GetSourceCap as u16);
        assert_eq!(events[0].message_id, 0);
        assert_eq!(events[0].counter, 0);
        assert_eq!(events[0].detail, UNAVAILABLE_U16);
        assert_eq!(events[3].code, NumericTracePath::Software as u8);
        assert_eq!(events[3].message_id, 0);
        assert_eq!(events[4].code, NumericTracePath::Software as u8);
    }

    #[cfg(feature = "numeric-trace")]
    struct AutoRetryDriver {
        outcome: Result<(), usbpd_traits::DriverTxError>,
    }

    #[cfg(feature = "numeric-trace")]
    impl usbpd_traits::Driver for AutoRetryDriver {
        const HAS_AUTO_GOOD_CRC: bool = true;
        const HAS_AUTO_RETRY: bool = true;

        async fn wait_for_vbus(&mut self) {}

        async fn receive(&mut self, _buffer: &mut [u8]) -> Result<usize, usbpd_traits::DriverRxError> {
            core::future::pending().await
        }

        async fn transmit(&mut self, _data: &[u8]) -> Result<(), usbpd_traits::DriverTxError> {
            self.outcome
        }

        async fn transmit_hard_reset(&mut self) -> Result<(), usbpd_traits::DriverTxError> {
            Ok(())
        }
    }

    #[cfg(feature = "numeric-trace")]
    fn auto_retry_protocol_layer(
        outcome: Result<(), usbpd_traits::DriverTxError>,
    ) -> SinkProtocolLayer<AutoRetryDriver, DummyTimer> {
        SinkProtocolLayer(ProtocolLayer::new(
            AutoRetryDriver { outcome },
            Header::new_template(
                crate::DataRole::Ufp,
                crate::PowerRole::Sink,
                super::message::header::SpecificationRevision::R3_X,
            ),
        ))
    }

    #[cfg(feature = "numeric-trace")]
    #[tokio::test]
    async fn numeric_trace_reports_hardware_retry_success_and_failure() {
        use crate::numeric_trace::{
            NumericTraceEventKind as Kind, NumericTracePath, NumericTraceProtocolError, NumericTraceTxReason,
            test_support::CaptureGuard,
        };
        use crate::protocol_layer::message::header::ControlMessageType;
        use usbpd_traits::DriverTxError;

        {
            let capture = CaptureGuard::start();
            auto_retry_protocol_layer(Ok(())).transmit_control_message(ControlMessageType::GetSourceCap).await.unwrap();
            let events = capture.events();
            assert_event_kinds(
                &events,
                &[Kind::TxStart, Kind::TxHardwareRetry, Kind::GoodCrcReceived, Kind::TxSuccess],
            );
            assert_eq!(events[1].code, NumericTracePath::Hardware as u8);
            assert_eq!(events[2].code, NumericTracePath::Hardware as u8);
            assert_eq!(events[3].code, NumericTracePath::Hardware as u8);
        }

        {
            let capture = CaptureGuard::start();
            let error = auto_retry_protocol_layer(Err(DriverTxError::Discarded))
                .transmit_control_message(ControlMessageType::GetSourceCap)
                .await;
            assert!(matches!(error, Err(ProtocolError::TransmitRetriesExceeded(_))));
            let events = capture.events();
            assert_event_kinds(&events, &[Kind::TxStart, Kind::TxHardwareRetry, Kind::TxFailure, Kind::ProtocolError]);
            assert_eq!(events[2].code, NumericTraceTxReason::RetriesExceeded as u8);
            assert_eq!(events[3].code, NumericTraceProtocolError::TxRetriesExceeded as u8);
        }
    }
}
