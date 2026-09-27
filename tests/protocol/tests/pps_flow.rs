use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::data::request::{Avs, CurrentRequest, PowerSource, Pps, VoltageRequest};
use usbpd::protocol_layer::message::data::source_capabilities::{Augmented, PowerDataObject, SourceCapabilities};
use usbpd::protocol_layer::message::extended::ExtendedHeader;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event, RequestRejection};
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

/// Only the PPS refresh timer expires. Protocol response timers remain under
/// control of the scripted source.
struct PpsRefreshTimer;

impl Timer for PpsRefreshTimer {
    async fn after_millis(milliseconds: u64) {
        if milliseconds != 4_992 {
            pending().await
        }
    }
}

static TELEMETRY_CLOCK_MS: AtomicU32 = AtomicU32::new(0);
static SOFTWARE_PPS_CLOCK_TICKS: AtomicU32 = AtomicU32::new(0);
static SOFTWARE_CRC_TIMEOUTS: AtomicUsize = AtomicUsize::new(0);

/// Advances only when the test DPM issues another telemetry inquiry. A
/// deadline-based PPS timer reaches zero after five inquiries; a recreated
/// relative timer would remain five seconds away forever.
struct PpsTelemetryTimer;

impl Timer for PpsTelemetryTimer {
    fn now_128ms_ticks() -> u32 {
        TELEMETRY_CLOCK_MS.load(Ordering::SeqCst)
    }

    async fn after_millis(milliseconds: u64) {
        if milliseconds != 0 {
            pending().await
        }
    }
}

/// Exercises the same software GoodCRC/retry path used by CH32X035. Two
/// telemetry inquiries advance the clock by about two seconds each; the
/// remaining 896 ms PPS-refresh deadline then wins before another inquiry.
struct SoftwarePpsTimer;

impl Timer for SoftwarePpsTimer {
    fn now_128ms_ticks() -> u32 {
        SOFTWARE_PPS_CLOCK_TICKS.load(Ordering::SeqCst)
    }

    async fn after_millis(milliseconds: u64) {
        if milliseconds == 1
            && SOFTWARE_CRC_TIMEOUTS
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| remaining.checked_sub(1))
                .is_ok()
        {
            return;
        }
        if milliseconds == 896 {
            return;
        }
        pending().await
    }
}

/// Only SinkRequestTimer expires. This models the mandatory delay after a
/// source defers a power request with Wait.
struct WaitRetryTimer;

impl Timer for WaitRetryTimer {
    async fn after_millis(milliseconds: u64) {
        if milliseconds != 100 {
            pending().await
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

fn source_capabilities(message_id: u8, pdos: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; 2 + pdos.len() * 4];
    source_header(message_id, MessageType::Data(DataMessageType::SourceCapabilities), pdos.len() as u8)
        .to_bytes(&mut bytes[..2]);
    for (destination, pdo) in bytes[2..].chunks_exact_mut(4).zip(pdos) {
        destination.copy_from_slice(&pdo.to_le_bytes());
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

fn fixed_pdo(voltage_mv: u32, current_ma: u32) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10)
}

fn pps_pdo(minimum_mv: u32, maximum_mv: u32, current_ma: u32) -> u32 {
    (0b11 << 30) | ((maximum_mv / 100) << 17) | ((minimum_mv / 100) << 8) | (current_ma / 50)
}

fn spr_avs_pdo(current_15v_ma: u32, current_20v_ma: u32) -> u32 {
    (0b11 << 30) | (0b10 << 28) | ((current_15v_ma / 10) << 10) | (current_20v_ma / 10)
}

fn pps_request(position: u8, voltage_mv: u32, current_ma: u32) -> PowerSource {
    let rdo = (u32::from(position) << 28) | (1 << 24) | ((voltage_mv / 20) << 9) | (current_ma / 50);
    PowerSource::Pps(Pps(rdo))
}

fn avs_request(position: u8, voltage_mv: u32, current_ma: u32) -> PowerSource {
    let rdo = (u32::from(position) << 28) | (1 << 24) | ((voltage_mv / 25) << 9) | (current_ma / 50);
    PowerSource::Avs(Avs(rdo))
}

struct ScriptedPpsDriver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
    sink_tx_checks: Arc<AtomicUsize>,
    detach_after: usize,
}

impl Driver for ScriptedPpsDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    fn sink_tx_ok(&mut self) -> bool {
        self.sink_tx_checks.fetch_add(1, Ordering::SeqCst);
        true
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let transmitted = self.transmitted.lock().unwrap().len();
        eprintln!("software receive: transmitted={transmitted}, front={:?}", self.receive.front().map(|entry| entry.0));
        let Some((required_transmits, _)) = self.receive.front() else {
            return if transmitted >= self.detach_after { Err(DriverRxError::Detached) } else { pending().await };
        };
        if transmitted < *required_transmits {
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
        panic!("the PPS refresh trace must not hard reset")
    }
}

struct SoftwarePpsDriver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
    sink_tx_checks: Arc<AtomicUsize>,
    duplicate_ps_rdy_delivered: Arc<AtomicBool>,
    delay_first_good_crc_for: Option<MessageType>,
    delayed_good_crc: bool,
    ps_rdy_seen: bool,
    soft_reset_followup_guard: Option<usize>,
    detach_after: usize,
}

