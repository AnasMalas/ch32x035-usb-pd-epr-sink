//! Storage-independent records for a small persistent PD incident recorder.
//!
//! This module defines only the fixed record/page ABI and ring behavior. It
//! does not access flash, choose a power-fail detector, own a USB transport,
//! or decide which application events are exceptional. Those are board and
//! product responsibilities.

/// Size of one CH32X035 user-flash page.
pub const PAGE_SIZE: usize = 256;
/// Number of records retained by one page.
pub const RECORD_CAPACITY: usize = 16;
/// Encoded size of one record.
pub const RECORD_LEN: usize = 12;
/// Persistent black-box record/page ABI version.
pub const BLACK_BOX_ABI_VERSION: u8 = 1;

/// Persistent log flags.
pub mod flags {
    /// A Hard Reset caused the recorder to snapshot/freeze.
    pub const HARD_RESET_TRIGGERED: u8 = 1 << 0;
    /// The current incident is frozen against later recovery traffic.
    pub const FROZEN: u8 = 1 << 1;
    /// At least one older record was overwritten by the rolling ring.
    pub const OVERWROTE_OLD_RECORDS: u8 = 1 << 2;
    /// The profile records the full numeric protocol trace.
    pub const DEEP_TRACE: u8 = 1 << 3;
}

/// High-level application event IDs used by the reference firmware.
///
/// Application-owned records set bit 7 in [`Record::kind`]. Numeric protocol
/// records use the unmodified `NumericTraceEventKind` value instead.
pub mod application_event_kind {
    pub const HARD_RESET: u8 = 1;
    pub const PHY_RESET_FAILED: u8 = 2;
    pub const PHY_UNSTABLE: u8 = 3;
    pub const PARTNER_TIMEOUT: u8 = 4;
    pub const PROTOCOL_RECOVERY: u8 = 5;
    pub const TERMINAL: u8 = 6;
    pub const EPR_ENTRY_FAILED: u8 = 7;
    /// A board-qualified VBUS-present predicate deasserted.
    pub const VBUS_DETECTOR_LOW: u8 = 8;
}

const MAGIC: [u8; 4] = *b"PDBB";
const FORMAT_VERSION: u8 = 1;
const HEADER_LEN: usize = 16;
const CRC_OFFSET: usize = PAGE_SIZE - 4;

/// One timestamped numeric or application incident record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct Record([u8; RECORD_LEN]);

impl Record {
    pub const EMPTY: Self = Self([0; RECORD_LEN]);

    pub fn new(uptime_ms: u32, kind: u8, code: u8, message_id: u8, counter: u8, header: u16, detail: u16) -> Self {
        let mut bytes = [0; RECORD_LEN];
        bytes[0..4].copy_from_slice(&uptime_ms.to_le_bytes());
        bytes[4] = kind;
        bytes[5] = code;
        bytes[6] = message_id;
        bytes[7] = counter;
        bytes[8..10].copy_from_slice(&header.to_le_bytes());
        bytes[10..12].copy_from_slice(&detail.to_le_bytes());
        Self(bytes)
    }

    /// Construct a high-level application record with one 32-bit context.
    pub fn application(uptime_ms: u32, kind: u8, code: u8, context: u32) -> Self {
        Self::new(uptime_ms, 0x80 | (kind & 0x7f), code, u8::MAX, u8::MAX, context as u16, (context >> 16) as u16)
    }

    pub fn uptime_ms(self) -> u32 {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }

    pub fn kind(self) -> u8 {
        self.0[4]
    }

    pub fn code(self) -> u8 {
        self.0[5]
    }

    pub fn message_id(self) -> u8 {
        self.0[6]
    }

    pub fn counter(self) -> u8 {
        self.0[7]
    }

    pub fn header(self) -> u16 {
        u16::from_le_bytes([self.0[8], self.0[9]])
    }

    pub fn detail(self) -> u16 {
        u16::from_le_bytes([self.0[10], self.0[11]])
    }

    pub fn context(self) -> u32 {
        u32::from(self.header()) | (u32::from(self.detail()) << 16)
    }

    pub fn encode(self, output: &mut [u8; RECORD_LEN]) {
        *output = self.0;
    }

    pub fn decode(input: &[u8; RECORD_LEN]) -> Self {
        Self(*input)
    }
}

const _: () = assert!(core::mem::size_of::<Record>() == RECORD_LEN);

/// A fixed rolling log suitable for encoding into one flash page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Log {
    generation: u32,
    next_sequence: u16,
    count: u8,
    flags: u8,
    trace_abi: u8,
    records: [Record; RECORD_CAPACITY],
}

impl Log {
    pub const fn new(trace_abi: u8, initial_flags: u8) -> Self {
        Self {
            generation: 0,
            next_sequence: 0,
            count: 0,
            flags: initial_flags,
            trace_abi,
            records: [Record::EMPTY; RECORD_CAPACITY],
        }
    }

    pub const fn generation(&self) -> u32 {
        self.generation
    }

    pub fn set_generation(&mut self, generation: u32) {
        self.generation = generation;
    }

    pub const fn next_sequence(&self) -> u16 {
        self.next_sequence
    }

