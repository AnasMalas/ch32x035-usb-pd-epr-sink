#![cfg(feature = "numeric-trace")]

use core::sync::atomic::{AtomicU8, Ordering};

use pd_sink::numeric_trace::{
    set_numeric_trace_callback, NumericTraceCallback, NumericTraceEvent, NumericTraceEventKind,
    NUMERIC_TRACE_ABI_VERSION,
};

static LAST_KIND: AtomicU8 = AtomicU8::new(0);

fn capture(event: NumericTraceEvent) {
    LAST_KIND.store(event.kind as u8, Ordering::Relaxed);
}

#[test]
fn public_numeric_trace_abi_is_fixed_and_registration_is_reversible() {
    assert_eq!(NUMERIC_TRACE_ABI_VERSION, 1);
    assert_eq!(core::mem::size_of::<NumericTraceEvent>(), 8);

    let callback: NumericTraceCallback = capture;
    let previous = set_numeric_trace_callback(Some(callback));
    assert!(previous.is_none());
    let installed = set_numeric_trace_callback(None);
    assert!(installed.is_some());
    assert_eq!(LAST_KIND.load(Ordering::Relaxed), 0);
    assert_eq!(NumericTraceEventKind::EprKeepAlive as u8, 13);
}
