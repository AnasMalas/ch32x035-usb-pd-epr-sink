use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::data::source_info::SourceInfo;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event};
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

static SENDER_RESPONSE_CALLS: AtomicUsize = AtomicUsize::new(0);
static SOURCE_INFO_TIMEOUT_FIRED: AtomicBool = AtomicBool::new(false);

struct SourceInfoTimeoutTimer;

impl Timer for SourceInfoTimeoutTimer {
    async fn after_millis(milliseconds: u64) {
        if milliseconds == 30 {
            let call = SENDER_RESPONSE_CALLS.fetch_add(1, Ordering::SeqCst);
            if call >= 1 {
                SOURCE_INFO_TIMEOUT_FIRED.store(true, Ordering::SeqCst);
                return;
            }
        }
        pending().await
    }
}

fn source_header(message_id: u8, message_type: MessageType, num_objects: u8) -> Header {
    let raw_type = match message_type {
        MessageType::Control(message_type) => message_type as u8,
        MessageType::Data(message_type) => message_type as u8,
        MessageType::Extended(message_type) => message_type as u8,
    };
    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_id(message_id)
        .with_message_type_raw(raw_type)
        .with_num_objects(num_objects)
        .with_extended(matches!(message_type, MessageType::Extended(_)))
}

fn source_control(message_id: u8, message_type: ControlMessageType) -> Vec<u8> {
    let mut bytes = vec![0; 2];
    source_header(message_id, MessageType::Control(message_type), 0).to_bytes(&mut bytes);
    bytes
}

fn source_data(message_id: u8, message_type: DataMessageType, objects: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; 2 + objects.len() * 4];
    source_header(message_id, MessageType::Data(message_type), objects.len() as u8).to_bytes(&mut bytes[..2]);
    for (destination, object) in bytes[2..].chunks_exact_mut(4).zip(objects) {
        destination.copy_from_slice(&object.to_le_bytes());
    }
    bytes
}

struct ScriptedDriver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
    silence_until_source_info_timeout: bool,
}

impl Driver for ScriptedDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let Some((required_transmits, _)) = self.receive.front() else {
            if self.silence_until_source_info_timeout && !SOURCE_INFO_TIMEOUT_FIRED.load(Ordering::SeqCst) {
                pending().await
            }
            return Err(DriverRxError::Detached);
        };
        if self.transmitted.lock().unwrap().len() < *required_transmits {
            pending().await
        }
        let (_, message) = self.receive.pop_front().unwrap();
        buffer[..message.len()].copy_from_slice(&message);
        Ok(message.len())
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        self.transmitted.lock().unwrap().push(data.to_vec());
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        panic!("Source_Info query must not hard reset")
    }
}

struct SourceInfoDpm {
    transitions: Arc<AtomicUsize>,
    requested: bool,
    present_pdp: Arc<AtomicUsize>,
    second_object_seen: Arc<AtomicBool>,
}

