//! Protocol behavior with the CH32 PHY's acknowledgement model: the driver
//! sends GoodCRC for every received frame (`HAS_AUTO_GOOD_CRC`), while the
//! protocol layer still waits for the partner's GoodCRC and retries in
//! software. A scripted Source reacts to each Sink transmission.

use std::collections::VecDeque;
use std::future::{Future, pending, poll_fn};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::extended::ExtendedHeader;
use usbpd::protocol_layer::message::extended::pps_status::PpsStatus;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event};
use usbpd::sink::policy_engine::{Error as SinkError, Sink};
use usbpd::timers::Timer;
use usbpd::{DataRole, PowerRole};
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    for _ in 0..1_000_000 {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
    }
    panic!("the policy engine stopped making progress");
}

struct NeverTimer;

impl Timer for NeverTimer {
    async fn after_millis(_milliseconds: u64) {
        pending().await
    }
}

static GUARD_WAITS: AtomicUsize = AtomicUsize::new(0);

/// Expires only the 20 ms Sink AMS guard, counting each expiry.
struct GuardTimer;

impl Timer for GuardTimer {
    async fn after_millis(milliseconds: u64) {
        if milliseconds == 20 {
            GUARD_WAITS.fetch_add(1, Ordering::SeqCst);
            return;
        }
        pending().await
    }
}

fn source_header(message_id: u8, message_type: MessageType, num_objects: u8) -> Header {
    let (raw_type, extended) = match message_type {
        MessageType::Control(message_type) => (message_type as u8, false),
        MessageType::Data(message_type) => (message_type as u8, false),
        MessageType::Extended(message_type) => (message_type as u8, true),
    };
    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_id(message_id)
        .with_message_type_raw(raw_type)
        .with_num_objects(num_objects)
        .with_extended(extended)
}

fn control(message_id: u8, message_type: ControlMessageType) -> Vec<u8> {
    let mut bytes = vec![0; 2];
    source_header(message_id, MessageType::Control(message_type), 0).to_bytes(&mut bytes);
    bytes
}

fn good_crc(message_id: u8) -> Vec<u8> {
    control(message_id, ControlMessageType::GoodCRC)
}

fn fixed_5v_capabilities(message_id: u8) -> Vec<u8> {
    let fixed_5v: u32 = ((5_000 / 50) << 10) | (3_000 / 10);
    let mut bytes = vec![0; 6];
    source_header(message_id, MessageType::Data(DataMessageType::SourceCapabilities), 1).to_bytes(&mut bytes[..2]);
    bytes[2..].copy_from_slice(&fixed_5v.to_le_bytes());
    bytes
}

fn pps_status(message_id: u8) -> Vec<u8> {
    let payload = [0x5c, 0x03, 46, 0b0000_1010];
    let mut bytes = vec![0; 2 + 8];
    source_header(message_id, MessageType::Extended(ExtendedMessageType::PpsStatus), 2).to_bytes(&mut bytes[..2]);
    ExtendedHeader::new(payload.len() as u16).with_chunked(true).to_bytes(&mut bytes[2..4]);
    bytes[4..8].copy_from_slice(&payload);
    bytes
}

type Responder = Box<dyn FnMut(usize, Header) -> Vec<Vec<u8>>>;

/// Delivers the Source's frames in order. Each Sink transmission is shown to
/// `respond`, which returns what the Source puts on the wire next.
struct ScriptedSource {
    receive: VecDeque<Vec<u8>>,
    transmitted: Arc<Mutex<Vec<Header>>>,
    respond: Responder,
    detach_after: usize,
}

impl Driver for ScriptedSource {
    const HAS_AUTO_GOOD_CRC: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        poll_fn(|_| {
            if let Some(frame) = self.receive.pop_front() {
                buffer[..frame.len()].copy_from_slice(&frame);
                return Poll::Ready(Ok(frame.len()));
            }
            if self.transmitted.lock().unwrap().len() >= self.detach_after {
                return Poll::Ready(Err(DriverRxError::Detached));
            }
            Poll::Pending
        })
        .await
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        let header = Header::from_bytes(&data[..2]).unwrap();
        let index = {
            let mut transmitted = self.transmitted.lock().unwrap();
            transmitted.push(header);
            transmitted.len() - 1
        };
        let frames = (self.respond)(index, header);
        self.receive.extend(frames);
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        panic!("a crossed or unacknowledged message must not escalate to Hard Reset")
    }
}

