# `ch32x035-usb-pd-epr-sink`

This `no_std` crate contains reusable USB Power Delivery sink policy and an
optional adapter for the CH32X035 integrated USB-PD PHY. It deliberately does
not own an executor, application entry point, GPIO assignment, USB console,
GUI, display, persistent settings, or product policy.

The API and dependency arrangement are still under review, so the crate is not
published to crates.io.

## Add the dependency

For a nearby reviewed checkout:

```toml
[dependencies.pd-sink]
package = "ch32x035-usb-pd-epr-sink"
path = "../ch32x035-usb-pd-epr-sink/crates/pd-sink"
default-features = false
features = ["ch32x035f8u6"]
```

For a remote reproducible build, pin an exact commit:

```toml
[dependencies.pd-sink]
package = "ch32x035-usb-pd-epr-sink"
git = "https://github.com/AnasMalas/ch32x035-usb-pd-epr-sink"
rev = "<reviewed commit SHA>"
default-features = false
features = ["ch32x035f8u6"]
```

The complete CH32 firmware also uses the maintained `usbpd`,
`usbpd-traits`, and `ch32-hal` descendants from the same checkout. Keep direct
dependencies aligned with the entries in the
[reference firmware manifest](../../examples/ch32x035-usb-pd-sink-firmware/Cargo.toml);
mixing unrelated protocol or HAL revisions can bypass required fixes or
duplicate code.

## Features

| Feature | Effect |
|---|---|
| `ch32x035c8t6`, `ch32x035f7p6`, `ch32x035f8u6`, `ch32x035g8r6`, `ch32x035g8u6`, or `ch32x035r8t6` | Selects one CH32X035 package and adds the pin-agnostic PHY driver and `Ch32x035Port` adapter |
| `hard-reset-reasons` | Preserves typed local Hard Reset causes from the maintained policy engine |

The default build enables `hard-reset-reasons` but no MCU integration.
Applications that need the CH32 adapter normally use
`default-features = false` with exactly one package feature and enable
`hard-reset-reasons` only when that diagnostic detail is worth the firmware
space.

All six USB-PD-capable CH32X035 package variants are supported. CH32X033 is
not: despite sharing much of the family, it has USB but no integrated USB-PD
peripheral, so the PHY adapter cannot run on it.

## Flash use

These request-planning paths add roughly the following release/LTO flash over
the same minimal CH32X035 program. Each row is an alternative, not a
cumulative cost.

| Application behavior | Added flash |
|---|---:|
| Decode advertised PDOs | ~1 KiB |
| Request a fixed PDO | ~4 KiB |
| Request PPS at 12 V | ~5 KiB |
| Request SPR AVS at 12 V | ~5 KiB |
| Change PPS voltage at runtime | ~5 KiB |
| Request EPR AVS at runtime | ~5 KiB |

These paths construct and retain requests; they do not include the on-wire
policy engine, PD PHY, executor, console, or application. The complete
reference sink, including those runtime pieces and its safety supervisor, is
about 40 KiB without a console or 51 KiB with compact USB control. Fixed, PPS,
and EPR configurations are broadly the same size because they are selected at
runtime.

## Integration sequence

1. Build `SinkConfig` from the complete board's voltage, current, and power
   limits. Do not derive safe limits from a charger's label.
2. Implement `SinkRuntime` for command input, typed observations, delays, and
   the firmware load-enable request.
3. With one CH32X035 package feature, implement `Ch32x035Port` for real
   VBUS-present state, cancellation-safe attach/detach waits, immediate load
   disable, and optional PHY diagnostics.
4. Construct `Ch32x035UsbPdDriver`, `SinkDevice`, and the maintained
   `usbpd::sink::policy_engine::Sink`.
5. Reset and run the policy engine using an application-owned bounded restart
   loop.
6. Independently enforce a hardware-default-off load gate.

The final assembly has this shape; the board-specific types and setup are
intentionally omitted:

```rust,ignore
let driver = Ch32x035UsbPdDriver::new(phy, AppPort);
let device = SinkDevice::new(sink_config(), AppRuntime)?;
let mut sink: usbpd::sink::policy_engine::Sink<_, AppTimer, _> =
    usbpd::sink::policy_engine::Sink::new(driver, device);

sink.run().await
```

The [integration guide](../../docs/integration.md) explains each adapter and
configuration field. The buildable
[reference `main.rs`](../../examples/ch32x035-usb-pd-sink-firmware/src/main.rs)
is the canonical end-to-end implementation.

## Primary API areas

| Need | Types/modules |
|---|---|
| Decode source offers | `SourceCapabilities`, `AdvertisedPdo`, `SourceSupply` |
| Plan a request | `RequestPlanner`, `RequestContext`, `SinkLimits`, `Demand` |
| Track a contract | `ContractTracker`, `ContractState` |
| Classify a renegotiation | `ContractTransition`, `ContractTransitionKind` |
| Select EPR entry/exit behavior | `EprEntryPolicy`, `EprEntryFallback`, `EprExitPolicy`, `EprExitFallback` |
| Run the reusable DPM | `SinkConfig`, `SinkDevice`, `SinkRuntime`, `SinkEvent` |
| Connect the CH32 PHY | `Ch32x035UsbPdDriver`, `Ch32x035Port`, `PhyEvent` |
| Exchange compact host frames | `control` module and `CONTROL_PROTOCOL_VERSION` |

## Safety boundary

`SinkRuntime::set_pd_load_permitted(true)` and
`Ch32x035Port::set_pd_load_permitted(true)` are PD-policy permissions after a
confirmed contract. The application owns a separate user output latch. None is
an independent safety mechanism. Hardware must enforce:

```text
LOAD_ON = PD_LOAD_PERMITTED AND USER_OUTPUT_ENABLED AND VBUS_PRESENT AND HARDWARE_OK
```

Detach, Hard Reset, protocol loss, and invalid state must disable the load.
Negotiated current is a permitted ceiling, not guaranteed source-side
electronic current limiting.

The `output-on` and `output-off` commands call the runtime's application-owned
user latch only. They never change the requested PD contract or EPR mode.
