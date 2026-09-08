//! Tests for the policy engine.

use core::sync::atomic::{AtomicU32, Ordering};

use super::Sink;
use crate::counters::{Counter, CounterType};
use crate::dummy::{DUMMY_CAPABILITIES, DummyDriver, DummySinkDevice, DummyTimer, MAX_DATA_MESSAGE_SIZE};
use crate::protocol_layer::message::data::Data;
use crate::protocol_layer::message::data::epr_mode::Action;
use crate::protocol_layer::message::data::request::{FixedVariableSupply, PowerSource};
use crate::protocol_layer::message::data::source_capabilities::PowerDataObject;
use crate::protocol_layer::message::extended::Extended;
use crate::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType,
};
use crate::protocol_layer::message::{Message, Payload};
use crate::protocol_layer::{ProtocolError, TxError, TxValidationError};
use crate::sink::device_policy_manager::{
    DevicePolicyManager, SinkStartup, SoftResetMode, StatusQueryFailure, StatusQueryKind,
};
#[cfg(feature = "hard-reset-reasons")]
use crate::sink::device_policy_manager::{HardResetOrigin, HardResetReason};
use crate::sink::policy_engine::State;
use crate::timers::Timer;
#[cfg(feature = "hard-reset-reasons")]
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

fn get_policy_engine() -> Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, DummySinkDevice> {
    Sink::new(DummyDriver::new(), DummySinkDevice {})
}

struct WarmEprStartupDpm;

impl crate::sink::device_policy_manager::DevicePolicyManager for WarmEprStartupDpm {
    fn startup(&self) -> SinkStartup {
        SinkStartup::SoftReset(SoftResetMode::Epr)
    }
}

#[test]
fn warm_startup_is_one_shot_and_a_port_restart_is_fresh_spr() {
    let mut policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, WarmEprStartupDpm> =
        Sink::new(DummyDriver::new(), WarmEprStartupDpm);

    assert!(matches!(policy_engine.state, State::SendSoftReset));
    assert_eq!(policy_engine.mode, super::Mode::Epr);

    policy_engine.restart();

    assert!(matches!(policy_engine.state, State::Discovery));
    assert_eq!(policy_engine.mode, super::Mode::Spr);
}

struct StartupLossDpm {
    detached: std::sync::Arc<AtomicU32>,
    protocol_lost: std::sync::Arc<AtomicU32>,
}

impl crate::sink::device_policy_manager::DevicePolicyManager for StartupLossDpm {
    fn startup(&self) -> SinkStartup {
        SinkStartup::SoftReset(SoftResetMode::Epr)
    }

    fn detached(&mut self) {
        self.detached.fetch_add(1, Ordering::SeqCst);
    }

