//! Reference-firmware PD incident recorder and USB retrieval ABI.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Instant;
use pd_sink::black_box::{
    application_event_kind, decode_page, encode_page, flags, newest, Log, Record, BLACK_BOX_ABI_VERSION, RECORD_LEN,
};

#[cfg(feature = "deep-black-box")]
use pd_sink::numeric_trace::{
    set_numeric_trace_callback, NumericTraceEvent, NumericTraceEventKind, NumericTraceHardResetPhase,
    NUMERIC_TRACE_ABI_VERSION,
};

use crate::black_box_flash;

pub const REQUEST_MAGIC: [u8; 2] = *b"BB";
const RESPONSE_MAGIC: [u8; 4] = *b"PDBB";
const RESPONSE_SUMMARY: u8 = 0xb0;
const RESPONSE_RECORD: u8 = 0xb1;
pub const RESPONSE_MAX_LEN: usize = 6 + 16;
const NO_PREPARED_PAGE: u8 = u8::MAX;

#[derive(Clone, Copy)]
struct State {
    log: Log,
    active_page: u8,
    valid: bool,
    restored: bool,
    dirty: bool,
    write_error: bool,
    prepared_page: u8,
}

impl State {
    const fn empty() -> Self {
        Self {
            log: Log::new(0, 0),
            active_page: 1,
            valid: false,
            restored: false,
            dirty: false,
            write_error: false,
            prepared_page: NO_PREPARED_PAGE,
        }
    }

    fn status_flags(self) -> u8 {
        u8::from(self.valid)
            | (u8::from(self.restored) << 1)
            | (u8::from(self.dirty) << 2)
            | (u8::from(self.write_error) << 3)
    }
}

static STATE: Mutex<CriticalSectionRawMutex, Cell<State>> = Mutex::new(Cell::new(State::empty()));
static POWER_FAIL_LATCHED: Mutex<CriticalSectionRawMutex, Cell<bool>> = Mutex::new(Cell::new(false));

#[cfg(feature = "deep-black-box")]
static LIVE: Mutex<CriticalSectionRawMutex, Cell<Log>> =
    Mutex::new(Cell::new(Log::new(NUMERIC_TRACE_ABI_VERSION, flags::DEEP_TRACE)));

fn now_ms() -> u32 {
    Instant::now().as_millis().min(u64::from(u32::MAX)) as u32
}

pub fn initialize() {
    let page_a = black_box_flash::read_page(0);
    let page_b = black_box_flash::read_page(1);
    let state = if let Some((active_page, log)) = newest(decode_page(&page_a), decode_page(&page_b)) {
        State {
            log,
            active_page,
            valid: true,
            restored: true,
            dirty: false,
            write_error: false,
            prepared_page: NO_PREPARED_PAGE,
        }
    } else {
        State::empty()
    };
    STATE.lock(|cell| cell.set(state));

    #[cfg(feature = "deep-black-box")]
    LIVE.lock(|cell| {
        cell.set(if state.valid && state.log.flags() & flags::FROZEN != 0 {
            // Preserve the initiating incident across secondary recovery boots.
            // Log::push() rejects later traffic while this flag remains set.
            state.log
        } else {
            Log::new(NUMERIC_TRACE_ABI_VERSION, flags::DEEP_TRACE)
        });
    });
}

/// Erase the inactive A/B page before PD, USB, and PVD are enabled.
pub fn prepare_inactive_page() {
    let state = STATE.lock(Cell::get);
    let erased_a = black_box_flash::page_is_erased(0);
    let erased_b = black_box_flash::page_is_erased(1);
    let target = if state.valid {
        state.active_page ^ 1
    } else if erased_a {
        0
    } else if erased_b {
        1
    } else {
        0
    };
    let already_erased = if target == 0 { erased_a } else { erased_b };
    let ready = already_erased || black_box_flash::erase_page(target);
    let verified = ready && black_box_flash::page_is_erased(target);

    STATE.lock(|cell| {
        let mut latest = cell.get();
        latest.prepared_page = if verified { target } else { NO_PREPARED_PAGE };
        latest.write_error = !verified;
        cell.set(latest);
    });
}

