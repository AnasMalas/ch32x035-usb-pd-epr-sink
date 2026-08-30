use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// Singleton transfer state shared by the USBPD task and interrupt handler.
pub(crate) struct TransferState<B: Send> {
    turnaround_requested: AtomicBool,
    rx_prearmed: AtomicBool,
    buffer: UnsafeCell<B>,
}

// SAFETY: the containing USBPD peripheral state is a singleton. Transfer
// methods and the ISR serialize access to the buffer according to the two
// flags; callers must uphold that contract when dereferencing `buffer_ptr`.
unsafe impl<B: Send> Sync for TransferState<B> {}

impl<B: Send> TransferState<B> {
    pub(crate) const fn new(buffer: B) -> Self {
        Self {
            turnaround_requested: AtomicBool::new(false),
            rx_prearmed: AtomicBool::new(false),
            buffer: UnsafeCell::new(buffer),
        }
    }

    /// Start an ordinary transmission and replace any stale turnaround state.
    pub(crate) fn begin_transmit(&self, request_turnaround: bool) {
        self.rx_prearmed.store(false, Ordering::Release);
        self.turnaround_requested.store(request_turnaround, Ordering::Release);
    }

    /// Complete TX-end handling, configuring RX before publishing it as armed.
    #[inline(always)]
    pub(crate) fn complete_transmit(&self, transfer_ok: bool, arm_receive: impl FnOnce()) {
        let requested = self.turnaround_requested.load(Ordering::Acquire);
        self.turnaround_requested.store(false, Ordering::Release);
        if requested && transfer_ok {
            arm_receive();
            self.rx_prearmed.store(true, Ordering::Release);
        }
    }

    /// Consume a receive configuration prepared by TX-end handling.
    pub(crate) fn take_prearmed_receive(&self) -> bool {
        // CH32X035 supports atomic byte loads/stores but not an atomic swap.
        // This has one consumer and is called only after TX completion wakes
        // the task, so a load followed by a store is sufficient.
        let prearmed = self.rx_prearmed.load(Ordering::Acquire);
        self.rx_prearmed.store(false, Ordering::Release);
        prearmed
    }

    /// Cancel pending or published turnaround state during reset/error paths.
    pub(crate) fn cancel(&self) {
        self.turnaround_requested.store(false, Ordering::Release);
        self.rx_prearmed.store(false, Ordering::Release);
    }

    /// Return the stable singleton buffer address.
    ///
    /// Dereferencing this pointer is unsafe. The caller must serialize task and
    /// ISR access using the peripheral transfer lifecycle.
    pub(crate) fn buffer_ptr(&self) -> *mut B {
        self.buffer.get()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::TransferState;

    #[test]
    fn receive_is_published_only_after_tx_end_arms_hardware() {
        let state = TransferState::new([0u8; 34]);
        state.begin_transmit(true);

        let mut hardware_armed = false;
        state.complete_transmit(true, || {
            assert!(!state.rx_prearmed.load(core::sync::atomic::Ordering::Acquire));
            hardware_armed = true;
        });

        assert!(hardware_armed);
        assert!(state.take_prearmed_receive());
        assert!(!state.take_prearmed_receive());
    }

    #[test]
    fn errors_cancellation_and_goodcrc_leave_no_prearmed_receive() {
        let state = TransferState::new([0u8; 34]);

        state.begin_transmit(true);
        state.complete_transmit(false, || panic!("buffer-error TX must not arm RX"));
        assert!(!state.take_prearmed_receive());

        state.begin_transmit(true);
        state.cancel();
        state.complete_transmit(true, || panic!("cancelled TX must not arm RX"));
        assert!(!state.take_prearmed_receive());

        state.begin_transmit(false);
        state.complete_transmit(true, || panic!("GoodCRC TX must not arm RX"));
        assert!(!state.take_prearmed_receive());
    }

    #[test]
    fn every_ordinary_retry_replaces_and_rearms_turnaround() {
        let state = TransferState::new([0u8; 34]);

        for _ in 0..3 {
            state.begin_transmit(true);
            state.complete_transmit(true, || {});
            assert!(state.take_prearmed_receive());
        }
    }

    #[test]
    fn moving_a_phy_handle_cannot_move_the_singleton_dma_buffer() {
        #[repr(align(4))]
        struct AlignedBuffer([u8; 34]);

        static STATE: TransferState<AlignedBuffer> = TransferState::new(AlignedBuffer([0; 34]));

        struct MockPhy(&'static TransferState<AlignedBuffer>);

        let phy = MockPhy(&STATE);
        let before = phy.0.buffer_ptr();
        let moved_phy = (phy,).0;
        let after = moved_phy.0.buffer_ptr();

        assert_eq!(before, after);
        assert_eq!((before as usize) & 0x3, 0);
        // Keep the field live so this remains a faithful aligned-buffer type.
        assert_eq!(unsafe { &*before }.0.len(), 34);
    }
}
