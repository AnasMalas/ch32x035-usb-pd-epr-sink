use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::data::Data;
use usbpd::protocol_layer::message::data::epr_mode::{Action, DataEnterFailed};
use usbpd::protocol_layer::message::extended::Extended;
use usbpd::protocol_layer::message::extended::extended_control::ExtendedControlMessageType;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::protocol_layer::message::{Message, ParseError, Payload};
use usbpd::sink::device_policy_manager::DevicePolicyManager;
use usbpd::sink::policy_engine::{Error as SinkError, Sink};
use usbpd::timers::Timer;
use usbpd::{DataRole, PowerRole};
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

struct NeverTimer;

impl Timer for NeverTimer {
    async fn after_millis(_milliseconds: u64) {
        pending().await
    }
}

fn source_header(message_type: MessageType, num_objects: u8) -> Header {
    let raw_type = match message_type {
        MessageType::Control(value) => value as u8,
        MessageType::Data(value) => value as u8,
        MessageType::Extended(value) => value as u8,
    };
    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_type_raw(raw_type)
        .with_num_objects(num_objects)
        .with_extended(matches!(message_type, MessageType::Extended(_)))
}

fn frame(header: Header, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 2 + body.len()];
    header.to_bytes(&mut bytes[..2]);
    bytes[2..].copy_from_slice(body);
    bytes
}

#[test]
fn malformed_lengths_and_extended_payloads_return_errors() {
    assert!(matches!(Message::from_bytes(&[]), Err(ParseError::InvalidLength { .. })));
    assert!(matches!(Message::from_bytes(&[0]), Err(ParseError::InvalidLength { .. })));

    let truncated_source_caps = frame(source_header(MessageType::Data(DataMessageType::SourceCapabilities), 1), &[]);
    assert!(matches!(
        Message::from_bytes(&truncated_source_caps),
        Err(ParseError::InvalidLength { expected: 6, found: 2 })
    ));

    let zero_object_extended =
        frame(source_header(MessageType::Extended(ExtendedMessageType::ExtendedControl), 0), &[]);
    assert!(matches!(Message::from_bytes(&zero_object_extended), Err(ParseError::Other(_))));

    let one_byte_extended_control =
        frame(source_header(MessageType::Extended(ExtendedMessageType::ExtendedControl), 1), &[0x01, 0x80, 0xff, 0x00]);
    assert!(matches!(
        Message::from_bytes(&one_byte_extended_control),
        Err(ParseError::InvalidLength { expected: 2, found: 1 })
    ));

    assert!(matches!(
        Message::parse_extended_payload(ExtendedMessageType::EprSourceCapabilities, &[0; 5]),
        Err(ParseError::Other(_))
    ));
    assert!(matches!(
        Message::parse_extended_payload(ExtendedMessageType::EprSourceCapabilities, &[0; 48]),
        Err(ParseError::InvalidLength { expected: 44, found: 48 })
    ));
}

#[test]
fn reserved_epr_values_parse_without_panicking() {
    let unknown_control =
        frame(source_header(MessageType::Extended(ExtendedMessageType::ExtendedControl), 1), &[0x02, 0x80, 0xff, 0x00]);
    let parsed = Message::from_bytes(&unknown_control).unwrap();
    let Some(Payload::Extended(Extended::ExtendedControl(control))) = parsed.payload else {
        panic!("expected Extended Control payload");
    };
    assert_eq!(control.message_type(), ExtendedControlMessageType::Unknown);

    let unknown_epr_action =
        frame(source_header(MessageType::Data(DataMessageType::EprMode), 1), &(0xff_u32 << 24).to_le_bytes());
    let parsed = Message::from_bytes(&unknown_epr_action).unwrap();
    let Some(Payload::Data(Data::EprMode(mode))) = parsed.payload else {
        panic!("expected EPR Mode payload");
    };
    assert_eq!(mode.action(), Action::Unknown);
    assert!(matches!(DataEnterFailed::from(0xff), DataEnterFailed::UnknownValue));
}