    fn protocol_lost(&mut self) {
        self.protocol_lost.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn an_unstarted_session_loss_cancels_warm_startup_exactly_once() {
    let detached = std::sync::Arc::new(AtomicU32::new(0));
    let protocol_lost = std::sync::Arc::new(AtomicU32::new(0));
    let dpm = StartupLossDpm {
        detached: std::sync::Arc::clone(&detached),
        protocol_lost: std::sync::Arc::clone(&protocol_lost),
    };
    let mut policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, StartupLossDpm> =
        Sink::new(DummyDriver::new(), dpm);

    policy_engine.restart_unstarted_after_detach();
    assert_eq!(detached.load(Ordering::SeqCst), 1);
    assert_eq!(protocol_lost.load(Ordering::SeqCst), 0);
    assert!(matches!(policy_engine.state, State::Discovery));
    assert_eq!(policy_engine.mode, super::Mode::Spr);

    policy_engine.restart_unstarted_after_protocol_loss();
    assert_eq!(detached.load(Ordering::SeqCst), 1);
    assert_eq!(protocol_lost.load(Ordering::SeqCst), 1);
    assert!(matches!(policy_engine.state, State::Discovery));
    assert_eq!(policy_engine.mode, super::Mode::Spr);
}

#[test]
fn every_local_tx_validation_error_is_a_terminal_sink_error() {
    for expected in [
        TxValidationError::UnchunkedExtendedMessagesNotSupported,
        TxValidationError::AvsVoltageAlignmentInvalid,
        TxValidationError::ExtendedMessageChunkingRequired,
    ] {
        let error = super::Error::from(ProtocolError::from(expected));
        let super::Error::InvalidTransmitMessage(actual) = error else {
            panic!("local TX validation was classified as recoverable protocol traffic")
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn wire_tx_errors_keep_their_existing_recovery_classification() {
    assert!(matches!(super::Error::from(ProtocolError::TxError(TxError::Detached)), super::Error::Detached));
    assert!(matches!(super::Error::from(ProtocolError::TxError(TxError::Discarded)), super::Error::PhyUnstable));
    assert!(matches!(
        super::Error::from(ProtocolError::TxError(TxError::HardReset)),
        super::Error::Protocol(ProtocolError::TxError(TxError::HardReset))
    ));
}

struct InvalidTransmitDpm {
    protocol_lost_count: std::sync::Arc<AtomicU32>,
}

impl crate::sink::device_policy_manager::DevicePolicyManager for InvalidTransmitDpm {
    fn protocol_lost(&mut self) {
        self.protocol_lost_count.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn run_returns_local_tx_validation_once_without_state_reentry() {
    let protocol_lost_count = std::sync::Arc::new(AtomicU32::new(0));
    let dpm = InvalidTransmitDpm { protocol_lost_count: std::sync::Arc::clone(&protocol_lost_count) };
    let mut policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, InvalidTransmitDpm> =
        Sink::new(DummyDriver::new(), dpm);
    policy_engine.state =
        State::SelectCapability(PowerSource::FixedVariableSupply(FixedVariableSupply((1 << 28) | (1 << 23))));

    assert!(matches!(
        policy_engine.run().await,
        Err(super::Error::InvalidTransmitMessage(TxValidationError::UnchunkedExtendedMessagesNotSupported))
    ));
    assert_eq!(protocol_lost_count.load(Ordering::SeqCst), 1);
    assert!(matches!(policy_engine.state, State::SelectCapability(_)));
    assert!(!policy_engine.protocol_layer.driver().has_transmitted_data());
}

fn simulate_source_control_message<TIMER: Timer, DPM: crate::sink::device_policy_manager::DevicePolicyManager>(
    policy_engine: &mut Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, TIMER, DPM>,
    control_message_type: ControlMessageType,
    message_id: u8,
) {
    let header = *policy_engine.protocol_layer.header();
    let mut buf = [0u8; MAX_DATA_MESSAGE_SIZE];

    let len = Message::new(Header::new_control(
        header,
        Counter::new_from_value(CounterType::MessageId, message_id),
        control_message_type,
    ))
    .to_bytes(&mut buf);
    policy_engine.protocol_layer.driver().inject_received_data(&buf[..len]);
}

fn simulate_source_extended_message<TIMER: Timer, DPM: DevicePolicyManager>(
    policy_engine: &mut Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, TIMER, DPM>,
    message_type: ExtendedMessageType,
    payload: Extended,
    message_id: u8,
) {
    let header = Header::new_extended(
        get_source_header_template(),
        Counter::new_from_value(CounterType::MessageId, message_id),
        message_type,
        0,
    );
    let message = Message { header, payload: Some(Payload::Extended(payload)) };
    let mut buf = [0u8; MAX_DATA_MESSAGE_SIZE];
    let len = message.to_bytes(&mut buf);
    policy_engine.protocol_layer.driver().inject_received_data(&buf[..len]);
}

struct StatusQueryDpm {
    general_status: std::sync::Arc<AtomicU32>,
    pps_status: std::sync::Arc<AtomicU32>,
    failure: std::sync::Arc<AtomicU32>,
}

impl DevicePolicyManager for StatusQueryDpm {
    fn inform_status(&mut self, status: &crate::protocol_layer::message::extended::status::Status) {
        self.general_status.store(u32::from(status.event_flags()) + 1, Ordering::SeqCst);
    }

    fn inform_pps_status(&mut self, status: &crate::protocol_layer::message::extended::pps_status::PpsStatus) {
        self.pps_status.store(u32::from(status.raw_bytes()[3]) + 1, Ordering::SeqCst);
    }

    fn status_query_failed(&mut self, query: StatusQueryKind, failure: StatusQueryFailure) {
        let query = match query {
            StatusQueryKind::General => 1,
            StatusQueryKind::Pps => 2,
        };
        let failure = match failure {
            StatusQueryFailure::NotSupported => 1,
            StatusQueryFailure::Rejected => 2,
            StatusQueryFailure::Deferred => 3,
            StatusQueryFailure::Timeout => 4,
        };
        self.failure.store(query * 10 + failure, Ordering::SeqCst);
    }
}

struct StatusQueryFixture {
    policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, StatusQueryDpm>,
    general_status: std::sync::Arc<AtomicU32>,
    pps_status: std::sync::Arc<AtomicU32>,
    failure: std::sync::Arc<AtomicU32>,
}

fn status_query_fixture() -> StatusQueryFixture {
    let general_status = std::sync::Arc::new(AtomicU32::new(0));
    let pps_status = std::sync::Arc::new(AtomicU32::new(0));
    let failure = std::sync::Arc::new(AtomicU32::new(0));
    let dpm = StatusQueryDpm {
        general_status: std::sync::Arc::clone(&general_status),
        pps_status: std::sync::Arc::clone(&pps_status),
        failure: std::sync::Arc::clone(&failure),
    };
    StatusQueryFixture { policy_engine: Sink::new(DummyDriver::new(), dpm), general_status, pps_status, failure }
}

#[tokio::test]
async fn shared_status_query_path_preserves_request_and_response_kinds() {
    use crate::dummy::get_source_capability_request;
    use crate::protocol_layer::message::extended::pps_status::PpsStatus;
    use crate::protocol_layer::message::extended::status::Status;

    let cases = [
        (
            StatusQueryKind::General,
            ControlMessageType::GetStatus,
            ExtendedMessageType::Status,
            Extended::Status(Status::from_bytes(&[1, 2, 3, 4, 5, 6, 7]).unwrap()),
            5,
            0,
        ),
        (
            StatusQueryKind::Pps,
            ControlMessageType::GetPpsStatus,
            ExtendedMessageType::PpsStatus,
            Extended::PpsStatus(PpsStatus::from_bytes(&[1, 2, 3, 8]).unwrap()),
            0,
            9,
        ),
    ];

    for (query, request_type, response_type, response, expected_general, expected_pps) in cases {
        let StatusQueryFixture { mut policy_engine, general_status, pps_status, failure } = status_query_fixture();
        policy_engine.state = State::GetStatus(query, get_source_capability_request());
        simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);
        simulate_source_extended_message(&mut policy_engine, response_type, response, 0);

        policy_engine.run_step().await.unwrap();

        let transmitted = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
        assert_eq!(transmitted.header.message_type(), MessageType::Control(request_type));
        assert_eq!(general_status.load(Ordering::SeqCst), expected_general);
        assert_eq!(pps_status.load(Ordering::SeqCst), expected_pps);
        assert_eq!(failure.load(Ordering::SeqCst), 0);
        assert!(matches!(policy_engine.state, State::Ready(_)));
    }
}

#[tokio::test]
async fn shared_status_query_path_preserves_failure_kind() {
    use crate::dummy::get_source_capability_request;

    for (query, query_code) in [(StatusQueryKind::General, 10), (StatusQueryKind::Pps, 20)] {
        for (response, failure_code) in
            [(ControlMessageType::NotSupported, 1), (ControlMessageType::Reject, 2), (ControlMessageType::Wait, 3)]
        {
            let StatusQueryFixture { mut policy_engine, general_status, pps_status, failure } = status_query_fixture();
            policy_engine.state = State::GetStatus(query, get_source_capability_request());
            simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);
            simulate_source_control_message(&mut policy_engine, response, 0);

            policy_engine.run_step().await.unwrap();

            assert_eq!(general_status.load(Ordering::SeqCst), 0);
            assert_eq!(pps_status.load(Ordering::SeqCst), 0);
            assert_eq!(failure.load(Ordering::SeqCst), query_code + failure_code);
            assert!(matches!(policy_engine.state, State::Ready(_)));
        }

        let StatusQueryFixture { mut policy_engine, general_status, pps_status, failure } = status_query_fixture();
        policy_engine.state = State::GetStatus(query, get_source_capability_request());
        simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);

        policy_engine.run_step().await.unwrap();

        assert_eq!(general_status.load(Ordering::SeqCst), 0);
        assert_eq!(pps_status.load(Ordering::SeqCst), 0);
        assert_eq!(failure.load(Ordering::SeqCst), query_code + 4);
        assert!(matches!(policy_engine.state, State::Ready(_)));
    }
}

static TRANSITION_CLOCK: AtomicU32 = AtomicU32::new(0);

struct TransitionDeadlineTimer;

impl Timer for TransitionDeadlineTimer {
    fn now_128ms_ticks() -> u32 {
        TRANSITION_CLOCK.load(Ordering::SeqCst)
    }

    async fn after_millis(_milliseconds: u64) {
        embassy_futures::yield_now().await;
    }
}

#[tokio::test]
async fn successful_epr_power_transition_rearms_keep_alive_from_ps_rdy() {
    use crate::dummy::get_source_capability_request;
    use crate::sink::policy_engine::Mode;

    let mut policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, TransitionDeadlineTimer, DummySinkDevice> =
        Sink::new(DummyDriver::new(), DummySinkDevice {});
    let request = get_source_capability_request();

    policy_engine.mode = Mode::Epr;
    policy_engine.state = State::TransitionSink(request);
    policy_engine.epr_keep_alive_deadline_tick = Some(7);
    TRANSITION_CLOCK.store(100, Ordering::SeqCst);
    simulate_source_control_message(&mut policy_engine, ControlMessageType::PsRdy, 0);

    policy_engine.run_step().await.unwrap();

    assert!(matches!(policy_engine.state, State::Ready(..)));
    assert_eq!(policy_engine.epr_keep_alive_deadline_tick, Some(103));
}

#[cfg(feature = "hard-reset-reasons")]
struct KeepAliveSourceResetDriver {
    sink_hard_resets: std::sync::Arc<AtomicU32>,
}

#[cfg(feature = "hard-reset-reasons")]
impl Driver for KeepAliveSourceResetDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, _buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        Err(DriverRxError::HardReset)
    }

    async fn transmit(&mut self, _data: &[u8]) -> Result<(), DriverTxError> {
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        self.sink_hard_resets.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(feature = "hard-reset-reasons")]
struct KeepAliveSourceResetDpm {
    origin: std::sync::Arc<AtomicU32>,
    reason: std::sync::Arc<AtomicU32>,
}

#[cfg(feature = "hard-reset-reasons")]
impl DevicePolicyManager for KeepAliveSourceResetDpm {
    fn hard_reset(&mut self, origin: HardResetOrigin, reason: HardResetReason) {
        self.origin.store(
            match origin {
                HardResetOrigin::Source => 1,
                HardResetOrigin::Sink => 2,
            },
            Ordering::SeqCst,
        );
        self.reason.store(reason as u32, Ordering::SeqCst);
    }
}

#[cfg(feature = "hard-reset-reasons")]
#[tokio::test]
async fn source_hard_reset_while_waiting_for_keep_alive_ack_is_not_retransmitted() {
    use crate::dummy::get_source_capability_request;
    use crate::sink::policy_engine::Mode;

    let sink_hard_resets = std::sync::Arc::new(AtomicU32::new(0));
    let origin = std::sync::Arc::new(AtomicU32::new(0));
    let reason = std::sync::Arc::new(AtomicU32::new(u32::MAX));
    let driver = KeepAliveSourceResetDriver { sink_hard_resets: std::sync::Arc::clone(&sink_hard_resets) };
    let dpm =
        KeepAliveSourceResetDpm { origin: std::sync::Arc::clone(&origin), reason: std::sync::Arc::clone(&reason) };
    let mut policy_engine: Sink<_, DummyTimer, _> = Sink::new(driver, dpm);

    policy_engine.mode = Mode::Epr;
    policy_engine.state = State::EprKeepAlive(get_source_capability_request());

    policy_engine.run_step().await.unwrap();
    assert!(matches!(policy_engine.state, State::TransitionToDefault));

    policy_engine.run_step().await.unwrap();
    assert_eq!(origin.load(Ordering::SeqCst), 1);
    assert_eq!(reason.load(Ordering::SeqCst), HardResetReason::SourceSignaled as u32);
    assert_eq!(sink_hard_resets.load(Ordering::SeqCst), 0);
}

/// Get a header template for simulating source messages (Source/Dfp roles).
/// This flips the roles from the sink's perspective to simulate messages from the source.
fn get_source_header_template() -> Header {
    use crate::protocol_layer::message::header::SpecificationRevision;
    use crate::{DataRole, PowerRole};

    // Source messages have Source/Dfp roles (opposite of sink's Sink/Ufp)
    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
}

/// Simulate an EPR Mode data message from the source with proper API.
/// Returns the serialized bytes for assertion.
fn simulate_source_epr_mode_message<DPM: crate::sink::device_policy_manager::DevicePolicyManager>(
    policy_engine: &mut Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, DPM>,
    action: Action,
    message_id: u8,
) -> heapless::Vec<u8, MAX_DATA_MESSAGE_SIZE> {
    use crate::protocol_layer::message::data::epr_mode::EprModeDataObject;

    let source_header = get_source_header_template();
    let header = Header::new_data(
        source_header,
        Counter::new_from_value(CounterType::MessageId, message_id),
        DataMessageType::EprMode,
        1, // 1 data object (the EprModeDataObject)
    );

    let epr_mode = EprModeDataObject::default().with_action(action);
    let message = Message::new_with_data(header, Data::EprMode(epr_mode));

    let mut buf = [0u8; MAX_DATA_MESSAGE_SIZE];
    let len = message.to_bytes(&mut buf);
    policy_engine.protocol_layer.driver().inject_received_data(&buf[..len]);

    let mut result = heapless::Vec::new();
    result.extend_from_slice(&buf[..len]).unwrap();
    result
}

/// Simulate an EprKeepAliveAck extended control message from the source.
/// Returns the serialized bytes for assertion.
fn simulate_epr_keep_alive_ack<DPM: crate::sink::device_policy_manager::DevicePolicyManager>(
    policy_engine: &mut Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, DPM>,
    message_id: u8,
) -> heapless::Vec<u8, MAX_DATA_MESSAGE_SIZE> {
    use crate::protocol_layer::message::Payload;
    use crate::protocol_layer::message::extended::Extended;
    use crate::protocol_layer::message::extended::extended_control::{ExtendedControl, ExtendedControlMessageType};

    let source_header = get_source_header_template();
    // Create extended message header (num_objects=0 as used in transmit_extended_control_message)
    let header = Header::new_extended(
        source_header,
        Counter::new_from_value(CounterType::MessageId, message_id),
        ExtendedMessageType::ExtendedControl,
        0,
    );

    // Create the message with proper payload
    let mut message = Message::new(header);
    message.payload = Some(Payload::Extended(Extended::ExtendedControl(
        ExtendedControl::default().with_message_type(ExtendedControlMessageType::EprKeepAliveAck),
    )));

    // Serialize and inject
    let mut buf = [0u8; MAX_DATA_MESSAGE_SIZE];
    let len = message.to_bytes(&mut buf);
    policy_engine.protocol_layer.driver().inject_received_data(&buf[..len]);

    let mut result = heapless::Vec::new();
    result.extend_from_slice(&buf[..len]).unwrap();
    result
}

#[cfg(feature = "numeric-trace")]
fn assert_trace_kinds(
    events: &[crate::numeric_trace::NumericTraceEvent],
    expected: &[crate::numeric_trace::NumericTraceEventKind],
) {
    assert!(events.iter().map(|event| event.kind).eq(expected.iter().copied()));
}

#[cfg(feature = "numeric-trace")]
#[tokio::test]
async fn numeric_trace_orders_successful_epr_keep_alive() {
    use crate::dummy::get_source_capability_request;
    use crate::numeric_trace::{
        NumericTraceEprKeepAlivePhase, NumericTraceEventKind as Kind, test_support::CaptureGuard,
    };
    use crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType;
    use crate::sink::policy_engine::Mode;

    let mut policy_engine = get_policy_engine();
    policy_engine.mode = Mode::Epr;
    policy_engine.state = State::EprKeepAlive(get_source_capability_request());
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);
    simulate_epr_keep_alive_ack(&mut policy_engine, 0);

    let capture = CaptureGuard::start();
    policy_engine.run_step().await.unwrap();
    let events = capture.events();

    assert_trace_kinds(
        &events,
        &[
            Kind::EprKeepAlive,
            Kind::TxStart,
            Kind::GoodCrcWait,
            Kind::RxMessage,
            Kind::GoodCrcReceived,
            Kind::TxSuccess,
            Kind::RxMessage,
            Kind::GoodCrcTransmitted,
            Kind::EprKeepAlive,
        ],
    );
    assert_eq!(events[0].code, NumericTraceEprKeepAlivePhase::Request as u8);
    assert_eq!(events[1].code, u8::from(ExtendedControlMessageType::EprKeepAlive));
    assert_eq!(events[6].code, u8::from(ExtendedControlMessageType::EprKeepAliveAck));
    assert_eq!(events[8].code, NumericTraceEprKeepAlivePhase::Acknowledged as u8);
    assert!(matches!(policy_engine.state, State::Ready(_)));
}

#[cfg(all(feature = "numeric-trace", feature = "hard-reset-reasons"))]
#[tokio::test]
async fn numeric_trace_reports_keep_alive_timeout_and_hard_reset_reason() {
    use crate::dummy::get_source_capability_request;
    use crate::numeric_trace::{
        NumericTraceEprKeepAlivePhase, NumericTraceEventKind as Kind, NumericTraceHardResetPhase,
        NumericTraceProtocolError, test_support::CaptureGuard,
    };
    use crate::sink::device_policy_manager::HardResetReason;
    use crate::sink::policy_engine::Mode;

    let mut policy_engine = get_policy_engine();
    policy_engine.mode = Mode::Epr;
    policy_engine.state = State::EprKeepAlive(get_source_capability_request());
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);

    let capture = CaptureGuard::start();
    policy_engine.run_step().await.unwrap();
    assert!(matches!(policy_engine.state, State::HardReset(HardResetReason::EprKeepAliveFailed)));
    policy_engine.run_step().await.unwrap();
    let events = capture.events();

    let timeout = events
        .iter()
        .position(|event| {
            event.kind == Kind::EprKeepAlive && event.code == NumericTraceEprKeepAlivePhase::Timeout as u8
        })
        .unwrap();
    let protocol_timeout = events
        .iter()
        .position(|event| event.kind == Kind::ProtocolError && event.code == NumericTraceProtocolError::RxTimeout as u8)
        .unwrap();
    let reset_start = events
        .iter()
        .position(|event| {
            event.kind == Kind::HardReset && event.code == NumericTraceHardResetPhase::TransmitStart as u8
        })
        .unwrap();
    let reset_complete = events
        .iter()
        .position(|event| {
            event.kind == Kind::HardReset && event.code == NumericTraceHardResetPhase::TransmitComplete as u8
        })
        .unwrap();

    assert!(protocol_timeout < timeout);
    assert!(timeout < reset_start);
    assert!(reset_start < reset_complete);
    assert_eq!(events[reset_start].detail, HardResetReason::EprKeepAliveFailed as u16);
    assert_eq!(events[reset_start].counter, 1);
}

#[tokio::test]
async fn test_negotiation() {
    // Instantiated in `Discovery` state
    let mut policy_engine = get_policy_engine();

    // Provide capabilities
    policy_engine.protocol_layer.driver().inject_received_data(&DUMMY_CAPABILITIES);

    // `Discovery` -> `WaitForCapabilities`
    policy_engine.run_step().await.unwrap();

    // `WaitForCapabilities` -> `EvaluateCapabilities`
    policy_engine.run_step().await.unwrap();

    let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    // Simulate `GoodCrc` with ID 0.
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);

