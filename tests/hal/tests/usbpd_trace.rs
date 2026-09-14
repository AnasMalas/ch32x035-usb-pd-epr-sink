#![allow(dead_code)]

// The complete HAL is tied to the QingKe target and cannot be linked into a
// native test executable. Compile the exact architecture-independent trace
// callback/record module so its ABI and delivery ordering remain in the
// maintained host suite.
#[rustfmt::skip]
#[path = "../../../vendor/ch32-hal/src/usbpd/trace.rs"]
mod usbpd_trace;