struct MalformedDriver {
    reported_length: usize,
    bytes: Option<Vec<u8>>,
    served: bool,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
    hard_reset: Arc<AtomicBool>,
}

impl Driver for MalformedDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        if self.served {
            return Err(DriverRxError::Detached);
        }
        self.served = true;
        if let Some(bytes) = self.bytes.take() {
            buffer[..bytes.len()].copy_from_slice(&bytes);
        }
        Ok(self.reported_length)
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        self.transmitted.lock().unwrap().push(data.to_vec());
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        self.hard_reset.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct RecoveryDpm {
    detached: Arc<AtomicBool>,
    protocol_lost: Arc<AtomicBool>,
}

impl DevicePolicyManager for RecoveryDpm {
    async fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    async fn protocol_lost(&mut self) {
        self.protocol_lost.store(true, Ordering::SeqCst);
    }
}

fn run_malformed_driver(reported_length: usize, bytes: Option<Vec<u8>>) {
    let detached = Arc::new(AtomicBool::new(false));
    let protocol_lost = Arc::new(AtomicBool::new(false));
    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let driver = MalformedDriver {
        reported_length,
        bytes,
        served: false,
        transmitted: Arc::clone(&transmitted),
        hard_reset: Arc::new(AtomicBool::new(false)),
    };
    let dpm = RecoveryDpm { detached: Arc::clone(&detached), protocol_lost: Arc::clone(&protocol_lost) };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert!(!protocol_lost.load(Ordering::SeqCst));

    let transmitted = transmitted.lock().unwrap();
    assert_eq!(transmitted.len(), 1, "malformed input should trigger exactly one recovery message");
    let header = Header::from_bytes(&transmitted[0][..2]).unwrap();
    assert_eq!(header.message_type(), MessageType::Control(ControlMessageType::SoftReset));
}

#[test]
fn real_policy_engine_soft_resets_after_a_truncated_frame() {
    let header = frame(source_header(MessageType::Data(DataMessageType::SourceCapabilities), 1), &[]);
    run_malformed_driver(header.len(), Some(header));
}

#[test]
fn real_policy_engine_rejects_an_impossible_driver_length() {
    run_malformed_driver(31, None);
}

#[test]
fn real_policy_engine_rejects_a_short_nonfinal_extended_chunk() {
    let malformed_chunk = frame(
        source_header(MessageType::Extended(ExtendedMessageType::EprSourceCapabilities), 2),
        // Total DataSize is 44 bytes, so chunk 0 must carry the full 26-byte
        // non-final payload rather than these six bytes.
        &[0x2c, 0x80, 0, 0, 0, 0, 0, 0],
    );
    run_malformed_driver(malformed_chunk.len(), Some(malformed_chunk));
}

#[test]
fn real_policy_engine_hard_resets_an_invalid_mandatory_five_volt_pdo() {
    let detached = Arc::new(AtomicBool::new(false));
    let protocol_lost = Arc::new(AtomicBool::new(false));
    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let hard_reset = Arc::new(AtomicBool::new(false));
    let fixed_20v_3a = (400_u32 << 10) | 300;
    let invalid_capabilities =
        frame(source_header(MessageType::Data(DataMessageType::SourceCapabilities), 1), &fixed_20v_3a.to_le_bytes());
    let driver = MalformedDriver {
        reported_length: invalid_capabilities.len(),
        bytes: Some(invalid_capabilities),
        served: false,
        transmitted: Arc::clone(&transmitted),
        hard_reset: Arc::clone(&hard_reset),
    };
    let dpm = RecoveryDpm { detached: Arc::clone(&detached), protocol_lost: Arc::clone(&protocol_lost) };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert!(!protocol_lost.load(Ordering::SeqCst));
    assert!(hard_reset.load(Ordering::SeqCst));
    assert!(transmitted.lock().unwrap().is_empty());
}
