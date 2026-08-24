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
- CH32X035 PHY receive/transmit cancellation on VBUS loss when one package
  feature is enabled.

The application owns:

- the Embassy executor and interrupt binding;
- VBUS-present sensing and the firmware load-enable output;
- the independent hardware load gate;
- the source of user commands and the destination for diagnostics;
- USB CDC, displays, persistent settings, and every feature unrelated to PD;
- the GPIO choices. The scripted example's rev0 OPA1/PB10 binding and the
  alternate PA6/PA7 binding are both application choices.

## Dependency

Until a crates.io release is available, consume a reviewed checkout by path:

```toml
[dependencies.pd-sink]
package = "ch32x035-usb-pd-epr-sink"
path = "../ch32x035-usb-pd-epr-sink/crates/pd-sink"
default-features = false
features = ["ch32x035f8u6"]
```

Remote builds should pin a reviewed commit rather than follow a moving branch:

```toml
[dependencies.pd-sink]
package = "ch32x035-usb-pd-epr-sink"
git = "https://github.com/AnasMalas/ch32x035-usb-pd-epr-sink"
rev = "<reviewed commit SHA>"
default-features = false
features = ["ch32x035f8u6"]
```

This repository carries maintained descendants of `usbpd`, `usbpd-traits`,
and `ch32-hal`. Applications that use those crates directly should align on
the same versions: duplicate protocol or HAL crates increase firmware size and
can bypass the fixes documented in each `vendor/*/UPSTREAM.md`. A crates.io
release is deferred until the dependency arrangement and public APIs are
stable.

Choose exactly one of `ch32x035c8t6`, `ch32x035f7p6`, `ch32x035f8u6`,
`ch32x035g8r6`, `ch32x035g8u6`, or `ch32x035r8t6`. CH32X033 does not expose
the USB-PD peripheral and cannot use the CH32 PHY adapter.

## Application adapters

Implement `SinkRuntime` for a small application type. Its core services set the
PD policy's load permission, clear stale commands, wait for a new command, delay
after Hard Reset, and report typed observations. A single
`observe(SinkEvent)` implementation is enough. Flash-constrained firmware can
override the typed `on_*` callbacks directly so unused event formatting is
removed by the linker.

Implement `Ch32x035Port` for another small application type. It reports the
current VBUS-present level, waits for attach/detach notifications, clears a
stale detach notification at session start, publishes prompt PD load-control
updates, and optionally reports `PhyEvent` diagnostics.

The VBUS-present level is an active-high physical detector contract. High
means the detector is initialized and VBUS is above a board-chosen
minimum-valid threshold. Low means VBUS is below that threshold or the
detector is unavailable. Start with the published state low, qualify the raw
high continuously before publishing attachment, and publish the first raw low
or unavailable observation immediately without detach debounce. The wait
methods must remain cancellation-safe.

Document the detector's active polarity, nominal rising and falling
thresholds, worst-case threshold tolerance, hysteresis, assertion
qualification interval, and maximum deassertion-to-load-off latency. The
detector and independent physical load gate must default off when the MCU is
reset or unpowered, when the detector is unpowered, or when its state is
uncertain. This is a coarse minimum-VBUS predicate, not a measurement or proof
that VBUS agrees with the negotiated contract.

The reference implementation uses Embassy signals populated by a board-owned
GPIO/comparator supervisor task. Another application can use different pins
or detector circuitry without modifying the library.

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
- `transition_load_policy`, selected explicitly for the downstream load path;
- `max_auto_epr_attempts` and `hard_reset_recovery_ms`.

`SinkDevice::new` validates values that would be truncated on the wire or
advertised inconsistently. A 5 V-only application sets EPR fields and the
automatic-entry count to zero. A 28 V or 48 V application must explicitly
configure an EPR-capable board limit; the MCU alone does not make its external
power path safe for those voltages.

## Startup

The application creates the CH32 PHY with its chosen CC pins, implements
`Ch32x035SessionTimer`, and passes the PHY, `Ch32x035Port`, configuration, and
`SinkRuntime` to `Ch32x035SinkSession`. That high-level session owns PHY reset,
policy-engine construction, terminal local-error classification, and the
standard bounded recovery delays. See
[`examples/ch32x035-usb-pd-sink-firmware/src/main.rs`](../examples/ch32x035-usb-pd-sink-firmware/src/main.rs)
for a complete buildable consumer.

Every physical attachment starts by requesting fixed 5 V and learning source
capabilities. A high voltage is not selected until an explicit request or the
application's own later policy asks for one.

### Warm MCU-reset recovery

`Ch32x035SinkSession::new_recovering` is an explicit alternative for a short local MCU
restart when the application has trustworthy evidence that the same physical
port session may still be powered. The caller supplies a `RecoveryIntent` with
the retained SPR/EPR mode, user request, retry limit, and whether the output
latch should be restored. The DPM makes the corresponding `usbpd` startup send
Soft Reset first; EPR recovery remains in EPR and therefore waits for EPR
Source Capabilities instead of entering EPR again or requesting 5 V.

Do not infer this from VBUS presence alone and do not replay an old intent from
ordinary flash. A product can combine its MCU reset cause with a volatile
session token or another short-lived board-specific proof. A cold boot, a new
attachment, or uncertain evidence must call `Ch32x035SinkSession::new`.

Recovery immediately clears both software load controls. The retained target
is planned against the newly received capabilities, and the output latch is
restored only after a fresh Accept and PS_RDY. Only a transient `Wait` is
retried, up to `maximum_attempts`. Reject, unavailable/mismatched
capabilities, Hard Reset, detach, protocol loss, a replacement request, and
Output Off cancel the automatic restore and emit typed `SinkEvent`
observations.