#[derive(Default)]
struct Observations {
    transitions: AtomicUsize,
    pps_statuses: AtomicUsize,
    pps_queries: AtomicUsize,
}

struct QueryDpm {
    seen: Arc<Observations>,
    queries: usize,
    guard_ms: u32,
}

impl DevicePolicyManager for QueryDpm {
    fn transition_power(&mut self, _accepted: &usbpd::protocol_layer::message::data::request::PowerSource) {
        self.seen.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn inform_pps_status(&mut self, _status: &PpsStatus) {
        self.seen.pps_statuses.fetch_add(1, Ordering::SeqCst);
    }

    fn sink_ams_guard_millis(&self) -> u32 {
        self.guard_ms
    }

    fn get_event(
        &mut self,
        _source_capabilities: &usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities,
    ) -> impl Future<Output = Event> {
        // Issue the next query only after the previous one was answered.
        let issued = self.seen.pps_queries.load(Ordering::SeqCst);
        let answered = self.seen.pps_statuses.load(Ordering::SeqCst);
        let ready = self.seen.transitions.load(Ordering::SeqCst) >= 1 && issued < self.queries && answered == issued;
        if ready {
            self.seen.pps_queries.fetch_add(1, Ordering::SeqCst);
        }
        async move { if ready { Event::RequestPpsStatus } else { pending().await } }
    }
}

fn message_type(header: &Header) -> MessageType {
    header.message_type()
}

fn run<T: Timer>(
    respond: Responder,
    detach_after: usize,
    queries: usize,
    guard_ms: u32,
) -> (Vec<Header>, Arc<Observations>) {
    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Observations::default());
    let driver = ScriptedSource {
        receive: VecDeque::from([fixed_5v_capabilities(0)]),
        transmitted: Arc::clone(&transmitted),
        respond,
        detach_after,
    };
    let dpm = QueryDpm { seen: Arc::clone(&seen), queries, guard_ms };
    let mut sink: Sink<_, T, _> = Sink::new(driver, dpm);
    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    let transmitted = transmitted.lock().unwrap().clone();
    (transmitted, seen)
}

const REQUEST: MessageType = MessageType::Data(DataMessageType::Request);
const GET_PPS_STATUS: MessageType = MessageType::Control(ControlMessageType::GetPpsStatus);
const ACCEPT: MessageType = MessageType::Control(ControlMessageType::Accept);
const SOFT_RESET: MessageType = MessageType::Control(ControlMessageType::SoftReset);

#[test]
fn partner_soft_reset_during_goodcrc_wait_is_accepted_without_a_sink_soft_reset() {
    let (transmitted, seen) = run::<NeverTimer>(
        Box::new(|index, header| match index {
            0 => vec![
                good_crc(header.message_id()),
                control(1, ControlMessageType::Accept),
                control(2, ControlMessageType::PsRdy),
            ],
            // The Source soft-resets instead of acknowledging the query.
            1 => vec![control(0, ControlMessageType::SoftReset)],
            2 => vec![good_crc(header.message_id()), fixed_5v_capabilities(1)],
            3 => vec![
                good_crc(header.message_id()),
                control(2, ControlMessageType::Accept),
                control(3, ControlMessageType::PsRdy),
            ],
            _ => vec![good_crc(header.message_id())],
        }),
        4,
        1,
        0,
    );

    let types: Vec<_> = transmitted.iter().map(message_type).collect();
    assert_eq!(types, [REQUEST, GET_PPS_STATUS, ACCEPT, REQUEST]);
    assert!(!types.contains(&SOFT_RESET));
    assert_eq!(seen.transitions.load(Ordering::SeqCst), 2);
}

