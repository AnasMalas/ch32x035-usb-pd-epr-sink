// Architecture-independent receive bookkeeping shared by the USBPD
// interrupt handler and the PD task.
//
// The interrupt handler owns reception: it copies every accepted SOP frame
// into [`RxRing`], answers it with GoodCRC, and re-arms the receiver without
// waiting for the task. The task only consumes frames. This keeps GoodCRC
// and receiver turnaround inside the USB PD timing limits regardless of how
// long other executor tasks run.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering};

/// Header plus the largest data payload the CH32 PHY can carry, without CRC.
pub(crate) const MESSAGE_BYTES: usize = 30;
const HEADER_BYTES: usize = 2;
const CRC_BYTES: usize = 4;

/// Return the message length, without CRC, of a completed SOP frame that the
/// protocol layer could accept, or `None` for a frame that must not be
/// acknowledged.
///
/// `byte_count` is the PHY's DMA byte count, including the four CRC bytes.
/// The length rule mirrors the protocol layer's frame validation, which ran
/// before software GoodCRC. A truncated or corrupted frame therefore stays
/// unacknowledged and the partner retries it.
pub(crate) fn message_length(frame: &[u8], byte_count: usize) -> Option<usize> {
    if !(HEADER_BYTES + CRC_BYTES..=MESSAGE_BYTES + CRC_BYTES).contains(&byte_count) || frame.len() < HEADER_BYTES {
        return None;
    }
    let length = byte_count - CRC_BYTES;
    let header = u16::from_le_bytes([frame[0], frame[1]]);
    let objects = usize::from((header >> 12) & 0x7);
    let extended = header & 0x8000 != 0;
    if extended && objects == 0 {
        return None;
    }
    (length == HEADER_BYTES + objects * 4).then_some(length)
}

/// Return whether `header` is a GoodCRC message, which is never acknowledged.
pub(crate) fn is_good_crc(header: u16) -> bool {
    header & 0xf01f == 0x0001
}

/// Build the GoodCRC header that acknowledges `received` from a Sink/UFP.
///
/// The MessageID is echoed. The Specification Revision is the partner's,
/// capped at Revision 3.x; the protocol layer negotiates the same value
/// (the lower of the local and partner revisions) from the first message.
pub(crate) fn good_crc_header(received: u16) -> u16 {
    let revision = ((received >> 6) & 0x3).min(0b10);
    0x0001 | (revision << 6) | (received & 0x0e00)
}

/// Single-producer, single-consumer queue of received messages.
///
/// Only the interrupt handler calls [`Self::push`]; only the PD task calls
/// [`Self::pop`] and [`Self::clear`]. Each index has exactly one writer, so
/// plain atomic loads and stores are sufficient on cores without
/// read-modify-write atomics.
pub(crate) struct RxRing<const N: usize> {
    head: AtomicU8,
    tail: AtomicU8,
    slots: UnsafeCell<[[u8; MESSAGE_BYTES + 1]; N]>,
}

// SAFETY: slots between `head` and `tail` belong to the consumer, and the one
// slot at `tail` belongs to the producer until `tail` is published.
unsafe impl<const N: usize> Sync for RxRing<N> {}

impl<const N: usize> RxRing<N> {
    pub(crate) const fn new() -> Self {
        assert!(N.is_power_of_two() && N <= 128);
        Self {
            head: AtomicU8::new(0),
            tail: AtomicU8::new(0),
            slots: UnsafeCell::new([[0; MESSAGE_BYTES + 1]; N]),
        }
    }

    /// Producer only. Queue a copy of `message`; return `false` when full.
    pub(crate) fn push(&self, message: &[u8]) -> bool {
        debug_assert!(message.len() <= MESSAGE_BYTES);
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if usize::from(tail.wrapping_sub(head)) >= N {
            return false;
        }
        // SAFETY: the slot at `tail` is free and unpublished; only the
        // producer writes it.
        let slot = unsafe { &mut (*self.slots.get())[usize::from(tail) & (N - 1)] };
        slot[0] = message.len() as u8;
        slot[1..=message.len()].copy_from_slice(message);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer only. Remove the oldest message and copy it into `out`.
    ///
    /// Returns `None` when empty, `Some(Ok(length))` for a copied message, or
    /// `Some(Err(required))` when `out` is too small; that message is dropped.
    pub(crate) fn pop(&self, out: &mut [u8]) -> Option<Result<usize, usize>> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: the slot at `head` was published by the producer and is not
        // reused until `head` advances below.
        let slot = unsafe { &(*self.slots.get())[usize::from(head) & (N - 1)] };
        let length = usize::from(slot[0]);
        let result = match out.get_mut(..length) {
            Some(destination) => {
                destination.copy_from_slice(&slot[1..=length]);
                Ok(length)
            }
            None => Err(length),
        };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(result)
    }