impl DevicePolicyManager for SourceInfoDpm {
    async fn transition_power(&mut self, _accepted: &usbpd::protocol_layer::message::data::request::PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    async fn inform_source_info(&mut self, source_info: &SourceInfo) {
        self.present_pdp.store(source_info.port_present_pdp_watts() as usize, Ordering::SeqCst);
        self.second_object_seen.store(source_info.object2.is_some(), Ordering::SeqCst);
    }

    fn get_event(
        &mut self,
        _source_capabilities: &usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities,
    ) -> impl Future<Output = Event> {
        let event = if !self.requested && self.transitions.load(Ordering::SeqCst) == 1 {
            self.requested = true;
            Some(Event::RequestSourceInfo)
        } else {
            None
        };
        async move {
            match event {
                Some(event) => event,
                None => pending().await,
            }
        }
    }
}

#[test]
fn source_info_query_refines_present_power_without_disturbing_the_contract() {
    let fixed_5v = ((5_000 / 50) << 10) | (3_000 / 10);
    let source_info_1 = (1 << 31) | (140 << 16) | (100 << 8) | 100;
    let source_info_2 = (1 << 31) | (280 << 9) | 200;
    let receive = VecDeque::from([
        (0, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_data(3, DataMessageType::SourceInfo, &[source_info_1, source_info_2])),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let transitions = Arc::new(AtomicUsize::new(0));
    let present_pdp = Arc::new(AtomicUsize::new(0));
    let second_object_seen = Arc::new(AtomicBool::new(false));
    let driver =
        ScriptedDriver { receive, transmitted: Arc::clone(&transmitted), silence_until_source_info_timeout: false };
    let dpm = SourceInfoDpm {
        transitions: Arc::clone(&transitions),
        requested: false,
        present_pdp: Arc::clone(&present_pdp),
        second_object_seen: Arc::clone(&second_object_seen),
    };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert_eq!(transitions.load(Ordering::SeqCst), 1);
    assert_eq!(present_pdp.load(Ordering::SeqCst), 100);
    assert!(second_object_seen.load(Ordering::SeqCst));

    let transmitted = transmitted.lock().unwrap();
    let message_types: Vec<MessageType> =
        transmitted.iter().map(|message| Header::from_bytes(&message[..2]).unwrap().message_type()).collect();
    assert_eq!(
        message_types,
        [MessageType::Data(DataMessageType::Request), MessageType::Control(ControlMessageType::GetSourceInfo)]
    );
}

fn assert_source_info_refusal_preserves_contract(response: ControlMessageType) {
    let fixed_5v = ((5_000 / 50) << 10) | (3_000 / 10);
    let receive = VecDeque::from([
        (0, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_control(3, response)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let transitions = Arc::new(AtomicUsize::new(0));
    let present_pdp = Arc::new(AtomicUsize::new(0));
    let second_object_seen = Arc::new(AtomicBool::new(false));
    let driver =
        ScriptedDriver { receive, transmitted: Arc::clone(&transmitted), silence_until_source_info_timeout: false };
    let dpm = SourceInfoDpm {
        transitions: Arc::clone(&transitions),
        requested: false,
        present_pdp: Arc::clone(&present_pdp),
        second_object_seen: Arc::clone(&second_object_seen),
    };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert_eq!(transitions.load(Ordering::SeqCst), 1, "optional refusal must retain the 5 V contract");
    assert_eq!(present_pdp.load(Ordering::SeqCst), 0);
    assert!(!second_object_seen.load(Ordering::SeqCst));

    let transmitted = transmitted.lock().unwrap();
    let message_types: Vec<MessageType> =
        transmitted.iter().map(|message| Header::from_bytes(&message[..2]).unwrap().message_type()).collect();
    assert_eq!(
        message_types,
        [MessageType::Data(DataMessageType::Request), MessageType::Control(ControlMessageType::GetSourceInfo)]
    );
}

#[test]
fn source_info_refusal_or_deferral_preserves_the_existing_contract() {
    for response in [ControlMessageType::NotSupported, ControlMessageType::Reject, ControlMessageType::Wait] {
        assert_source_info_refusal_preserves_contract(response);
    }
}

#[test]
fn source_info_timeout_preserves_the_existing_contract() {
    SENDER_RESPONSE_CALLS.store(0, Ordering::SeqCst);
    SOURCE_INFO_TIMEOUT_FIRED.store(false, Ordering::SeqCst);

    let fixed_5v = ((5_000 / 50) << 10) | (3_000 / 10);
    let receive = VecDeque::from([
        (0, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let transitions = Arc::new(AtomicUsize::new(0));
    let present_pdp = Arc::new(AtomicUsize::new(0));
    let second_object_seen = Arc::new(AtomicBool::new(false));
    let driver =
        ScriptedDriver { receive, transmitted: Arc::clone(&transmitted), silence_until_source_info_timeout: true };
    let dpm = SourceInfoDpm {
        transitions: Arc::clone(&transitions),
        requested: false,
        present_pdp: Arc::clone(&present_pdp),
        second_object_seen: Arc::clone(&second_object_seen),
    };
    let mut sink: Sink<_, SourceInfoTimeoutTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(SOURCE_INFO_TIMEOUT_FIRED.load(Ordering::SeqCst));
    assert_eq!(transitions.load(Ordering::SeqCst), 1, "optional timeout must retain the 5 V contract");
    assert_eq!(present_pdp.load(Ordering::SeqCst), 0);
    assert!(!second_object_seen.load(Ordering::SeqCst));

    let transmitted = transmitted.lock().unwrap();
    let message_types: Vec<MessageType> =
        transmitted.iter().map(|message| Header::from_bytes(&message[..2]).unwrap().message_type()).collect();
    assert_eq!(
        message_types,
        [MessageType::Data(DataMessageType::Request), MessageType::Control(ControlMessageType::GetSourceInfo)]
    );
}

#[test]
fn source_info_parser_rejects_truncated_or_extra_objects() {
    assert!(SourceInfo::from_bytes(&[], 0).is_none());
    assert!(SourceInfo::from_bytes(&[0; 4], 2).is_none());
    assert!(SourceInfo::from_bytes(&[0; 12], 3).is_none());
    assert!(SourceInfo::from_bytes(&[0; 4], 1).is_some());
    assert!(SourceInfo::from_bytes(&[0; 8], 2).is_some());
}
