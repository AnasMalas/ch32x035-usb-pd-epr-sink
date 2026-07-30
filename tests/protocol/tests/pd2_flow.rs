use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::data::request::{FixedVariableSupply, PowerSource};
use usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities;
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

fn source_header(message_id: u8, message_type: MessageType, num_objects: u8) -> Header {
    let raw_type = match message_type {
        MessageType::Control(message_type) => message_type as u8,
        MessageType::Data(message_type) => message_type as u8,
        MessageType::Extended(message_type) => message_type as u8,
    };
    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R2_0)
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

fn fixed_request(position: u8, current_ma: u32) -> PowerSource {
    let current_10ma = current_ma / 10;
    let rdo = (u32::from(position) << 28) | (1 << 24) | (current_10ma << 10) | current_10ma;
    PowerSource::FixedVariableSupply(FixedVariableSupply(rdo))
}

struct ScriptedPd2Driver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
    sink_tx_checks: Arc<AtomicUsize>,
}

impl Driver for ScriptedPd2Driver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    fn sink_tx_ok(&mut self) -> bool {
        self.sink_tx_checks.fetch_add(1, Ordering::SeqCst);
        false
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let transmitted = self.transmitted.lock().unwrap().len();
        let Some((required_transmits, _)) = self.receive.front() else {
            return if transmitted >= 2 { Err(DriverRxError::Detached) } else { pending().await };
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
        panic!("the PD 2.0 compatibility trace must not hard reset")
    }
}

struct Pd2Dpm {
    detached: Arc<AtomicBool>,
    transitions: Arc<AtomicUsize>,
    initial_request: PowerSource,
    renegotiation: Option<PowerSource>,
}

impl DevicePolicyManager for Pd2Dpm {
    fn request(&mut self, _source_capabilities: &SourceCapabilities) -> PowerSource {
        self.initial_request
    }

    fn transition_power(&mut self, _accepted: &PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    fn get_event(&mut self, _source_capabilities: &SourceCapabilities) -> impl Future<Output = Event> {
        let event = if self.transitions.load(Ordering::SeqCst) == 1 {
            self.renegotiation.take().map(Event::RequestPower)
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
fn pd2_renegotiation_does_not_use_the_pd3_sinktx_gate() {
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let fixed_9v = fixed_pdo(9_000, 3_000);
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v, fixed_9v])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (2, source_control(3, ControlMessageType::Accept)),
        (2, source_control(4, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let sink_tx_checks = Arc::new(AtomicUsize::new(0));
    let detached = Arc::new(AtomicBool::new(false));
    let transitions = Arc::new(AtomicUsize::new(0));
    let driver = ScriptedPd2Driver {
        receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_checks: Arc::clone(&sink_tx_checks),
    };
    let dpm = Pd2Dpm {
        detached: Arc::clone(&detached),
        transitions: Arc::clone(&transitions),
        initial_request: fixed_request(1, 3_000),
        renegotiation: Some(fixed_request(2, 3_000)),
    };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert_eq!(transitions.load(Ordering::SeqCst), 2);
    assert_eq!(sink_tx_checks.load(Ordering::SeqCst), 0, "PD 2.0 must bypass SinkTxOK/SinkTxNG");

    let transmitted = transmitted.lock().unwrap();
    assert_eq!(transmitted.len(), 2);
    for message in transmitted.iter() {
        let header = Header::from_bytes(&message[..2]).unwrap();
        assert_eq!(header.message_type(), MessageType::Data(DataMessageType::Request));
        assert!(matches!(header.spec_revision(), Ok(SpecificationRevision::R2_0)));
    }
    let renegotiation_rdo = FixedVariableSupply(u32::from_le_bytes(transmitted[1][2..6].try_into().unwrap()));
    assert_eq!(renegotiation_rdo.object_position(), 2);
    assert_eq!(renegotiation_rdo.raw_operating_current(), 300);
}