pub fn enable_power_fail_capture() {
    // Keep the early load cutoff even if the inactive journal page could not
    // be prepared. The ISR independently skips persistence in that case.
    black_box_flash::configure_power_fail_detector();
}

/// Latch the application load off until the MCU reboots.
pub fn latch_power_fail() {
    POWER_FAIL_LATCHED.lock(|cell| cell.set(true));
}

pub fn power_fail_latched() -> bool {
    POWER_FAIL_LATCHED.lock(Cell::get)
}

#[cfg(not(feature = "deep-black-box"))]
pub fn record_application(kind: u8, code: u8, context: u32) {
    STATE.lock(|cell| {
        let mut state = cell.get();
        state.log.push(Record::application(now_ms(), kind, code, context));
        state.log.set_generation(state.log.generation().wrapping_add(1));
        state.dirty = true;
        cell.set(state);
    });
}

#[cfg(feature = "deep-black-box")]
pub fn record_application(kind: u8, code: u8, context: u32) {
    LIVE.lock(|cell| {
        let mut live = cell.get();
        live.push(Record::application(now_ms(), kind, code, context));
        cell.set(live);
    });
    snapshot_live(false);
}

pub fn record_hard_reset(sent: bool, cause: u8, recovery_ms: u64, vbus_present: bool) {
    let code = cause | if sent { 0x80 } else { 0 };
    let record = Record::new(
        now_ms(),
        0x80 | application_event_kind::HARD_RESET,
        code,
        u8::from(vbus_present),
        u8::MAX,
        u16::MAX,
        recovery_ms.min(u64::from(u16::MAX)) as u16,
    );

    #[cfg(not(feature = "deep-black-box"))]
    STATE.lock(|cell| {
        let mut state = cell.get();
        state.log.push(record);
        state.log.insert_flags(flags::HARD_RESET_TRIGGERED);
        state.log.set_generation(state.log.generation().wrapping_add(1));
        state.dirty = true;
        cell.set(state);
    });

    #[cfg(feature = "deep-black-box")]
    {
        LIVE.lock(|cell| {
            let mut live = cell.get();
            live.push(record);
            live.insert_flags(flags::HARD_RESET_TRIGGERED | flags::FROZEN);
            cell.set(live);
        });
        snapshot_live(true);
        persist_frozen_snapshot();
    }
}

/// Commit the first frozen incident while the already-prepared journal page is
/// still available. A PD Hard Reset does not normally reset the MCU, but some
/// failing source/board combinations remove power or reset the application
/// during recovery. Keeping this write synchronous makes the initiating trace
/// survive that secondary reset.
#[cfg(feature = "deep-black-box")]
fn persist_frozen_snapshot() {
    critical_section::with(|_| {
        let state = STATE.lock(Cell::get);
        if state.prepared_page > 1 || !state.dirty {
            return;
        }

        let page = encode_page(state.log);
        let written = black_box_flash::program_page(state.prepared_page, &page);

        STATE.lock(|cell| {
            let mut latest = cell.get();
            latest.prepared_page = NO_PREPARED_PAGE;
            latest.write_error = !written;
            if written {
                latest.active_page = state.prepared_page;
                latest.valid = true;
                latest.log = state.log;
                latest.dirty = false;
            }
            cell.set(latest);
        });
    });
}

#[cfg(feature = "deep-black-box")]
fn snapshot_live(freeze: bool) {
    let mut live = LIVE.lock(Cell::get);
    if freeze {
        live.insert_flags(flags::HARD_RESET_TRIGGERED | flags::FROZEN);
        LIVE.lock(|cell| cell.set(live));
    }
    STATE.lock(|cell| {
        let mut state = cell.get();
        live.set_generation(state.log.generation().wrapping_add(1));
        state.log = live;
        state.dirty = true;
        cell.set(state);
    });
}

