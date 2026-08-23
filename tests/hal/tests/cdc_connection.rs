// The complete HAL is tied to the QingKe target and cannot be linked into a
// native test executable. Compile the exact architecture-independent state
// primitive used by the CDC driver so its multi-waiter contract remains part
// of the maintained host test suite.
mod connection {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/ch32-hal/src/usb_x0fs/connection.rs"));
}
