use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::data::alert::AlertDataObject;
use usbpd::protocol_layer::message::extended::ExtendedHeader;
use usbpd::protocol_layer::message::extended::pps_status::PpsStatus;
use usbpd::protocol_layer::message::extended::status::Status;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event, StatusQueryFailure, StatusQueryKind};
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

fn source_extended(message_id: u8, message_type: ExtendedMessageType, payload: &[u8]) -> Vec<u8> {
    let body_size = 2 + payload.len();
    let num_objects = body_size.div_ceil(4);
    let mut bytes = vec![0; 2 + num_objects * 4];
    source_header(message_id, MessageType::Extended(message_type), num_objects as u8).to_bytes(&mut bytes[..2]);
    ExtendedHeader::new(payload.len() as u16).with_chunked(true).to_bytes(&mut bytes[2..4]);
    bytes[4..4 + payload.len()].copy_from_slice(payload);
    bytes
}

struct ScriptedDriver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Driver for ScriptedDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let Some((required_transmits, _)) = self.receive.front() else {
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
        panic!("status telemetry must preserve the explicit contract")
    }
}

struct StatusDpm {
    transitions: Arc<AtomicUsize>,
    pps_requested: bool,
    alert_seen: Arc<AtomicBool>,
    status_requested: bool,
    pps_raw: Arc<AtomicU32>,
    status_event_flags: Arc<AtomicUsize>,
    status_failure: Arc<AtomicUsize>,
}

impl DevicePolicyManager for StatusDpm {
    fn transition_power(&mut self, _accepted: &usbpd::protocol_layer::message::data::request::PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn inform_alert(&mut self, alert: &AlertDataObject) {
        self.alert_seen.store(alert.operating_condition_change(), Ordering::SeqCst);
    }

    fn inform_pps_status(&mut self, status: &PpsStatus) {
        self.pps_raw.store(u32::from_le_bytes(status.raw_bytes()), Ordering::SeqCst);
    }

    fn inform_status(&mut self, status: &Status) {
        self.status_event_flags.store(status.event_flags() as usize, Ordering::SeqCst);
    }

    fn status_query_failed(&mut self, query: StatusQueryKind, failure: StatusQueryFailure) {
        let query = match query {
            StatusQueryKind::General => 0,
            StatusQueryKind::Pps => 10,
            StatusQueryKind::SourceInfo => 20,
        };
        let failure = match failure {
            StatusQueryFailure::NotSupported => 1,
            StatusQueryFailure::Rejected => 2,
            StatusQueryFailure::Deferred => 3,
            StatusQueryFailure::Timeout => 4,
            StatusQueryFailure::UnsupportedRevision => 5,
        };
        self.status_failure.store(query + failure, Ordering::SeqCst);
    }

    fn get_event(
        &mut self,
        _source_capabilities: &usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities,
    ) -> impl Future<Output = Event> {
        let event = if self.transitions.load(Ordering::SeqCst) == 1 && !self.pps_requested {
            self.pps_requested = true;
            Some(Event::RequestPpsStatus)
        } else if self.alert_seen.load(Ordering::SeqCst) && !self.status_requested {
            self.status_requested = true;
            Some(Event::RequestStatus)
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
fn pps_status_and_alert_driven_general_status_preserve_the_contract() {
    let fixed_5v = ((5_000 / 50) << 10) | (3_000 / 10);
    let pps_status = [0x5c, 0x03, 46, 0b0000_1010];
    let general_status = [42, 0b0001_0110, 0, 0b0001_0000, 0b10, 0b10, 0];
    let receive = VecDeque::from([
        (0, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_extended(3, ExtendedMessageType::PpsStatus, &pps_status)),
        (2, source_data(4, DataMessageType::Alert, &[1 << 28])),
        (3, source_extended(5, ExtendedMessageType::Status, &general_status)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let transitions = Arc::new(AtomicUsize::new(0));
    let alert_seen = Arc::new(AtomicBool::new(false));
    let pps_raw = Arc::new(AtomicU32::new(0));
    let status_event_flags = Arc::new(AtomicUsize::new(0));
    let status_failure = Arc::new(AtomicUsize::new(0));
    let driver = ScriptedDriver { receive, transmitted: Arc::clone(&transmitted) };
    let dpm = StatusDpm {
        transitions: Arc::clone(&transitions),
        pps_requested: false,
        alert_seen: Arc::clone(&alert_seen),
        status_requested: false,
        pps_raw: Arc::clone(&pps_raw),
        status_event_flags: Arc::clone(&status_event_flags),
        status_failure: Arc::clone(&status_failure),
    };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert_eq!(transitions.load(Ordering::SeqCst), 1);
    assert!(alert_seen.load(Ordering::SeqCst));
    assert_eq!(pps_raw.load(Ordering::SeqCst), u32::from_le_bytes(pps_status));
    assert_eq!(status_event_flags.load(Ordering::SeqCst), 1 << 4);
    assert_eq!(status_failure.load(Ordering::SeqCst), 0);

    let message_types: Vec<MessageType> = transmitted
        .lock()
        .unwrap()
        .iter()
        .map(|message| Header::from_bytes(&message[..2]).unwrap().message_type())
        .collect();
    assert_eq!(
        message_types,
        [
            MessageType::Data(DataMessageType::Request),
            MessageType::Control(ControlMessageType::GetPpsStatus),
            MessageType::Control(ControlMessageType::GetStatus),
        ]
    );
}

#[test]
fn refused_or_deferred_pps_status_is_nonfatal_and_reported() {
    for (response, expected) in
        [(ControlMessageType::NotSupported, 11), (ControlMessageType::Reject, 12), (ControlMessageType::Wait, 13)]
    {
        let fixed_5v = ((5_000 / 50) << 10) | (3_000 / 10);
        let receive = VecDeque::from([
            (0, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v])),
            (1, source_control(1, ControlMessageType::Accept)),
            (1, source_control(2, ControlMessageType::PsRdy)),
            (2, source_control(3, response)),
        ]);

        let transmitted = Arc::new(Mutex::new(Vec::new()));
        let transitions = Arc::new(AtomicUsize::new(0));
        let failure = Arc::new(AtomicUsize::new(0));
        let driver = ScriptedDriver { receive, transmitted: Arc::clone(&transmitted) };
        let dpm = StatusDpm {
            transitions: Arc::clone(&transitions),
            pps_requested: false,
            alert_seen: Arc::new(AtomicBool::new(false)),
            status_requested: false,
            pps_raw: Arc::new(AtomicU32::new(0)),
            status_event_flags: Arc::new(AtomicUsize::new(0)),
            status_failure: Arc::clone(&failure),
        };
        let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

        assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
        assert_eq!(transitions.load(Ordering::SeqCst), 1);
        assert_eq!(failure.load(Ordering::SeqCst), expected);
        assert_eq!(transmitted.lock().unwrap().len(), 2);
    }
}

#[test]
fn status_parsers_accept_defined_fields_and_ignore_future_trailing_bytes() {
    assert!(PpsStatus::from_bytes(&[0; 3]).is_none());
    assert_eq!(PpsStatus::from_bytes(&[0x5c, 0x03, 46, 0x0a, 0xff]).unwrap().output_voltage_20mv(), Some(860));

    assert!(Status::from_bytes(&[0; 6]).is_none());
    assert_eq!(Status::from_bytes(&[0, 0, 0, 0x10, 0, 0, 0, 0xff]).unwrap().event_flags(), 0x10);
}