    /// Consumer only. Return whether no message is queued.
    pub(crate) fn is_empty(&self) -> bool {
        self.head.load(Ordering::Relaxed) == self.tail.load(Ordering::Acquire)
    }

    /// Consumer only. Discard every queued message.
    pub(crate) fn clear(&self) {
        self.head.store(self.tail.load(Ordering::Acquire), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{good_crc_header, is_good_crc, message_length, RxRing, MESSAGE_BYTES};

    fn header(message_type: u16, objects: u16, message_id: u16, revision: u16) -> u16 {
        message_type | (revision << 6) | (message_id << 9) | (objects << 12) | 0x0100
    }

    fn frame(header: u16, objects: usize) -> ([u8; 34], usize) {
        let mut bytes = [0u8; 34];
        bytes[..2].copy_from_slice(&header.to_le_bytes());
        (bytes, 2 + objects * 4 + 4)
    }

    #[test]
    fn only_complete_consistent_frames_are_acknowledged() {
        let (control, count) = frame(header(3, 0, 2, 2), 0);
        assert_eq!(message_length(&control, count), Some(2));

        let (caps, count) = frame(header(1, 7, 5, 2), 7);
        assert_eq!(message_length(&caps, count), Some(30));

        // Byte count disagrees with the header's object count.
        assert_eq!(message_length(&caps, count - 4), None);
        // Too short to hold a header and CRC, or longer than the PHY buffer.
        assert_eq!(message_length(&control, 5), None);
        assert_eq!(message_length(&control, 35), None);
        // An extended message always carries at least one object.
        let (extended, count) = frame(header(0x0f, 0, 1, 2) | 0x8000, 0);
        assert_eq!(message_length(&extended, count), None);
        let (extended, count) = frame(header(0x0f, 1, 1, 2) | 0x8000, 1);
        assert_eq!(message_length(&extended, count), Some(6));
    }

    #[test]
    fn good_crc_echoes_message_id_and_caps_revision() {
        let accept = header(3, 0, 5, 2);
        let reply = good_crc_header(accept);
        assert!(is_good_crc(reply));
        assert_eq!((reply >> 9) & 0x7, 5);
        assert_eq!((reply >> 6) & 0x3, 2);
        // Sink power role and UFP data role.
        assert_eq!(reply & 0x0120, 0);

        assert_eq!((good_crc_header(header(3, 0, 1, 1)) >> 6) & 0x3, 1);
        assert_eq!((good_crc_header(header(3, 0, 1, 3)) >> 6) & 0x3, 2);
    }

    #[test]
    fn good_crc_is_recognised_only_without_data_objects() {
        assert!(is_good_crc(0x0001));
        assert!(is_good_crc(header(1, 0, 7, 2)));
        assert!(!is_good_crc(header(1, 1, 7, 2)));
        assert!(!is_good_crc(header(3, 0, 7, 2)));
        assert!(!is_good_crc(0x8001 | (1 << 12)));
    }

    #[test]
    fn ring_preserves_order_and_reports_full() {
        let ring = RxRing::<4>::new();
        let mut out = [0u8; MESSAGE_BYTES];
        assert!(ring.is_empty());
        for id in 0..4u8 {
            assert!(ring.push(&[id, 0x10]));
        }
        assert!(
            !ring.push(&[9, 9]),
            "a full ring rejects the frame so it is not acknowledged"
        );
        for id in 0..4u8 {
            assert_eq!(ring.pop(&mut out), Some(Ok(2)));
            assert_eq!(out[..2], [id, 0x10]);
        }
        assert_eq!(ring.pop(&mut out), None);
    }

    #[test]
    fn ring_indices_wrap_and_clear_discards_everything() {
        let ring = RxRing::<4>::new();
        let mut out = [0u8; MESSAGE_BYTES];
        for round in 0..300u16 {
            let byte = round as u8;
            assert!(ring.push(&[byte; 6]));
            assert_eq!(ring.pop(&mut out), Some(Ok(6)));
            assert_eq!(out[..6], [byte; 6]);
        }
        assert!(ring.push(&[1, 2]));
        assert!(ring.push(&[3, 4]));
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.pop(&mut out), None);
    }

    #[test]
    fn too_small_destination_consumes_the_frame() {
        let ring = RxRing::<2>::new();
        assert!(ring.push(&[1; 10]));
        let mut small = [0u8; 4];
        assert_eq!(ring.pop(&mut small), Some(Err(10)));
        assert!(ring.is_empty());
    }
}