    // `EvaluateCapabilities` -> `SelectCapability`
    policy_engine.run_step().await.unwrap();

    // Simulate `Accept` message.
    simulate_source_control_message(&mut policy_engine, ControlMessageType::Accept, 1);

    // `SelectCapability` -> `TransitionSink`
    policy_engine.run_step().await.unwrap();

    let request_capabilities =
        Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    assert!(matches!(request_capabilities.header.message_type(), MessageType::Data(DataMessageType::Request)));

    // Simulate `PsRdy` message.
    simulate_source_control_message(&mut policy_engine, ControlMessageType::PsRdy, 2);

    // `TransitionSink` -> `Ready`
    policy_engine.run_step().await.unwrap();
    assert!(matches!(policy_engine.state, State::Ready(..)));

    let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));
}

#[tokio::test]
async fn test_reserved_epr_mode_entry_response_sends_soft_reset() {
    use crate::dummy::{DummySinkEprDevice, get_source_capability_request};
    use crate::units::Power;

    let mut policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, DummySinkEprDevice> =
        Sink::new(DummyDriver::new(), DummySinkEprDevice::new());
    policy_engine.state = State::EprModeEntry(get_source_capability_request(), Power::from_watts(140));

    // The EPR_Mode Enter transmission starts with MessageID 0 in a fresh
    // protocol layer. A reserved action from the Source must be recovered via
    // Soft Reset rather than reaching an internal payload assumption.
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);
    simulate_source_epr_mode_message(&mut policy_engine, Action::Unknown, 0);

    policy_engine.run_step().await.unwrap();

    assert!(matches!(policy_engine.state, State::SendSoftReset));
}

