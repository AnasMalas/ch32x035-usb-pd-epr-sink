# Integrating the sink library

The library is intended to be bolted onto another CH32X035 firmware without
owning that firmware's entry point, executor, user interface, or board pins.
The application remains in control of `main`.

## Boundary

The library owns:

- PDO parsing and validation;
- fixed, PPS, SPR AVS, and EPR AVS request planning;
- source, cable, board, current, voltage, and power limits;
- contract invalidation and safe request transitions;
- stack request conversion, Source_Info handling, reset handling, EPR entry,
  EPR exit, the bounded automatic-entry budget, Source Alert handling, and
  general/PPS status queries;
- CH32X035 PHY receive/transmit cancellation on VBUS loss when the optional
  `ch32x035` feature is enabled.

The application owns:

- the Embassy executor and interrupt binding;
- VBUS-present sensing and the firmware load-enable output;
- the independent hardware load gate;
- the source of user commands and the destination for diagnostics;
- USB CDC, displays, persistent settings, and every feature unrelated to PD;
- the GPIO choices. The reference PA6/PB12 assignment is only an example.

## Dependency

During private review, consume the repository directly and enable the hardware
adapter:

```toml
[dependencies]
pd-sink = { package = "ch32x035-usb-pd-epr-sink", git = "https://github.com/AnasMalas/ch32x035-usb-pd-epr-sink", features = ["ch32x035"] }
```

The repository currently carries maintained descendants of `usbpd`,
`usbpd-traits`, and `ch32-hal`; no second copies should be added to an
application. A crates.io release is intentionally deferred until those
dependency arrangements and APIs are stable.

## Application adapters

Implement `SinkRuntime` for a small application type. Its core services set the
firmware load request, clear stale commands, wait for a new command, delay
after Hard Reset, and report typed observations. A single
`observe(SinkEvent)` implementation is enough. Flash-constrained firmware can
override the typed `on_*` callbacks directly so unused event formatting is
removed by the linker.

Implement `Ch32x035Port` for another small application type. It reports the
current VBUS-present level, waits for attach/detach notifications, clears a
stale detach notification at session start, disables or enables the firmware
load request, and optionally reports `PhyEvent` diagnostics.

The reference implementation uses Embassy signals populated by an EXTI GPIO
task. Another application can use different pins or a different
3.3 V-safe power-good circuit without modifying the library.

## PPS current-limit indicator

The library reports state and deliberately does not own an LED GPIO. An
application can route both PPS_Status and Alert-triggered general Status to one
indicator:

```rust
fn observe(&mut self, event: SinkEvent) {
    match event {
        SinkEvent::PpsStatus(status) => {
            self.set_cl_led(status.is_current_limited());
        }
        SinkEvent::SourceStatus(status) => {
            if let Some(mode) = status.pps_operating_mode() {
                self.set_cl_led(mode.is_current_limited());
            }
        }
        SinkEvent::Detached
        | SinkEvent::HardReset { .. }
        | SinkEvent::ProtocolLost { .. } => {
            self.set_cl_led(false);
        }
        _ => {}
    }
}
```

A compliant PPS Source sends `Alert` when it changes between CV and CL. The
reusable policy manager follows a non-battery Alert with `Get_Status`, so that
path does not need polling. `RequestPpsStatus` remains useful for live
source-reported voltage/current and as optional periodic compatibility polling
for Sources that omit the Alert.

## Configuration

Construct `SinkConfig` from the board's real limits, not the charger's claimed
maximums. Important fields are:

- `controller.request_context.limits.max_voltage`;
- `board_max_current`, `cable_max_current`, and `max_power`;
- `RequestFlags::epr_capable`;
- the EPR operational PDP, which must agree with the extended sink descriptor;
- `max_auto_epr_attempts` and `hard_reset_recovery_ms`.

`SinkDevice::new` validates values that would be truncated on the wire or
advertised inconsistently. A 5 V-only application sets EPR fields and the
automatic-entry count to zero. A 28 V or 48 V application must explicitly
configure an EPR-capable board limit; the MCU alone does not make its external
power path safe for those voltages.

## Startup

The application creates the CH32 PHY with its chosen CC pins, wraps it in
`Ch32x035UsbPdDriver`, creates `SinkDevice`, and passes both to the maintained
`usbpd::sink::policy_engine::Sink`. The application then runs the policy engine
and applies its own bounded restart policy. See
[`examples/usb-console/src/main.rs`](../examples/usb-console/src/main.rs) for a
complete buildable consumer.

Every physical attachment starts by requesting fixed 5 V and learning source
capabilities. A high voltage is not selected until an explicit request or the
application's own later policy asks for one.

## Load safety

`SinkRuntime::set_load_enabled(true)` is only a firmware request after PS_RDY.
It must not be the sole safety path. The board should enforce:

```text
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

VBUS removal must turn the load path off without relying on the executor, the
PD stack, or a functioning MCU. See [`hardware_interface.md`](hardware_interface.md).
