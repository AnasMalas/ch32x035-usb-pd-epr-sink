use std::future::{Future, pending};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

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
