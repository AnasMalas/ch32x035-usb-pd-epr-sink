# Documentation

Choose the document by what you are trying to do:

## Debug real hardware

- [Debugging real USB-PD hardware](debugging.md) gives a symptom-driven
  isolation workflow, nonintrusive trace design, numeric event decoder, and
  issue-report checklist for timing and power failures.

## Use or integrate

- [Integrating the sink library](integration.md) explains the application
  adapters, configuration, startup sequence, and load-safety boundary.
- [Compact control protocol](control_protocol.md) describes the
  transport-neutral host framing used by the reference browser client.
- [Source interoperability observations](charger_interoperability.md) records
  measured charger behavior and the conservative policies derived from it.

## Understand or maintain

- [Firmware architecture](architecture.md) defines the reusable, maintained
  dependency, firmware-example, and browser-client boundaries.
- [Contributing](../CONTRIBUTING.md) covers repository checks, change
  placement, evidence, dependency updates, and licensing.

## Build and validate the examples

- [CH32X035 sink firmware](../examples/ch32x035-usb-pd-sink-firmware/README.md)
  is the exact build, flash, and runtime guide.
- [Hardware/firmware interface](../examples/ch32x035-usb-pd-sink-firmware/docs/hardware_interface.md)
  defines the pin and fail-safe power-path contract.
- [First-board verification](../examples/ch32x035-usb-pd-sink-firmware/docs/first_board_verification.md)
  starts at safe 5 V and records the qualification evidence required before
  higher-voltage testing.
- [Browser control client](../examples/browser-usb-pd-control-client/README.md)
  covers local desktop Web Serial and hosted Android WebUSB use.
- [No-load CC wake probe](../examples/ch32x035-usbpd-cc-wake-probe/README.md)
  isolates the experimental `IE_PD_IO` wake source from load control and
  production detach policy so its hardware behavior can be characterized.

The root [README](../README.md) is the shortest starting point for a new user.
Implementation history that still affects behavior belongs in the relevant
architecture, interoperability, or `vendor/*/UPSTREAM.md` document rather than
in a conversational handoff.