#[cfg(feature = "deep-black-box")]
fn capture_numeric_trace(event: NumericTraceEvent) {
    let hard_reset_snapshot = event.kind == NumericTraceEventKind::HardReset
        && matches!(
            event.code,
            code if code == NumericTraceHardResetPhase::Received as u8
                || code == NumericTraceHardResetPhase::TransmitStart as u8
                || code == NumericTraceHardResetPhase::TransmitComplete as u8
                || code == NumericTraceHardResetPhase::TransmitFailure as u8
        );
    LIVE.lock(|cell| {
        let mut live = cell.get();
        live.push(Record::new(
            now_ms(),
            event.kind as u8,
            event.code,
            event.message_id,
            event.counter,
            event.header,
            event.detail,
        ));
        if hard_reset_snapshot {
            live.insert_flags(flags::HARD_RESET_TRIGGERED);
        }
        cell.set(live);
    });

    if hard_reset_snapshot {
        snapshot_live(false);
    }
}

#[cfg(feature = "deep-black-box")]
pub fn enable_numeric_trace() {
    set_numeric_trace_callback(Some(capture_numeric_trace));
}

/// Called only from the PVD interrupt after PB10 has already been cleared.
///
/// Appending this event snapshots the current deep numeric ring even when the
/// falling rail outruns the task-level VBUS-detector future. `sample_context`
/// contains raw GPIOB inputs in bits 0..15 and raw EXTI pending bits in
/// bits 16..31.
pub fn persist_on_power_fail(sample_flags: u8, sample_context: u32) {
    record_application(application_event_kind::POWER_FAIL_SAMPLE, sample_flags, sample_context);

    let state = STATE.lock(Cell::get);
    if state.prepared_page > 1 || !state.dirty {
        return;
    }
    let page = encode_page(state.log);
    let written = black_box_flash::program_page(state.prepared_page, &page);

    STATE.lock(|cell| {
        let mut latest = cell.get();
        latest.prepared_page = NO_PREPARED_PAGE;
        latest.write_error = !written;
        if written {
            latest.active_page = state.prepared_page;
            latest.valid = true;
            latest.log = state.log;
            latest.dirty = false;
        }
        cell.set(latest);
    });
}

#[derive(Clone, Copy)]
pub struct Response {
    bytes: [u8; RESPONSE_MAX_LEN],
    len: u8,
}

impl Response {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Encode one small raw `BB<page>` query response alongside compact protocol v1.
pub fn response(page: u8) -> Response {
    let state = STATE.lock(Cell::get);
    let mut bytes = [0; RESPONSE_MAX_LEN];
    bytes[..4].copy_from_slice(&RESPONSE_MAGIC);

    let payload_len = if page == 0 {
        bytes[4] = RESPONSE_SUMMARY;
        bytes[6] = BLACK_BOX_ABI_VERSION;
        bytes[7] = state.status_flags();
        bytes[8] = state.log.len();
        bytes[9] = state.log.flags();
        bytes[10..14].copy_from_slice(&state.log.generation().to_le_bytes());
        bytes[14..16].copy_from_slice(&state.log.next_sequence().to_le_bytes());
        bytes[16] = state.log.trace_abi();
        bytes[17] = state.active_page;
        bytes[18] = state.prepared_page;
        bytes[19] = if cfg!(feature = "deep-black-box") { 2 } else { 1 };
        14
    } else {
        bytes[4] = RESPONSE_RECORD;
        let index = usize::from(page - 1);
        bytes[6] = BLACK_BOX_ABI_VERSION;
        bytes[7] = index as u8;
        bytes[8] = u8::from(index < usize::from(state.log.len()));
        bytes[9] = RECORD_LEN as u8;
        if let Some(record) = state.log.record(index) {
            record.encode((&mut bytes[10..10 + RECORD_LEN]).try_into().unwrap());
        }
        4 + RECORD_LEN
    };
    bytes[5] = payload_len as u8;
    Response { bytes, len: (6 + payload_len) as u8 }
}