impl Driver for SoftwarePpsDriver {
    async fn wait_for_vbus(&mut self) {}

    fn sink_tx_ok(&mut self) -> bool {
        self.sink_tx_checks.fetch_add(1, Ordering::SeqCst);
        true
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let transmitted = self.transmitted.lock().unwrap().len();
        if self.soft_reset_followup_guard == Some(transmitted) {
            return Err(DriverRxError::Detached);
        }

        let Some((required_transmits, _)) = self.receive.front() else {
            return if transmitted >= self.detach_after { Err(DriverRxError::Detached) } else { pending().await };
        };
        if transmitted < *required_transmits {
            pending().await
        }

        let (_, message) = self.receive.pop_front().unwrap();
        let header = Header::from_bytes(&message[..2]).unwrap();
        eprintln!("software receive -> {:?} id={}", header.message_type(), header.message_id());
        match header.message_type() {
            MessageType::Control(ControlMessageType::PsRdy) if self.ps_rdy_seen => {
                self.duplicate_ps_rdy_delivered.store(true, Ordering::SeqCst);
            }
            MessageType::Control(ControlMessageType::PsRdy) => self.ps_rdy_seen = true,
            MessageType::Control(ControlMessageType::SoftReset) => {
                // A broken duplicate filter would immediately call receive()
                // again after the software GoodCRC instead of advancing the
                // policy engine to its reset Accept.
                self.soft_reset_followup_guard = Some(transmitted + 1);
            }
            _ => {}
        }
        buffer[..message.len()].copy_from_slice(&message);
        Ok(message.len())
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        let header = Header::from_bytes(&data[..2]).unwrap();
        let message_type = header.message_type();
        eprintln!("software transmit -> {message_type:?} id={}", header.message_id());
        if !self.delayed_good_crc && self.delay_first_good_crc_for == Some(message_type) {
            self.delayed_good_crc = true;
            SOFTWARE_CRC_TIMEOUTS.store(1, Ordering::SeqCst);
        }
        self.transmitted.lock().unwrap().push(data.to_vec());
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        panic!("the production software GoodCRC path must recover without Hard Reset")
    }
}

struct PpsDpm {
    detached: Arc<AtomicBool>,
    request_calls: Arc<AtomicUsize>,
    request: PowerSource,
}

struct TelemetryDpm {
    request_calls: Arc<AtomicUsize>,
    transitions: Arc<AtomicUsize>,
    queries: usize,
    request: PowerSource,
}

struct SoftwareTelemetryDpm {
    detached: Arc<AtomicBool>,
    duplicate_ps_rdy_delivered: Arc<AtomicBool>,
    request_calls: Arc<AtomicUsize>,
    transitions: Arc<AtomicUsize>,
    statuses: Arc<AtomicUsize>,
    queries: usize,
    request: PowerSource,
}