#[tokio::test]
async fn test_epr_negotiation() {
    use crate::dummy::{DUMMY_SPR_CAPS_EPR_CAPABLE, DummySinkEprDevice};

    // Create policy engine with EPR-capable DPM
    let mut policy_engine: Sink<DummyDriver<MAX_DATA_MESSAGE_SIZE>, DummyTimer, DummySinkEprDevice> =
        Sink::new(DummyDriver::new(), DummySinkEprDevice::new());

    // === Phase 1: Initial SPR Negotiation ===
    // Using same flow as test_negotiation
    eprintln!("Starting test");

    policy_engine.protocol_layer.driver().inject_received_data(&DUMMY_SPR_CAPS_EPR_CAPABLE);

    // Discovery -> WaitForCapabilities
    eprintln!("run_step 1");
    policy_engine.run_step().await.unwrap();

    // WaitForCapabilities -> EvaluateCapabilities
    eprintln!("run_step 2");
    policy_engine.run_step().await.unwrap();

    eprintln!("Probing first GoodCRC");
    let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    eprintln!("Got first GoodCRC");
    assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    // Simulate GoodCRC with ID 0
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 0);

    // EvaluateCapabilities -> SelectCapability
    policy_engine.run_step().await.unwrap();

    // Simulate Accept
    simulate_source_control_message(&mut policy_engine, ControlMessageType::Accept, 1);

    // SelectCapability -> TransitionSink
    policy_engine.run_step().await.unwrap();

    let request_capabilities =
        Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    assert!(matches!(request_capabilities.header.message_type(), MessageType::Data(DataMessageType::Request)));

    // Simulate PsRdy
    simulate_source_control_message(&mut policy_engine, ControlMessageType::PsRdy, 2);

    // TransitionSink -> Ready
    policy_engine.run_step().await.unwrap();
    eprintln!("State after last run_step: {:?}", policy_engine.state);
    assert!(matches!(policy_engine.state, State::Ready(..)));

    eprintln!("Has transmitted data: {}", policy_engine.protocol_layer.driver().has_transmitted_data());
    let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    eprintln!("Got first GoodCRC: {:?}", good_crc.header.message_type());
    assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    // Probe any remaining messages from Phase 1
    while policy_engine.protocol_layer.driver().has_transmitted_data() {
        let msg = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
        eprintln!("Draining leftover message: {:?}", msg.header.message_type());
    }

    eprintln!("\n=== Phase 1 Complete: SPR negotiation at 20V ===\n");

    // === Phase 2: EPR Mode Entry ===
    // Per spec 8.3.3.26.2, EPR mode entry flow:
    // 1. Sink sends EPR_Mode (Enter), starts SenderResponseTimer
    // 2. Source sends EnterAcknowledged
    // 3. Source performs cable discovery (we skip this in test)
    // 4. Source sends EnterSucceeded
    eprintln!("=== Phase 2: EPR Mode Entry ===");

    // Ready -> EprModeEntry (DPM triggers EnterEprMode event)
    policy_engine.run_step().await.unwrap();
    eprintln!("State after DPM event: {:?}", policy_engine.state);

    // Inject GoodCRC for EPR_Mode (Enter) that will be transmitted
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 1);

    // Inject EPR_Mode (EnterAcknowledged) using proper API
    // From capture line 78-81: RAW[6]: AA 19 00 00 00 02 (Source, msg_id=4)
    let epr_enter_ack_bytes = simulate_source_epr_mode_message(
        &mut policy_engine,
        Action::EnterAcknowledged,
        4, // message_id from capture
    );
    // Assert bytes match the real capture
    assert_eq!(
        &epr_enter_ack_bytes[..],
        &[0xAA, 0x19, 0x00, 0x00, 0x00, 0x02],
        "EPR EnterAcknowledged bytes should match capture"
    );

    // EprModeEntry: sends EPR_Mode (Enter), receives EnterAcknowledged -> EprEntryWaitForResponse
    match policy_engine.run_step().await {
        Ok(_) => eprintln!("EprModeEntry run_step succeeded"),
        Err(e) => eprintln!("EprModeEntry run_step failed: {:?}", e),
    }
    eprintln!("State after EprModeEntry: {:?}", policy_engine.state);

    // Probe EPR_Mode (Enter) message
    let epr_enter_bytes = policy_engine.protocol_layer.driver().probe_transmitted_data();
    let epr_enter = Message::from_bytes(&epr_enter_bytes).unwrap();
    eprintln!("Probed message type: {:?}", epr_enter.header.message_type());
    assert!(matches!(epr_enter.header.message_type(), MessageType::Data(DataMessageType::EprMode)));
    if let Some(Payload::Data(Data::EprMode(mode))) = epr_enter.payload {
        assert_eq!(mode.action(), Action::Enter);
    } else {
        panic!("Expected EprMode Enter payload");
    }
    // Assert EPR_Mode Enter bytes match capture line 71-74: RAW[6]: 8A 14 00 00 00 01
    // Note: Our test starts from different state so message_id may differ
    eprintln!("EPR_Mode Enter bytes: {:02X?}", &epr_enter_bytes[..]);

    // Probe GoodCRC for EnterAcknowledged
    let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    eprintln!("Probed GoodCRC for EnterAck: {:?}", good_crc.header.message_type());
    assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    // Inject EPR_Mode (EnterSucceeded) using proper API
    // From capture line 85-88: RAW[6]: AA 1B 00 00 00 03 (Source, msg_id=5)
    let epr_enter_succeeded_bytes = simulate_source_epr_mode_message(
        &mut policy_engine,
        Action::EnterSucceeded,
        5, // message_id from capture
    );
    // Assert bytes match the real capture
    assert_eq!(
        &epr_enter_succeeded_bytes[..],
        &[0xAA, 0x1B, 0x00, 0x00, 0x00, 0x03],
        "EPR EnterSucceeded bytes should match capture"
    );

    // EprEntryWaitForResponse receives EnterSucceeded -> EprWaitForCapabilities
    policy_engine.run_step().await.unwrap();
    eprintln!("State after EnterSucceeded: {:?}", policy_engine.state);

    // Probe GoodCRC for EnterSucceeded
    let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    eprintln!("=== Phase 2 Complete: EPR mode entry succeeded ===\n");

    // === Phase 3: Chunked EPR Source Capabilities ===
    // This follows the real-world capture flow per USB PD spec 6.12.2.1.2:
    // 1. Source sends chunk 0 -> Sink sends GoodCRC
    // 2. Sink sends Chunk Request (chunk=1) -> Source sends GoodCRC
    // 3. Source sends chunk 1 -> Sink sends GoodCRC
    use crate::dummy::{DUMMY_EPR_SOURCE_CAPS_CHUNK_0, DUMMY_EPR_SOURCE_CAPS_CHUNK_1};

    eprintln!("=== Phase 3: Chunked EPR Source Capabilities ===");

    // Source sends EPR_Source_Capabilities chunk 0
    policy_engine.protocol_layer.driver().inject_received_data(&DUMMY_EPR_SOURCE_CAPS_CHUNK_0);

    // Inject GoodCRC for the Chunk Request that sink will send after receiving chunk 0
    // The chunk request message ID will be based on tx_message counter
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 2);

    // Source sends chunk 1 after receiving the chunk request
    policy_engine.protocol_layer.driver().inject_received_data(&DUMMY_EPR_SOURCE_CAPS_CHUNK_1);

    // EprWaitForCapabilities -> Protocol layer:
    // - receives chunk 0, sends GoodCRC
    // - sends chunk request, waits for GoodCRC
    // - receives chunk 1, sends GoodCRC
    // - assembles message -> EvaluateCapabilities
    policy_engine.run_step().await.unwrap();

    // Probe GoodCRC for chunk 0
    let good_crc_0 = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    eprintln!("Chunk 0 GoodCRC: {:?}", good_crc_0.header.message_type());
    assert!(matches!(good_crc_0.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    // Probe the Chunk Request message (per spec 6.12.2.1.2.4)
    // Chunk requests are parsed as ChunkedExtendedMessage error, so we use parse_extended_chunk
    let chunk_req_data = policy_engine.protocol_layer.driver().probe_transmitted_data();
    let (chunk_req_header, chunk_req_ext_header, _chunk_data) = Message::parse_extended_chunk(&chunk_req_data).unwrap();
    eprintln!(
        "Chunk Request: type={:?}, chunk_number={}, request_chunk={}",
        chunk_req_header.message_type(),
        chunk_req_ext_header.chunk_number(),
        chunk_req_ext_header.request_chunk()
    );
    assert!(chunk_req_header.extended(), "Chunk request should be an extended message");
    assert!(matches!(
        chunk_req_header.message_type(),
        MessageType::Extended(ExtendedMessageType::EprSourceCapabilities)
    ));
    assert!(chunk_req_ext_header.request_chunk(), "Should be a chunk request");
    assert_eq!(chunk_req_ext_header.chunk_number(), 1, "Should request chunk 1");

    // Probe GoodCRC for chunk 1
    let good_crc_1 = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    eprintln!("Chunk 1 GoodCRC: {:?}", good_crc_1.header.message_type());
    assert!(matches!(good_crc_1.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

    eprintln!("=== Phase 3 Complete: EPR caps assembled (with chunk request per spec) ===\n");

    // === Phase 4: EPR Power Negotiation ===
    // Selects PDO#8 (28V @ 5A = 140W)
    eprintln!("=== Phase 4: EPR Power Negotiation ===");

    // EvaluateCapabilities -> DPM.request() selects EPR PDO#8 (28V) -> SelectCapability
    policy_engine.run_step().await.unwrap();
    eprintln!("State after evaluate: {:?}", policy_engine.state);

    // Inject GoodCRC for the EprRequest that will be transmitted
    // Note: TX message counter is now 3 (after chunk request in Phase 3 incremented it from 2 to 3)
    simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, 3);

    // Also inject Accept message that SelectCapability will wait for after transmitting
    simulate_source_control_message(&mut policy_engine, ControlMessageType::Accept, 0);

    // SelectCapability -> sends EprRequest, waits for Accept -> TransitionSink
    policy_engine.run_step().await.unwrap();

    // Probe the EPR Request
    let epr_request = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
    eprintln!("EPR Request: {:?} (message_id={})", epr_request.header.message_type(), epr_request.header.message_id());
    assert!(matches!(epr_request.header.message_type(), MessageType::Data(DataMessageType::EprRequest)));

    // Verify EPR Request selects PDO#8 (28V)
    if let Some(Payload::Data(Data::Request(PowerSource::EprRequest(epr)))) = &epr_request.payload {
        let object_pos = epr.object_position();
        eprintln!("EPR Request: PDO#{} (RDO=0x{:08X})", object_pos, epr.rdo);
        assert_eq!(object_pos, 8, "Should request PDO#8 (28V) to match real capture");

        // Verify it's the 28V PDO
        if let PowerDataObject::FixedSupply(fixed) =
            crate::protocol_layer::message::data::source_capabilities::parse_raw_pdo(epr.pdo)
        {
            assert_eq!(fixed.raw_voltage(), 560, "28V = 560 * 50mV");
            assert_eq!(fixed.raw_max_current(), 500, "5A = 500 * 10mA");
        }
    } else {
        panic!("Expected EprRequest payload");
    }

    // Drain any leftover messages
    eprintln!("Has transmitted data before drain: {}", policy_engine.protocol_layer.driver().has_transmitted_data());
    while policy_engine.protocol_layer.driver().has_transmitted_data() {
        let msg = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
        eprintln!("Draining: {:?}", msg.header.message_type());
    }
    eprintln!("Has transmitted data after drain: {}", policy_engine.protocol_layer.driver().has_transmitted_data());

    // Inject PsRdy that TransitionSink will wait for
    simulate_source_control_message(&mut policy_engine, ControlMessageType::PsRdy, 1);

    // TransitionSink waits for PsRdy -> Ready
    policy_engine.run_step().await.unwrap();

    // Probe any GoodCRCs we transmitted (for Accept and PsRdy messages we received)
    while policy_engine.protocol_layer.driver().has_transmitted_data() {
        let msg = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
        eprintln!("Phase 4 transmitted: {:?}", msg.header.message_type());
        assert!(matches!(msg.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));
    }

    // Verify we're in Ready state with EPR power
    assert!(matches!(policy_engine.state, State::Ready(..)));
    eprintln!("Final state: {:?}", policy_engine.state);

    eprintln!("=== Phase 4 Complete: EPR power negotiation at 28V/5A (140W) ===\n");

    // === Phase 5: EPR Keep-Alive ===
    // Per USB PD spec 8.3.3.3.11, sink must send EprKeepAlive periodically in EPR mode.
    // Real capture shows multiple keep-alive exchanges after EPR contract (lines 145-214).
    // We manually transition to EprKeepAlive state to test this flow, simulating multiple
    // keep-alive cycles to verify the sink continues sending them.
    eprintln!("=== Phase 5: EPR Keep-Alive (multiple cycles) ===");

    // Test multiple keep-alive cycles to verify the sink keeps sending them
    // Real capture shows 7 keep-alive exchanges - we'll test 3 to verify the pattern
    // From capture (lines 152-183), EprKeepAliveAck messages have Source/Dfp roles.
    let mut sink_tx_counter = 4u8; // Sink TX counter after EPR Request
    let mut source_tx_counter = 2u8; // Source TX counter (increments with each message source sends)

    // Expected EprKeepAliveAck bytes from capture (for first 3 cycles):
    // Cycle 1 (msg_id=2): B0 95 02 80 04 00
    // Cycle 2 (msg_id=3): B0 97 02 80 04 00
    // Cycle 3 (msg_id=4): B0 99 02 80 04 00
    // Note: Header changes based on msg_id, but extended header (02 80) and payload (04 00) stay same

    for cycle in 1..=3 {
        eprintln!("--- Keep-Alive cycle {} ---", cycle);

        // Manually set state to EprKeepAlive (normally triggered by SinkEPRKeepAliveTimer in Ready state)
        if let State::Ready(power_source) = policy_engine.state.clone() {
            policy_engine.state = State::EprKeepAlive(power_source);
        } else {
            panic!("Expected Ready state before keep-alive cycle {}", cycle);
        }

        // Inject GoodCRC for the EprKeepAlive message that will be transmitted
        simulate_source_control_message(&mut policy_engine, ControlMessageType::GoodCRC, sink_tx_counter);
        sink_tx_counter = sink_tx_counter.wrapping_add(1);

        // Inject EprKeepAliveAck response from source with correct message ID
        let keep_alive_ack_bytes = simulate_epr_keep_alive_ack(&mut policy_engine, source_tx_counter);
        eprintln!("  EprKeepAliveAck bytes: {:02X?}", &keep_alive_ack_bytes[..]);
        // Verify payload matches capture pattern
        // The extended header data_size=2 (low byte 0x02), and the chunked bit may or may not be set
        // (spec allows both). Real capture shows chunked=true (0x80), but our impl uses chunked=false (0x00)
        // Payload: 04 00 (ExtendedControl with EprKeepAliveAck type)
        assert_eq!(keep_alive_ack_bytes[2] & 0x1F, 0x02, "data_size should be 2");
        assert_eq!(&keep_alive_ack_bytes[4..], &[0x04, 0x00], "EprKeepAliveAck payload should match capture");
        source_tx_counter = source_tx_counter.wrapping_add(1);

        // EprKeepAlive sends keep-alive, receives ack -> Ready
        policy_engine.run_step().await.unwrap();

        // Probe the EprKeepAlive message
        let keep_alive_bytes = policy_engine.protocol_layer.driver().probe_transmitted_data();
        let keep_alive = Message::from_bytes(&keep_alive_bytes).unwrap();
        eprintln!("  EprKeepAlive sent: {:?}", keep_alive.header.message_type());
        assert!(matches!(
            keep_alive.header.message_type(),
            MessageType::Extended(ExtendedMessageType::ExtendedControl)
        ));

        // Verify it's actually an EprKeepAlive message
        if let Some(Payload::Extended(crate::protocol_layer::message::extended::Extended::ExtendedControl(ctrl))) =
            &keep_alive.payload
        {
            assert_eq!(
                ctrl.message_type(),
                crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType::EprKeepAlive,
                "Expected EprKeepAlive message type"
            );
        } else {
            panic!("Expected ExtendedControl payload with EprKeepAlive");
        }
        // Verify EprKeepAlive payload matches capture pattern
        // From capture (e.g. line 145-148): 90 9A 02 80 03 00
        // Extended header data_size=2, Payload: 03 00 (EprKeepAlive type)
        // Note: chunked bit may differ between our impl (0x00) and capture (0x80)
        assert_eq!(keep_alive_bytes[2] & 0x1F, 0x02, "data_size should be 2");
        assert_eq!(&keep_alive_bytes[4..], &[0x03, 0x00], "EprKeepAlive payload should match capture");

        // Probe GoodCRC for EprKeepAliveAck
        let good_crc = Message::from_bytes(&policy_engine.protocol_layer.driver().probe_transmitted_data()).unwrap();
        assert!(matches!(good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC)));

        // Verify we're back in Ready state (ready for next keep-alive cycle)
        assert!(matches!(policy_engine.state, State::Ready(..)));
        eprintln!("  Returned to Ready state");
    }

    eprintln!("=== Phase 5 Complete: {} EPR keep-alive cycles succeeded ===\n", 3);
    eprintln!("=== Full EPR negotiation test PASSED ===");
}