#[test]
fn an_answer_without_goodcrc_acknowledges_the_query_and_consumes_its_message_id() {
    let (transmitted, seen) = run::<NeverTimer>(
        Box::new(|index, header| match index {
            0 => vec![
                good_crc(header.message_id()),
                control(1, ControlMessageType::Accept),
                control(2, ControlMessageType::PsRdy),
            ],
            // GoodCRC lost on the wire; the Source's answer arrives instead.
            1 => vec![pps_status(3)],
            _ => vec![good_crc(header.message_id()), pps_status(4)],
        }),
        3,
        2,
        0,
    );

    let types: Vec<_> = transmitted.iter().map(message_type).collect();
    assert_eq!(types, [REQUEST, GET_PPS_STATUS, GET_PPS_STATUS]);
    let ids: Vec<_> = transmitted.iter().map(Header::message_id).collect();
    assert_eq!(ids, [0, 1, 2], "reusing MessageID 1 would make the Source drop the next query");
    assert_eq!(seen.pps_statuses.load(Ordering::SeqCst), 2);
}

#[test]
fn a_crossing_partner_request_is_served_before_the_query_is_repeated() {
    let (transmitted, seen) = run::<NeverTimer>(
        Box::new(|index, header| match index {
            0 => vec![
                good_crc(header.message_id()),
                control(1, ControlMessageType::Accept),
                control(2, ControlMessageType::PsRdy),
            ],
            // The Source starts its own AMS; our query was not received.
            1 => vec![control(3, ControlMessageType::GetSinkCap)],
            2 => vec![good_crc(header.message_id())],
            _ => vec![good_crc(header.message_id()), pps_status(4)],
        }),
        4,
        1,
        0,
    );

    let types: Vec<_> = transmitted.iter().map(message_type).collect();
    assert_eq!(types, [REQUEST, GET_PPS_STATUS, MessageType::Data(DataMessageType::SinkCapabilities), GET_PPS_STATUS]);
    let ids: Vec<_> = transmitted.iter().map(Header::message_id).collect();
    assert_eq!(ids, [0, 1, 2, 3]);
    assert_eq!(seen.pps_statuses.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_goodcrc_and_partner_retransmission_do_not_disturb_the_goodcrc_wait() {
    let (transmitted, seen) = run::<NeverTimer>(
        Box::new(|index, header| match index {
            0 => vec![
                good_crc(header.message_id()),
                control(1, ControlMessageType::Accept),
                control(2, ControlMessageType::PsRdy),
            ],
            1 => vec![
                // A late acknowledgement for the Request and a retransmitted
                // PS_RDY precede the real GoodCRC.
                good_crc(0),
                control(2, ControlMessageType::PsRdy),
                good_crc(header.message_id()),
                pps_status(3),
            ],
            _ => vec![good_crc(header.message_id())],
        }),
        2,
        1,
        0,
    );

    let types: Vec<_> = transmitted.iter().map(message_type).collect();
    assert_eq!(types, [REQUEST, GET_PPS_STATUS]);
    assert_eq!(seen.transitions.load(Ordering::SeqCst), 1);
    assert_eq!(seen.pps_statuses.load(Ordering::SeqCst), 1);
}

#[test]
fn sink_initiated_ams_waits_for_the_guard_after_an_exchange() {
    static QUERY_AFTER_GUARD: AtomicBool = AtomicBool::new(false);
    GUARD_WAITS.store(0, Ordering::SeqCst);
    QUERY_AFTER_GUARD.store(false, Ordering::SeqCst);

    let (transmitted, seen) = run::<GuardTimer>(
        Box::new(|index, header| {
            if index == 1 {
                QUERY_AFTER_GUARD.store(GUARD_WAITS.load(Ordering::SeqCst) >= 1, Ordering::SeqCst);
            }
            match index {
                0 => vec![
                    good_crc(header.message_id()),
                    control(1, ControlMessageType::Accept),
                    control(2, ControlMessageType::PsRdy),
                ],
                _ => vec![good_crc(header.message_id()), pps_status(3)],
            }
        }),
        2,
        1,
        20,
    );

    let types: Vec<_> = transmitted.iter().map(message_type).collect();
    assert_eq!(types, [REQUEST, GET_PPS_STATUS]);
    assert!(QUERY_AFTER_GUARD.load(Ordering::SeqCst), "the query must follow the post-PS_RDY guard");
    assert_eq!(seen.pps_statuses.load(Ordering::SeqCst), 1);
}