struct SoftwareSoftResetDpm {
    detached: Arc<AtomicBool>,
    request_calls: Arc<AtomicUsize>,
    transitions: Arc<AtomicUsize>,
    statuses: Arc<AtomicUsize>,
    event_sent: bool,
    request: PowerSource,
}

struct WaitRetryDpm {
    detached: Arc<AtomicBool>,
    transitions: Arc<AtomicUsize>,
    request_calls: Arc<AtomicUsize>,
    wait_seen: Arc<AtomicBool>,
    event_sent: bool,
    desired: PowerSource,
}

struct SprAvsDpm {
    detached: Arc<AtomicBool>,
    typed_offer_seen: Arc<AtomicBool>,
    request: PowerSource,
}

impl DevicePolicyManager for PpsDpm {
    fn request(&mut self, _source_capabilities: &SourceCapabilities) -> PowerSource {
        self.request_calls.fetch_add(1, Ordering::SeqCst);
        self.request
    }

    fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    async fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> Event {
        pending().await
    }
}

impl DevicePolicyManager for TelemetryDpm {
    fn request(&mut self, _source_capabilities: &SourceCapabilities) -> PowerSource {
        self.request_calls.fetch_add(1, Ordering::SeqCst);
        self.request
    }

    fn transition_power(&mut self, _accepted: &PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> impl Future<Output = Event> {
        let event = if self.transitions.load(Ordering::SeqCst) == 1 && self.queries < 5 {
            self.queries += 1;
            TELEMETRY_CLOCK_MS.store(self.queries as u32 * 8, Ordering::SeqCst);
            Some(Event::RequestPpsStatus)
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

impl DevicePolicyManager for SoftwareTelemetryDpm {
    fn request(&mut self, _source_capabilities: &SourceCapabilities) -> PowerSource {
        self.request_calls.fetch_add(1, Ordering::SeqCst);
        self.request
    }

    fn transition_power(&mut self, _accepted: &PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn inform_pps_status(&mut self, _status: &usbpd::protocol_layer::message::extended::pps_status::PpsStatus) {
        self.statuses.fetch_add(1, Ordering::SeqCst);
    }

    fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> impl Future<Output = Event> {
        eprintln!(
            "software event: transitions={} duplicate={} queries={}",
            self.transitions.load(Ordering::SeqCst),
            self.duplicate_ps_rdy_delivered.load(Ordering::SeqCst),
            self.queries
        );
        let event = if self.transitions.load(Ordering::SeqCst) == 1
            && self.duplicate_ps_rdy_delivered.load(Ordering::SeqCst)
            && self.queries < 2
        {
            self.queries += 1;
            SOFTWARE_PPS_CLOCK_TICKS.store(self.queries as u32 * 16, Ordering::SeqCst);
            Some(Event::RequestPpsStatus)
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

impl DevicePolicyManager for SoftwareSoftResetDpm {
    fn request(&mut self, _source_capabilities: &SourceCapabilities) -> PowerSource {
        self.request_calls.fetch_add(1, Ordering::SeqCst);
        self.request
    }

    fn transition_power(&mut self, _accepted: &PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn inform_pps_status(&mut self, _status: &usbpd::protocol_layer::message::extended::pps_status::PpsStatus) {
        self.statuses.fetch_add(1, Ordering::SeqCst);
    }

    fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> impl Future<Output = Event> {
        let event = if self.transitions.load(Ordering::SeqCst) == 1 && !self.event_sent {
            self.event_sent = true;
            Some(Event::RequestPpsStatus)
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

impl DevicePolicyManager for WaitRetryDpm {
    fn request(&mut self, source_capabilities: &SourceCapabilities) -> PowerSource {
        let call = self.request_calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            PowerSource::new_fixed(CurrentRequest::Highest, VoltageRequest::Safe5V, source_capabilities).unwrap()
        } else {
            self.desired
        }
    }

    fn transition_power(&mut self, _accepted: &PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn request_not_accepted(&mut self, reason: RequestRejection) {
        assert_eq!(reason, RequestRejection::Wait);
        self.wait_seen.store(true, Ordering::SeqCst);
    }

    fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> impl Future<Output = Event> {
        let event = if !self.event_sent && self.transitions.load(Ordering::SeqCst) == 1 {
            self.event_sent = true;
            Some(Event::RequestPower(self.desired))
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

impl DevicePolicyManager for SprAvsDpm {
    fn inform(&mut self, source_capabilities: &SourceCapabilities) {
        assert!(matches!(source_capabilities.pdos().nth(1), Some(PowerDataObject::Augmented(Augmented::SprAvs(_)))));
        self.typed_offer_seen.store(true, Ordering::SeqCst);
    }

    fn request(&mut self, _source_capabilities: &SourceCapabilities) -> PowerSource {
        self.request
    }

    fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    async fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> Event {
        pending().await
    }
}

#[test]
fn pps_contract_is_refreshed_as_a_sinktx_gated_ams() {
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let pps = pps_pdo(5_000, 21_000, 3_000);
    let request = pps_request(2, 17_220, 3_000);
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, pps])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_control(3, ControlMessageType::Accept)),
        (2, source_control(4, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let sink_tx_checks = Arc::new(AtomicUsize::new(0));
    let detached = Arc::new(AtomicBool::new(false));
    let request_calls = Arc::new(AtomicUsize::new(0));
    let driver = ScriptedPpsDriver {
        receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_checks: Arc::clone(&sink_tx_checks),
        detach_after: 2,
    };
    let dpm = PpsDpm { detached: Arc::clone(&detached), request_calls: Arc::clone(&request_calls), request };
    let mut sink: Sink<_, PpsRefreshTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert_eq!(sink_tx_checks.load(Ordering::SeqCst), 1, "only the periodic refresh starts a Sink AMS");
    assert_eq!(request_calls.load(Ordering::SeqCst), 2, "initial request and periodic refresh both use the DPM");

    let transmitted = transmitted.lock().unwrap();
    assert_eq!(transmitted.len(), 2);
    assert_eq!(&transmitted[0][2..], &transmitted[1][2..], "the refresh must repeat the exact accepted PPS RDO");
    let first_header = Header::from_bytes(&transmitted[0][..2]).unwrap();
    let refresh_header = Header::from_bytes(&transmitted[1][..2]).unwrap();
    assert_eq!(first_header.message_type(), MessageType::Data(DataMessageType::Request));
    assert_eq!(refresh_header.message_type(), MessageType::Data(DataMessageType::Request));
    assert_eq!(refresh_header.message_id(), (first_header.message_id() + 1) & 0x07);
    let rdo = u32::from_le_bytes(transmitted[0][2..6].try_into().unwrap());
    assert_eq!((rdo >> 9) & 0x7ff, 861, "the requested voltage must be encoded in exact 20 mV units");
    assert_eq!(rdo & 0x7f, 60, "3 A must be encoded in 50 mA units");
}

#[test]
fn pps_telemetry_does_not_postpone_the_contract_refresh_deadline() {
    TELEMETRY_CLOCK_MS.store(0, Ordering::SeqCst);
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let pps = pps_pdo(5_000, 21_000, 3_000);
    let request = pps_request(2, 17_220, 3_000);
    let pps_status = [0x5c, 0x03, 0, 0];
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, pps])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_extended(3, ExtendedMessageType::PpsStatus, &pps_status)),
        (3, source_extended(4, ExtendedMessageType::PpsStatus, &pps_status)),
        (4, source_extended(5, ExtendedMessageType::PpsStatus, &pps_status)),
        (5, source_extended(6, ExtendedMessageType::PpsStatus, &pps_status)),
        (6, source_extended(7, ExtendedMessageType::PpsStatus, &pps_status)),
        (7, source_control(0, ControlMessageType::Accept)),
        (7, source_control(1, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let request_calls = Arc::new(AtomicUsize::new(0));
    let transitions = Arc::new(AtomicUsize::new(0));
    let driver = ScriptedPpsDriver {
        receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_checks: Arc::new(AtomicUsize::new(0)),
        detach_after: 7,
    };
    let dpm = TelemetryDpm {
        request_calls: Arc::clone(&request_calls),
        transitions: Arc::clone(&transitions),
        queries: 0,
        request,
    };
    let mut sink: Sink<_, PpsTelemetryTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert_eq!(request_calls.load(Ordering::SeqCst), 2, "telemetry must not starve the periodic DPM re-plan");
    assert_eq!(transitions.load(Ordering::SeqCst), 2);

    let transmitted = transmitted.lock().unwrap();
    let message_types: Vec<MessageType> =
        transmitted.iter().map(|message| Header::from_bytes(&message[..2]).unwrap().message_type()).collect();
    assert_eq!(
        message_types,
        [
            MessageType::Data(DataMessageType::Request),
            MessageType::Control(ControlMessageType::GetPpsStatus),
            MessageType::Control(ControlMessageType::GetPpsStatus),
            MessageType::Control(ControlMessageType::GetPpsStatus),
            MessageType::Control(ControlMessageType::GetPpsStatus),
            MessageType::Control(ControlMessageType::GetPpsStatus),
            MessageType::Data(DataMessageType::Request),
        ]
    );
}

#[test]
fn production_software_good_crc_path_survives_pps_traffic_and_refresh() {
    SOFTWARE_PPS_CLOCK_TICKS.store(0, Ordering::SeqCst);
    SOFTWARE_CRC_TIMEOUTS.store(0, Ordering::SeqCst);
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let pps = pps_pdo(5_000, 21_000, 3_000);
    let request = pps_request(2, 17_220, 3_000);
    let pps_status = [0x5c, 0x03, 0, 0];
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, pps])),
        (2, source_control(0, ControlMessageType::GoodCRC)),
        (2, source_control(1, ControlMessageType::Accept)),
        (3, source_control(2, ControlMessageType::PsRdy)),
        // The Source retransmits PS_RDY because its first GoodCRC was lost.
        // It must be acknowledged and ignored without disturbing the contract.
        (4, source_control(2, ControlMessageType::PsRdy)),
        // The first telemetry GoodCRC arrives only after one software retry.
        (7, source_control(1, ControlMessageType::GoodCRC)),
        (7, source_extended(3, ExtendedMessageType::PpsStatus, &pps_status)),
        (9, source_control(2, ControlMessageType::GoodCRC)),
        (9, source_extended(4, ExtendedMessageType::PpsStatus, &pps_status)),
        // The mandatory PPS refresh wins after two roughly 2 s inquiries.
        (11, source_control(3, ControlMessageType::GoodCRC)),
        (11, source_control(5, ControlMessageType::Accept)),
        (12, source_control(6, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let sink_tx_checks = Arc::new(AtomicUsize::new(0));
    let duplicate_ps_rdy_delivered = Arc::new(AtomicBool::new(false));
    let detached = Arc::new(AtomicBool::new(false));
    let request_calls = Arc::new(AtomicUsize::new(0));
    let transitions = Arc::new(AtomicUsize::new(0));
    let statuses = Arc::new(AtomicUsize::new(0));
    let driver = SoftwarePpsDriver {
        receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_checks: Arc::clone(&sink_tx_checks),
        duplicate_ps_rdy_delivered: Arc::clone(&duplicate_ps_rdy_delivered),
        delay_first_good_crc_for: Some(MessageType::Control(ControlMessageType::GetPpsStatus)),
        delayed_good_crc: false,
        ps_rdy_seen: false,
        soft_reset_followup_guard: None,
        detach_after: 13,
    };
    let dpm = SoftwareTelemetryDpm {
        detached: Arc::clone(&detached),
        duplicate_ps_rdy_delivered,
        request_calls: Arc::clone(&request_calls),
        transitions: Arc::clone(&transitions),
        statuses: Arc::clone(&statuses),
        queries: 0,
        request,
    };
    let mut sink: Sink<_, SoftwarePpsTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert_eq!(request_calls.load(Ordering::SeqCst), 2, "initial selection plus mandatory refresh");
    assert_eq!(transitions.load(Ordering::SeqCst), 2);
    assert_eq!(statuses.load(Ordering::SeqCst), 2);
    assert!(sink_tx_checks.load(Ordering::SeqCst) >= 3, "telemetry and refresh must all be SinkTx gated");

    let transmitted = transmitted.lock().unwrap();
    let requests: Vec<&Vec<u8>> = transmitted
        .iter()
        .filter(|message| {
            Header::from_bytes(&message[..2]).unwrap().message_type() == MessageType::Data(DataMessageType::Request)
        })
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(&requests[0][2..], &requests[1][2..], "refresh must preserve the accepted PPS RDO");

    let telemetry: Vec<&Vec<u8>> = transmitted
        .iter()
        .filter(|message| {
            Header::from_bytes(&message[..2]).unwrap().message_type()
                == MessageType::Control(ControlMessageType::GetPpsStatus)
        })
        .collect();
    assert_eq!(telemetry.len(), 3, "one missed GoodCRC must cause exactly one retry");
    assert_eq!(telemetry[0], telemetry[1], "software retry must preserve the frame and MessageID");
    assert_eq!(
        Header::from_bytes(&telemetry[2][..2]).unwrap().message_id(),
        (Header::from_bytes(&telemetry[0][..2]).unwrap().message_id() + 1) & 0x07
    );
}

#[test]
fn production_software_good_crc_path_recovers_from_same_id_soft_reset() {
    SOFTWARE_CRC_TIMEOUTS.store(0, Ordering::SeqCst);
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let pps = pps_pdo(5_000, 21_000, 3_000);
    let request = pps_request(2, 17_220, 3_000);
    let pps_status = [0x5c, 0x03, 0, 0];
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, pps])),
        (2, source_control(0, ControlMessageType::GoodCRC)),
        (2, source_control(1, ControlMessageType::Accept)),
        (3, source_control(2, ControlMessageType::PsRdy)),
        (5, source_control(1, ControlMessageType::GoodCRC)),
        (5, source_extended(3, ExtendedMessageType::PpsStatus, &pps_status)),
        // Soft Reset intentionally reuses the preceding PPS_Status MessageID.
        (6, source_control(3, ControlMessageType::SoftReset)),
        (8, source_control(0, ControlMessageType::GoodCRC)),
        (8, source_capabilities(0, &[fixed_5v, pps])),
        (10, source_control(1, ControlMessageType::GoodCRC)),
        (10, source_control(1, ControlMessageType::Accept)),
        (11, source_control(2, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let detached = Arc::new(AtomicBool::new(false));
    let request_calls = Arc::new(AtomicUsize::new(0));
    let transitions = Arc::new(AtomicUsize::new(0));
    let statuses = Arc::new(AtomicUsize::new(0));
    let driver = SoftwarePpsDriver {
        receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_checks: Arc::new(AtomicUsize::new(0)),
        duplicate_ps_rdy_delivered: Arc::new(AtomicBool::new(false)),
        delay_first_good_crc_for: None,
        delayed_good_crc: false,
        ps_rdy_seen: false,
        soft_reset_followup_guard: None,
        detach_after: 12,
    };
    let dpm = SoftwareSoftResetDpm {
        detached: Arc::clone(&detached),
        request_calls: Arc::clone(&request_calls),
        transitions: Arc::clone(&transitions),
        statuses: Arc::clone(&statuses),
        event_sent: false,
        request,
    };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert_eq!(request_calls.load(Ordering::SeqCst), 2);
    assert_eq!(transitions.load(Ordering::SeqCst), 2, "the PPS contract must be restored after Soft Reset");
    assert_eq!(statuses.load(Ordering::SeqCst), 1);

    let transmitted = transmitted.lock().unwrap();
    let accepts: Vec<Header> = transmitted
        .iter()
        .map(|message| Header::from_bytes(&message[..2]).unwrap())
        .filter(|header| header.message_type() == MessageType::Control(ControlMessageType::Accept))
        .collect();
    assert_eq!(accepts.len(), 1, "one partner Soft Reset must produce one policy Accept");
    assert_eq!(accepts[0].message_id(), 0, "Soft Reset must restart the TX MessageID sequence");

    let same_id_good_crc_count = transmitted
        .iter()
        .map(|message| Header::from_bytes(&message[..2]).unwrap())
        .filter(|header| {
            header.message_type() == MessageType::Control(ControlMessageType::GoodCRC) && header.message_id() == 3
        })
        .count();
    assert_eq!(same_id_good_crc_count, 2, "PPS_Status and same-ID Soft Reset must each receive GoodCRC");
}

#[test]
fn wait_replans_and_retries_the_deferred_request_after_servicing_source_traffic() {
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let pps = pps_pdo(5_000, 21_000, 3_000);
    let desired = pps_request(2, 17_220, 3_000);
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, pps])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_control(3, ControlMessageType::Wait)),
        (2, source_control(4, ControlMessageType::GetSinkCap)),
        (4, source_control(5, ControlMessageType::Accept)),
        (4, source_control(6, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let sink_tx_checks = Arc::new(AtomicUsize::new(0));
    let detached = Arc::new(AtomicBool::new(false));
    let transitions = Arc::new(AtomicUsize::new(0));
    let request_calls = Arc::new(AtomicUsize::new(0));
    let wait_seen = Arc::new(AtomicBool::new(false));
    let driver = ScriptedPpsDriver {
        receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_checks: Arc::clone(&sink_tx_checks),
        detach_after: 2,
    };
    let dpm = WaitRetryDpm {
        detached: Arc::clone(&detached),
        transitions: Arc::clone(&transitions),
        request_calls: Arc::clone(&request_calls),
        wait_seen: Arc::clone(&wait_seen),
        event_sent: false,
        desired,
    };
    let mut sink: Sink<_, WaitRetryTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert!(wait_seen.load(Ordering::SeqCst));
    assert_eq!(transitions.load(Ordering::SeqCst), 2);
    assert_eq!(request_calls.load(Ordering::SeqCst), 2, "initial selection plus deferred re-plan");
    assert!(sink_tx_checks.load(Ordering::SeqCst) >= 2, "both user request attempts are SinkTx gated");

    let transmitted = transmitted.lock().unwrap();
    let message_types: Vec<MessageType> =
        transmitted.iter().map(|message| Header::from_bytes(&message[..2]).unwrap().message_type()).collect();
    assert_eq!(
        message_types,
        [
            MessageType::Data(DataMessageType::Request),
            MessageType::Data(DataMessageType::Request),
            MessageType::Data(DataMessageType::SinkCapabilities),
            MessageType::Data(DataMessageType::Request),
        ]
    );
    assert_ne!(&transmitted[0][2..], &transmitted[1][2..], "the deferred request must not be the old 5 V RDO");
    assert_eq!(&transmitted[1][2..], &transmitted[3][2..], "the deferred PPS request must be retried exactly");
}

#[test]
fn spr_avs_is_typed_and_sent_as_an_ordinary_avs_request() {
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let spr_avs = spr_avs_pdo(4_000, 3_000);
    let request = avs_request(2, 33_700, 3_000);
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, spr_avs])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let sink_tx_checks = Arc::new(AtomicUsize::new(0));
    let detached = Arc::new(AtomicBool::new(false));
    let typed_offer_seen = Arc::new(AtomicBool::new(false));
    let driver = ScriptedPpsDriver { receive, transmitted: Arc::clone(&transmitted), sink_tx_checks, detach_after: 1 };
    let dpm = SprAvsDpm { detached: Arc::clone(&detached), typed_offer_seen: Arc::clone(&typed_offer_seen), request };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert!(typed_offer_seen.load(Ordering::SeqCst));

    let transmitted = transmitted.lock().unwrap();
    assert_eq!(transmitted.len(), 1);
    let header = Header::from_bytes(&transmitted[0][..2]).unwrap();
    assert_eq!(header.message_type(), MessageType::Data(DataMessageType::Request));
    let rdo = u32::from_le_bytes(transmitted[0][2..6].try_into().unwrap());
    assert_eq!(rdo >> 28, 2);
    assert_eq!((rdo >> 9) & 0xfff, 1_348, "AVS voltage uses 25 mV wire units");
    assert_eq!(rdo & 0x7f, 60, "3 A uses 50 mA wire units");
}
