// The complete HAL is tied to the QingKe target and cannot be linked into a
// native test executable. Compile the exact architecture-independent transfer
// state used by the USBPD ISR so ordering, retry, and buffer-lifetime behavior
// remain in the maintained host suite.
mod turnaround {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/ch32-hal/src/usbpd/turnaround.rs"));
}
