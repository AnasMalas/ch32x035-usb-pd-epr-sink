# Examples

These applications demonstrate the reusable sink crate without expanding the
crate's ownership:

- [`reference-firmware/`](reference-firmware/) binds the sink to CH32X035F8U6
  pins, USB CDC, LinkE diagnostics, safety supervision, and build profiles.
- [`pd-control/`](pd-control/) is a browser client for firmware that exposes
  the compact control protocol over CDC.
- [`artifacts/`](artifacts/) is ignored local output staging for both examples.

The core library defines typed commands, events, and transport-neutral compact
frames. It does not enumerate USB devices, open a serial port, launch a
browser, format a terminal, or select application pins.