    pub const fn len(&self) -> u8 {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub const fn flags(&self) -> u8 {
        self.flags
    }

    pub fn insert_flags(&mut self, flags: u8) {
        self.flags |= flags;
    }

    pub const fn trace_abi(&self) -> u8 {
        self.trace_abi
    }

    pub fn push(&mut self, record: Record) {
        if self.flags & flags::FROZEN != 0 {
            return;
        }
        let slot = usize::from(self.next_sequence) & (RECORD_CAPACITY - 1);
        self.records[slot] = record;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        if usize::from(self.count) < RECORD_CAPACITY {
            self.count += 1;
        } else {
            self.flags |= flags::OVERWROTE_OLD_RECORDS;
        }
    }

    /// Return a record by age, with index zero being the oldest retained.
    pub fn record(&self, index: usize) -> Option<Record> {
        if index >= usize::from(self.count) {
            return None;
        }
        let oldest = self.next_sequence.wrapping_sub(u16::from(self.count));
        Some(self.records[(usize::from(oldest) + index) & (RECORD_CAPACITY - 1)])
    }
}

/// Encode a complete A/B journal page, including CRC32.
pub fn encode_page(log: Log) -> [u8; PAGE_SIZE] {
    let mut page = [0xff; PAGE_SIZE];
    page[0..4].copy_from_slice(&MAGIC);
    page[4] = FORMAT_VERSION;
    page[5] = RECORD_LEN as u8;
    page[6] = log.count.min(RECORD_CAPACITY as u8);
    page[7] = log.flags;
    page[8..12].copy_from_slice(&log.generation.to_le_bytes());
    page[12..14].copy_from_slice(&log.next_sequence.to_le_bytes());
    page[14] = log.trace_abi;
    page[15] = BLACK_BOX_ABI_VERSION;

    for (index, record) in log.records.iter().enumerate() {
        let offset = HEADER_LEN + index * RECORD_LEN;
        record.encode((&mut page[offset..offset + RECORD_LEN]).try_into().unwrap());
    }

    let crc = crc32(&page[..CRC_OFFSET]);
    page[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    page
}

/// Decode one CRC-valid journal page.
pub fn decode_page(page: &[u8; PAGE_SIZE]) -> Option<Log> {
    if page[0..4] != MAGIC
        || page[4] != FORMAT_VERSION
        || page[5] != RECORD_LEN as u8
        || usize::from(page[6]) > RECORD_CAPACITY
        || page[15] != BLACK_BOX_ABI_VERSION
    {
        return None;
    }
    let expected = u32::from_le_bytes(page[CRC_OFFSET..].try_into().ok()?);
    if crc32(&page[..CRC_OFFSET]) != expected {
        return None;
    }

    let mut records = [Record::EMPTY; RECORD_CAPACITY];
    for (index, record) in records.iter_mut().enumerate() {
        let offset = HEADER_LEN + index * RECORD_LEN;
        *record = Record::decode(page[offset..offset + RECORD_LEN].try_into().ok()?);
    }

    Some(Log {
        generation: u32::from_le_bytes(page[8..12].try_into().ok()?),
        next_sequence: u16::from_le_bytes(page[12..14].try_into().ok()?),
        count: page[6],
        flags: page[7],
        trace_abi: page[14],
        records,
    })
}

/// Choose the newest valid page, including generation wraparound.
pub fn newest(a: Option<Log>, b: Option<Log>) -> Option<(u8, Log)> {
    match (a, b) {
        (Some(a), Some(b)) => {
            if b.generation.wrapping_sub(a.generation) < 0x8000_0000 {
                Some((1, b))
            } else {
                Some((0, a))
            }
        }
        (Some(a), None) => Some((0, a)),
        (None, Some(b)) => Some((1, b)),
        (None, None) => None,
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_layout_and_application_context_are_stable() {
        let record = Record::application(1234, application_event_kind::PROTOCOL_RECOVERY, 7, 0x1234_5678);
        assert_eq!(core::mem::size_of::<Record>(), 12);
        assert_eq!(record.uptime_ms(), 1234);
        assert_eq!(record.kind(), 0x80 | application_event_kind::PROTOCOL_RECOVERY);
        assert_eq!(record.code(), 7);
        assert_eq!(record.context(), 0x1234_5678);
    }

    #[test]
    fn page_round_trip_and_corruption_rejection() {
        let mut log = Log::new(1, flags::DEEP_TRACE);
        log.set_generation(42);
        log.push(Record::new(10, 1, 2, 3, 4, 5, 6));
        let page = encode_page(log);
        assert_eq!(decode_page(&page), Some(log));
        assert_eq!(decode_page(&[0xff; PAGE_SIZE]), None);

        let mut corrupt = page;
        corrupt[20] ^= 0x40;
        assert_eq!(decode_page(&corrupt), None);
    }

    #[test]
    fn ring_keeps_latest_sixteen_in_age_order() {
        let mut log = Log::new(0, 0);
        for index in 0..20 {
            log.push(Record::application(index, 1, index as u8, index));
        }
        assert_eq!(log.len(), RECORD_CAPACITY as u8);
        assert_eq!(log.record(0).unwrap().uptime_ms(), 4);
        assert_eq!(log.record(15).unwrap().uptime_ms(), 19);
        assert_ne!(log.flags() & flags::OVERWROTE_OLD_RECORDS, 0);
    }

    #[test]
    fn frozen_log_does_not_admit_recovery_traffic() {
        let mut log = Log::new(1, 0);
        log.push(Record::new(1, 1, 0, 0, 0, 0, 0));
        log.insert_flags(flags::HARD_RESET_TRIGGERED | flags::FROZEN);
        log.push(Record::new(2, 2, 0, 0, 0, 0, 0));
        assert_eq!(log.len(), 1);
        assert_eq!(log.record(0).unwrap().uptime_ms(), 1);
    }

    #[test]
    fn newest_page_handles_generation_wrap() {
        let mut old = Log::new(0, 0);
        old.set_generation(u32::MAX);
        let mut new = old;
        new.set_generation(0);
        assert_eq!(newest(Some(old), Some(new)), Some((1, new)));
    }
}
