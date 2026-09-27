// The complete HAL is tied to the QingKe target and cannot be linked into a
// native test executable. Compile the exact architecture-independent receive
// queue and GoodCRC rules used by the USBPD interrupt handler so frame
// acceptance, acknowledgement, and queue ordering stay in the host suite.
mod rx_queue {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/ch32-hal/src/usbpd/rx_queue.rs"));
}