If an immediate Output Off path bypasses `SinkDevice::get_event` (as the
reference transports do), implement `SinkRuntime::recovery_cancel_requested`
with the same application-owned latch/token. This prevents an Output Off that
arrives during a PD exchange from being undone by the later PS_RDY callback;
the observation path remains independent from the load-control path.

## Load safety

`SinkRuntime::set_pd_load_permitted` is a PD-policy input, not ownership of the
product output. `true` means a contract has reached PS_RDY. While PD policy is
active, `false` inhibits the load immediately. When no usable PD session is
active, an application may instead let an explicit user latch control a
non-PD supply, such as a USB-A power source. The library deliberately does not
require ADC measurement or prescribe how that product policy validates its
supply.

One application-level formulation is:

```text
PD_ALLOWS_LOAD = (NOT PD_POLICY_ACTIVE) OR PD_LOAD_PERMITTED OR TRANSITION_BYPASS
MCU_LOAD_ENABLE = USER_OUTPUT_ENABLED AND PD_ALLOWS_LOAD
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

The reference marks PD policy active when a contract Request starts and marks
it unmanaged after an initial/uncontracted partner is classified as
unresponsive. Consequently repeated passive PD retries do not knock out a
user-enabled USB-A load. A real PD session loss first clears the user latch;
falling back to unmanaged operation never silently re-enables it.
`LoadControlState` implements this small arbitration as an optional, pin-free
helper; it does not require an ADC or own the final load output.

VBUS removal must turn the load path off without relying on the executor, the
PD stack, or a functioning MCU. The reference implementation is documented in
[`examples/ch32x035-usb-pd-sink-firmware/docs/hardware_interface.md`](../examples/ch32x035-usb-pd-sink-firmware/docs/hardware_interface.md).

The minimum-VBUS predicate cannot validate an active contract's voltage. A
board that must enforce contract-voltage agreement needs a separate,
appropriately accurate measurement and policy path in addition to this
detector and the default-off hardware gate.

Before each Request, `ContractTracker` classifies the wire transition against
the confirmed RDO. Identical maintenance and a same-encoded-voltage request
with known, nondecreasing operating current can retain load permission. For a
voltage change, reduced or uncertain current, or missing confirmed contract,
`TransitionLoadPolicy` explicitly selects manual re-arm, automatic restoration
after PS_RDY, or deliberately uninterrupted operation for a downstream path
rated for the complete transition.

Both inhibiting policies issue their load-control action before the PD Request
is returned. `InhibitUntilManualRearm` also clears the user latch;
`InhibitUntilReady` preserves it and restores only PD permission after PS_RDY.
`Uninterrupted` never weakens detector-low, detach, Hard Reset, protocol-loss,
or terminal-fault cutoff.
`SinkRuntime::on_contract_transition_started` receives the library-owned
classification for diagnostics; telemetry delivery is not part of the cutoff
path. Current comparisons use the limited current actually encoded in the RDO,
not the Source PDO maximum or an unbounded user demand.

`output-on` and `output-off` update only the runtime's user latch through
`SinkRuntime::set_user_output_enabled`. They do not submit a PD Request, alter
the desired contract, or enter/exit EPR. Scripted rev0 profiles default it off,
clear it on physical detector loss and real PD-session safety faults, and use
`InhibitUntilReady` for ordinary voltage transitions. Direct custom builds may
choose a different reset and transition policy. The USB and text transports
apply Output Off before sending an acknowledgement instead of waiting for the
PD policy engine to reach `Ready`.

## EPR entry and exit policy

Target-driven entry retains the requested EPR contract throughout mode entry;
it does not insert an intermediate 5 V Request. Targetless manual or automatic
discovery instead requires an `EprEntryPolicy` and the confirmed plan. With
`PreserveVoltage`, the EPR capability response is re-expressed through a
requestable SPR object in positions 1-7 only when its encoded voltage is equal
and its known encoded operating-current capability is not lower. Fixed, PPS,
and SPR AVS contracts use the same rule. This `EPR_Request` does not interrupt
load permission. An eventual voltage-changing EPR target is still classified
separately and inhibits the load before its Request.

`EprEntryFallback::Safe5V` explicitly selects fixed 5 V if continuity cannot
be established. `Refuse` rejects before entry when the latest SPR capabilities
already prove continuity impossible. If the EPR capability response changes
during entry, Refuse reports a typed refusal and conservatively establishes
fixed 5 V before exiting back to SPR; it never claims continuity through an
insufficient or unknown-current candidate. Initial automatic discovery after
the normal 5 V boot contract remains at 5 V naturally.

The direct controller API requires an `EprExitPolicy` and the confirmed plan
from `ContractTracker::active_plan()`. `PreserveVoltage` validates only SPR
objects in positions 1-7 from the latest EPR capability list. Its candidate
must encode the same voltage and at least the active RDO's limited operating
current; an uncertain power-limited current is not sufficient. A valid
candidate is requested with `EPR_Request`, and EPR Mode Exit is sent only after
Accept and PS_RDY. If the confirmed contract already uses a valid SPR object,
Exit can be sent directly.

`EprExitFallback::Refuse` leaves the confirmed EPR contract and load permission
untouched when no continuity candidate exists. `Safe5V` explicitly establishes
the fixed 5 V SPR object before Exit, unless that contract is already active.
The reference compact/text `exit-epr` command deliberately selects `Safe5V`
for conservative backward compatibility; exposing another policy does not
require changing compact control protocol v1.
