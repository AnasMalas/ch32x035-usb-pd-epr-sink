use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use pd_sink::{
    ControllerConfig, Milliamps, Millivolts, PortMode, Preference, RecoveryIntent, SinkConfig, SinkDevice, SinkEvent,
    SinkPowerDescriptor, SinkRuntime, TransitionLoadPolicy, UserRequest,
};
use usbpd::protocol_layer::message::Message;
use usbpd::protocol_layer::message::data::request::FixedVariableSupply;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, Header, MessageType, SpecificationRevision,
};
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

fn fixed_pdo(voltage_mv: u32, current_ma: u32) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10)
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
        panic!("successful warm recovery must not escalate to Hard Reset")
    }
}

#[derive(Default)]
struct RuntimeState {
    load: Vec<bool>,
    output: Vec<bool>,
    events: Vec<SinkEvent>,
    commands_cleared: usize,
}

struct RecoveryRuntime {
    state: Arc<Mutex<RuntimeState>>,
}

impl SinkRuntime for RecoveryRuntime {
    fn set_pd_load_permitted(&mut self, permitted: bool) {
        self.state.lock().unwrap().load.push(permitted);
    }

    fn set_user_output_enabled(&mut self, enabled: bool) {
        self.state.lock().unwrap().output.push(enabled);
    }

    fn clear_pending_commands(&mut self) {
        self.state.lock().unwrap().commands_cleared += 1;
    }

    fn observe(&mut self, event: SinkEvent) {
        self.state.lock().unwrap().events.push(event);
    }

    async fn wait_for_command(&mut self) -> pd_sink::Command {
        pending().await
    }

    async fn delay_millis(&mut self, _milliseconds: u64) {
        pending().await
    }
}

fn sink_config() -> SinkConfig {
    SinkConfig {
        controller: ControllerConfig::default(),
        descriptor: SinkPowerDescriptor {
            vendor_id: 0x1209,
            product_id: 1,
            maximum_current: Milliamps(3_000),
            pps_supported: false,
            avs_supported: false,
            spr_minimum_pdp_watts: 5,
            spr_operational_pdp_watts: 15,
            spr_maximum_pdp_watts: 15,
            epr_minimum_pdp_watts: 0,
            epr_operational_pdp_watts: 0,
            epr_maximum_pdp_watts: 0,
        },
        transition_load_policy: TransitionLoadPolicy::InhibitUntilReady,
        max_auto_epr_attempts: 0,
        hard_reset_recovery_ms: 2_000,
        sink_ams_guard_ms: 0,
    }
}

#[test]
fn pd_sink_recovery_intent_drives_the_complete_soft_reset_request_and_output_sequence() {
    let fixed_5v = fixed_pdo(5_000, 5_000);
    let fixed_9v = fixed_pdo(9_000, 3_000);
    let receive = VecDeque::from([
        (1, source_control(0, ControlMessageType::Accept)),
        (1, source_capabilities(1, &[fixed_5v, fixed_9v])),
        (2, source_control(2, ControlMessageType::Accept)),
        (2, source_control(3, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let runtime_state = Arc::new(Mutex::new(RuntimeState::default()));
    let driver = ScriptedDriver { receive, transmitted: Arc::clone(&transmitted) };
    let runtime = RecoveryRuntime { state: Arc::clone(&runtime_state) };
    let intent = RecoveryIntent {
        mode: PortMode::Spr,
        request: UserRequest::Voltage {
            voltage: Millivolts(9_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::Fixed,
        },
        restore_output: true,
        maximum_attempts: 2,
    };
    let device = SinkDevice::new_recovering(sink_config(), runtime, intent).unwrap();
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, device);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));

    let transmitted = transmitted.lock().unwrap();
    assert_eq!(transmitted.len(), 2);
    let soft_reset = Message::from_bytes(&transmitted[0]).unwrap();
    assert_eq!(soft_reset.header.message_type(), MessageType::Control(ControlMessageType::SoftReset));
    let request = Message::from_bytes(&transmitted[1]).unwrap();
    assert_eq!(request.header.message_type(), MessageType::Data(DataMessageType::Request));
    let request = FixedVariableSupply(u32::from_le_bytes(transmitted[1][2..6].try_into().unwrap()));
    assert_eq!(request.object_position(), 2);
    assert_eq!(request.operating_current().as_milliamps(), 2_000);

    let runtime = runtime_state.lock().unwrap();
    assert_eq!(runtime.load, [false, false, true, false]);
    assert_eq!(runtime.output, [false, true]);
    assert_eq!(runtime.commands_cleared, 1);
    assert!(runtime.events.contains(&SinkEvent::RecoveryStarted(intent)));
    assert!(runtime.events.iter().any(|event| matches!(
        event,
        SinkEvent::RecoverySucceeded { attempt: 1, plan, output_restored: true }
            if plan.object_position() == 2 && plan.operating_current() == Milliamps(2_000)
    )));
    assert!(runtime.events.contains(&SinkEvent::Detached));
}
