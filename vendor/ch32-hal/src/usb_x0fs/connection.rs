use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;

use embassy_sync::waitqueue::AtomicWaker;

/// Shared USB configuration state with one wake slot per independent waiter.
pub(crate) struct ConnectionState {
    configured: AtomicBool,
    sender_waker: AtomicWaker,
    receiver_waker: AtomicWaker,
}

impl ConnectionState {
    pub(crate) const fn new() -> Self {
        Self {
            configured: AtomicBool::new(false),
            sender_waker: AtomicWaker::new(),
            receiver_waker: AtomicWaker::new(),
        }
    }

    pub(crate) fn is_configured(&self) -> bool {
        self.configured.load(Ordering::SeqCst)
    }

    /// Publish either configuration or disconnect/reset state and notify both
    /// application directions. Neither waiter can overwrite the other's wake
    /// registration.
    pub(crate) fn publish(&self, configured: bool) {
        self.configured.store(configured, Ordering::SeqCst);
        self.sender_waker.wake();
        self.receiver_waker.wake();
    }

    pub(crate) async fn wait_sender(&self) {
        Self::wait(&self.configured, &self.sender_waker).await;
    }

    pub(crate) async fn wait_receiver(&self) {
        Self::wait(&self.configured, &self.receiver_waker).await;
    }

    async fn wait(configured: &AtomicBool, waker: &AtomicWaker) {
        poll_fn(|cx| {
            waker.register(cx.waker());
            if configured.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::future::Future;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll, Waker};
    use std::sync::Arc;
    use std::task::Wake;

    use super::ConnectionState;

    static SENDER_WAKE_COUNT: AtomicUsize = AtomicUsize::new(0);
    static RECEIVER_WAKE_COUNT: AtomicUsize = AtomicUsize::new(0);

    struct CountingWake(&'static AtomicUsize);

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting_waker(counter: &'static AtomicUsize) -> Waker {
        Waker::from(Arc::new(CountingWake(counter)))
    }

    #[test]
    fn sender_and_receiver_waiters_are_woken_together() {
        let state = ConnectionState::new();
        assert!(!state.is_configured());
        SENDER_WAKE_COUNT.store(0, Ordering::SeqCst);
        RECEIVER_WAKE_COUNT.store(0, Ordering::SeqCst);

        let mut sender_wait = core::pin::pin!(state.wait_sender());
        let mut receiver_wait = core::pin::pin!(state.wait_receiver());
        let sender_waker = counting_waker(&SENDER_WAKE_COUNT);
        let receiver_waker = counting_waker(&RECEIVER_WAKE_COUNT);
        let mut sender_context = Context::from_waker(&sender_waker);
        let mut receiver_context = Context::from_waker(&receiver_waker);

        assert!(matches!(sender_wait.as_mut().poll(&mut sender_context), Poll::Pending));
        assert!(matches!(
            receiver_wait.as_mut().poll(&mut receiver_context),
            Poll::Pending
        ));

        // USB reset/disconnect publishes the unconfigured state too. Both
        // waiters must be notified and then remain armed for reconfiguration.
        state.publish(false);
        assert_eq!(SENDER_WAKE_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(RECEIVER_WAKE_COUNT.load(Ordering::SeqCst), 1);
        assert!(matches!(sender_wait.as_mut().poll(&mut sender_context), Poll::Pending));
        assert!(matches!(
            receiver_wait.as_mut().poll(&mut receiver_context),
            Poll::Pending
        ));

        state.publish(true);
        assert!(state.is_configured());
        assert_eq!(SENDER_WAKE_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(RECEIVER_WAKE_COUNT.load(Ordering::SeqCst), 2);
        assert!(matches!(
            sender_wait.as_mut().poll(&mut sender_context),
            Poll::Ready(())
        ));
        assert!(matches!(
            receiver_wait.as_mut().poll(&mut receiver_context),
            Poll::Ready(())
        ));
    }
}
