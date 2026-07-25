use std::future::{Future, pending};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::header::{ControlMessageType, MessageType};
use usbpd::protocol_layer::message::header::{DataMessageType, Header, SpecificationRevision};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, HardResetOrigin};
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

enum FirstReceive {
    SourceHardReset,
    InvalidCapabilities,
}

struct OriginDriver {
    first_receive: Option<FirstReceive>,
    hard_resets_sent: Arc<AtomicUsize>,
}

impl Driver for OriginDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        match self.first_receive.take() {
            Some(FirstReceive::SourceHardReset) => Err(DriverRxError::HardReset),
            Some(FirstReceive::InvalidCapabilities) => {
                let header = Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
                    .with_message_type_raw(DataMessageType::SourceCapabilities as u8)
                    .with_num_objects(1);
                header.to_bytes(&mut buffer[..2]);
                let fixed_20v_3a = (400_u32 << 10) | 300;
                buffer[2..6].copy_from_slice(&fixed_20v_3a.to_le_bytes());
                Ok(6)
            }
            None => Err(DriverRxError::Detached),
        }
    }

    async fn transmit(&mut self, _data: &[u8]) -> Result<(), DriverTxError> {
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        self.hard_resets_sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct OriginDpm {
    origins: Arc<Mutex<Vec<HardResetOrigin>>>,
}

impl DevicePolicyManager for OriginDpm {
    async fn hard_reset(&mut self, origin: HardResetOrigin) {
        self.origins.lock().unwrap().push(origin);
    }
}

fn run_origin_trace(first_receive: FirstReceive) -> (Vec<HardResetOrigin>, usize) {
    let origins = Arc::new(Mutex::new(Vec::new()));
    let hard_resets_sent = Arc::new(AtomicUsize::new(0));
    let driver = OriginDriver { first_receive: Some(first_receive), hard_resets_sent: Arc::clone(&hard_resets_sent) };
    let dpm = OriginDpm { origins: Arc::clone(&origins) };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));

    let recorded = origins.lock().unwrap().clone();
    (recorded, hard_resets_sent.load(Ordering::SeqCst))
}

#[test]
fn reports_a_source_initiated_hard_reset() {
    let (origins, sent) = run_origin_trace(FirstReceive::SourceHardReset);
    assert_eq!(origins, [HardResetOrigin::Source]);
    assert_eq!(sent, 0);
}

#[test]
fn reports_a_sink_initiated_hard_reset() {
    let (origins, sent) = run_origin_trace(FirstReceive::InvalidCapabilities);
    assert_eq!(origins, [HardResetOrigin::Sink]);
    assert_eq!(sent, 1);
}

static RECOVERY_LISTENER_ARMED: AtomicUsize = AtomicUsize::new(0);

struct RecoveryTimer;

impl Timer for RecoveryTimer {
    async fn after_millis(milliseconds: u64) {
        if milliseconds == 2_000 {
            RECOVERY_LISTENER_ARMED.fetch_add(1, Ordering::SeqCst);
        }
        pending().await
    }
}

fn source_control(message_id: u8, message_type: ControlMessageType, buffer: &mut [u8]) -> usize {
    let header = Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_id(message_id)
        .with_message_type_raw(message_type as u8);
    header.to_bytes(&mut buffer[..2]);
    2
}

fn source_fixed_capability(message_id: u8, voltage_mv: u32, buffer: &mut [u8]) -> usize {
    let header = Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_id(message_id)
        .with_message_type_raw(DataMessageType::SourceCapabilities as u8)
        .with_num_objects(1);
    header.to_bytes(&mut buffer[..2]);
    let fixed_3a = ((voltage_mv / 50) << 10) | 300;
    buffer[2..6].copy_from_slice(&fixed_3a.to_le_bytes());
    6
}

struct RecoveryDriver {
    receive_step: u8,
    requests_sent: Arc<AtomicUsize>,
    hard_resets_sent: Arc<AtomicUsize>,
}

impl Driver for RecoveryDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        match self.receive_step {
            0 => {
                self.receive_step = 1;
                Ok(source_fixed_capability(0, 20_000, buffer))
            }
            1 if RECOVERY_LISTENER_ARMED.load(Ordering::SeqCst) == 1 => {
                self.receive_step = 2;
                Ok(source_fixed_capability(0, 5_000, buffer))
            }
            2 if self.requests_sent.load(Ordering::SeqCst) == 1 => {
                self.receive_step = 3;
                Ok(source_control(1, ControlMessageType::Accept, buffer))
            }
            3 => {
                self.receive_step = 4;
                Ok(source_control(2, ControlMessageType::PsRdy, buffer))
            }
            4 => Err(DriverRxError::Detached),
            _ => pending().await,
        }
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        let header = Header::from_bytes(&data[..2]).unwrap();
        if header.message_type() == MessageType::Data(DataMessageType::Request) {
            self.requests_sent.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        self.hard_resets_sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct RecoveryDpm {
    hard_resets: Arc<AtomicUsize>,
    recovered: Arc<AtomicUsize>,
    transitions: Arc<AtomicUsize>,
}

impl DevicePolicyManager for RecoveryDpm {
    async fn hard_reset(&mut self, _origin: HardResetOrigin) {
        self.hard_resets.fetch_add(1, Ordering::SeqCst);
    }

    fn hard_reset_recovery_millis(&self) -> u32 {
        2_000
    }

    async fn hard_reset_recovered(&mut self) {
        self.recovered.fetch_add(1, Ordering::SeqCst);
    }

    async fn transition_power(&mut self, _accepted: &usbpd::protocol_layer::message::data::request::PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn hard_reset_recovery_listens_for_fresh_capabilities_during_the_product_window() {
    RECOVERY_LISTENER_ARMED.store(0, Ordering::SeqCst);
    let requests_sent = Arc::new(AtomicUsize::new(0));
    let hard_resets_sent = Arc::new(AtomicUsize::new(0));
    let hard_resets = Arc::new(AtomicUsize::new(0));
    let recovered = Arc::new(AtomicUsize::new(0));
    let transitions = Arc::new(AtomicUsize::new(0));
    let driver = RecoveryDriver {
        receive_step: 0,
        requests_sent: Arc::clone(&requests_sent),
        hard_resets_sent: Arc::clone(&hard_resets_sent),
    };
    let dpm = RecoveryDpm {
        hard_resets: Arc::clone(&hard_resets),
        recovered: Arc::clone(&recovered),
        transitions: Arc::clone(&transitions),
    };
    let mut sink: Sink<_, RecoveryTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert_eq!(RECOVERY_LISTENER_ARMED.load(Ordering::SeqCst), 1);
    assert_eq!(hard_resets_sent.load(Ordering::SeqCst), 1);
    assert_eq!(hard_resets.load(Ordering::SeqCst), 1);
    assert_eq!(recovered.load(Ordering::SeqCst), 1);
    assert_eq!(requests_sent.load(Ordering::SeqCst), 1);
    assert_eq!(transitions.load(Ordering::SeqCst), 1);
}
