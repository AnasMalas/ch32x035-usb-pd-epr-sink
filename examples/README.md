# Examples

These applications demonstrate the reusable sink crate without expanding the
crate's ownership:

- [`ch32x035-usb-pd-sink-firmware/`](ch32x035-usb-pd-sink-firmware/) is the
  embedded device example. It binds the sink to CH32X035F8U6 pins, USB CDC,
  LinkE diagnostics, safety supervision, and build profiles.
- [`browser-usb-pd-control-client/`](browser-usb-pd-control-client/) is the
  desktop/Android host example. It connects to compatible firmware over CDC
  and presents the compact control protocol in a browser.
- [`generated-artifacts/`](generated-artifacts/) is ignored local output
  staging for both examples.

The core library defines typed commands, events, and transport-neutral compact
frames. It does not enumerate USB devices, open a serial port, launch a
browser, format a terminal, or select application pins.
