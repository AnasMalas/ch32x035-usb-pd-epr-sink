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

The high-level CH32 session keeps the maintained `usbpd` and `usbpd-traits`
implementation private. Board firmware only needs a direct `ch32-hal`
dependency for peripheral setup, aligned with the
[reference firmware manifest](../../examples/ch32x035-usb-pd-sink-firmware/Cargo.toml).

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
   limits and select an explicit `TransitionLoadPolicy`. Do not derive safe
   limits from a charger's label.
2. Implement `SinkRuntime` for command input, typed observations, delays, and
   the firmware load-enable request.
3. With one CH32X035 package feature, implement `Ch32x035Port` for a real
   active-high minimum-VBUS detector, cancellation-safe attach/detach waits,
   immediate load disable, and optional PHY diagnostics.
4. Implement `Ch32x035SessionTimer` using the application's monotonic timer.
5. Construct and run `Ch32x035SinkSession`; it owns PHY reset, policy-engine
   construction, terminal error classification, and bounded recovery delays.
6. Independently enforce a hardware-default-off load gate.

The final assembly has this shape; the board-specific types and setup are
intentionally omitted:

```rust,ignore
let mut session = Ch32x035SinkSession::<_, _, AppTimer>::new(
    phy,
    AppPort,
    sink_config(),
    AppRuntime,
)?;
session.run().await
```

`Ch32x035SinkSession::new` always selects a fresh, conservative attachment. A product
that has trustworthy short-lived evidence of a local MCU reset can instead
pass an explicit `RecoveryIntent` to `Ch32x035SinkSession::new_recovering`. That path
starts with a wire Soft Reset, keeps both software load controls off, bounds
transient retries, and restores the output latch only after a newly accepted
contract reaches PS_RDY. It deliberately does not read reset flags or persist
the target; see the [integration guide](../../docs/integration.md#warm-mcu-reset-recovery)
for the safety and cancellation requirements.

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
| Select transition load continuity | `TransitionLoadPolicy` |
| Arbitrate PD versus an application user latch | `LoadControlState` |
| Select EPR entry/exit behavior | `EprEntryPolicy`, `EprEntryFallback`, `EprExitPolicy`, `EprExitFallback` |
| Run the reusable DPM | `SinkConfig`, `SinkDevice`, `SinkRuntime`, `SinkEvent` |
| Recover after a proven warm reset | `RecoveryIntent`, `RecoveryCancellationReason`, `Ch32x035SinkSession::new_recovering` |
| Run the CH32 PHY lifecycle | `Ch32x035SinkSession`, `Ch32x035SessionTimer`, `Ch32x035Port`, `SinkSessionEvent` |
| Build a custom low-level integration | `Ch32x035UsbPdDriver`, `SinkDevice`, `PhyEvent` |
| Exchange compact host frames | `control` module and `CONTROL_PROTOCOL_VERSION` |

## Safety boundary

The `Ch32x035Port` VBUS predicate has a precise physical meaning. High means
an initialized detector reports VBUS above a board-chosen minimum-valid
threshold. Low means VBUS is below that threshold or the detector is
unavailable. Initialize it low, require a continuous high for the documented
assertion qualification interval, and react to the first low/unavailable
observation without debounce. The board integration must document active
polarity, nominal rising and falling thresholds, worst-case tolerance,
hysteresis, assertion time, and maximum deassertion-to-load-off latency.

This predicate is deliberately coarse. High does not measure VBUS and is not
evidence that the rail equals the voltage or tolerance required by the active
PD contract.

`SinkRuntime::set_pd_load_permitted` and
`Ch32x035Port::set_pd_load_permitted` publish PD-policy permission. They do not
give PD exclusive ownership of the product load. The application owns a
separate user output latch and may explicitly permit that latch to control a
non-PD supply when no usable PD session is active. No ADC or particular
voltage-validation scheme is required by the crate; those are product choices.

The application still combines its final MCU request with independent hardware
gates:

```text
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

`LoadControlState` is an optional pin-free helper for this arbitration. It
allows user control while PD is unmanaged, latches active-session safety
cutoffs until the application consumes them, and prevents lifecycle updates
from overwriting a pending user-latch clear. It still owns no GPIO, comparator,
ADC, or product policy.

Detach, Hard Reset, protocol loss, and invalid state must disable the load.
`TransitionLoadPolicy` separately selects manual re-arm, restoration after
PS_RDY, or deliberately uninterrupted operation for a downstream path rated
for every transition. None is an independent safety mechanism.
Negotiated current is a permitted ceiling, not guaranteed source-side
electronic current limiting.

The `output-on` and `output-off` commands call the runtime's application-owned
user latch only. They never change the requested PD contract or EPR mode.
