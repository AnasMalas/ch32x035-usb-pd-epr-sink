use std::future::{Future, pending};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use usbpd::sink::device_policy_manager::DevicePolicyManager;
use usbpd::sink::policy_engine::{Error as SinkError, Sink};
use usbpd::timers::Timer;
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

struct TestDpm {
    detached: Arc<AtomicBool>,
    protocol_lost: Arc<AtomicBool>,
}

impl DevicePolicyManager for TestDpm {
    async fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    async fn protocol_lost(&mut self) {
        self.protocol_lost.store(true, Ordering::SeqCst);
    }
}

struct DetachedDriver;

impl Driver for DetachedDriver {
    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, _buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        Err(DriverRxError::Detached)
    }

    async fn transmit(&mut self, _data: &[u8]) -> Result<(), DriverTxError> {
        Err(DriverTxError::Detached)
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        Err(DriverTxError::Detached)
    }
}

struct DiscardingDriver {
    receives: Arc<AtomicUsize>,
}

impl Driver for DiscardingDriver {
    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, _buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        self.receives.fetch_add(1, Ordering::SeqCst);
        Err(DriverRxError::Discarded)
    }

    async fn transmit(&mut self, _data: &[u8]) -> Result<(), DriverTxError> {
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        Ok(())
    }
}

#[test]
fn detach_leaves_the_policy_engine_and_notifies_the_product() {
    let detached = Arc::new(AtomicBool::new(false));
    let dpm = TestDpm { detached: Arc::clone(&detached), protocol_lost: Arc::new(AtomicBool::new(false)) };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(DetachedDriver, dpm);

    let result = block_on(sink.run());

    assert!(matches!(result, Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
}

#[test]
fn repeated_discarded_frames_are_bounded_and_restart_the_port() {
    let receives = Arc::new(AtomicUsize::new(0));
    let protocol_lost = Arc::new(AtomicBool::new(false));
    let dpm = TestDpm { detached: Arc::new(AtomicBool::new(false)), protocol_lost: Arc::clone(&protocol_lost) };
    let driver = DiscardingDriver { receives: Arc::clone(&receives) };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    let result = block_on(sink.run());

    assert!(matches!(result, Err(SinkError::PhyUnstable)));
    assert_eq!(receives.load(Ordering::SeqCst), 8);
    assert!(protocol_lost.load(Ordering::SeqCst));
}
