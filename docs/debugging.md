# Debugging real USB-PD hardware

This guide is for failures that appear only on hardware: negotiation timeouts,
Hard Reset loops, missed GoodCRC, EPR keepalive loss, unexplained VBUS cycles,
and failures that start when the rest of an application is enabled. It
complements the [firmware architecture](architecture.md),
[integration guide](integration.md), [compact control protocol](control_protocol.md),
and [source interoperability notes](charger_interoperability.md). Those
documents remain authoritative for ownership, framing, and source policy.

The central rule is simple: reproduce with the exact known-good reference,
record evidence at one boundary at a time, and never let observation delay the
PD task.

## Keep facts separate from explanations

Use these labels in notes and issue reports:

- **Fact:** directly observed in a device record, analyzer capture, scope
  trace, register sample, or reproducible build.
- **Inference:** the smallest explanation consistent with the facts, with the
  supporting observations stated.
- **Hypothesis:** an explanation that still needs a discriminating test.

When evidence supersedes a useful older hypothesis, keep the old lesson but
mark why it is no longer current. For example, a timeout that first looked
like scheduler starvation was later localized below the executor to the
USB-PD TX-to-RX handoff. The original symptom remains useful; the original
cause does not.

## Freeze the experiment first

Start with the exact public reference firmware and preserve enough identity to
rebuild it. Record all of the following before changing code:

- firmware build ID or artifact SHA-256;
- library and repository revision;
- complete Cargo feature set or scripted profile;
- device boot/session identifier and device uptime, if the application exposes
  them;
- source model, exact port, and activity on its other ports;
- cable identity, rating, orientation, and whether it is direct or split;
- board revision and the physical VBUS-present/load-cutoff arrangement;
- requested PDO/APDO, active contract, and the first unexpected event.

Host software should preserve the raw device records as well as its formatted
view. Change one causal layer per firmware build. A build that changes the PD
stack, display traffic, USB transport, and logging simultaneously cannot
identify which change mattered.

## Events that look alike from the host

Do not use “the serial port disappeared” as a root cause. At least five
different events can produce that symptom:

| Event | What actually changed | Best discriminator |
|---|---|---|
| MCU reboot or brownout | CPU, RAM, and USB restart | new boot ID, reset flags, uptime returning to zero |
| Source-driven VBUS cycle during Hard Reset | Source returns to default power and may briefly remove VBUS | Hard Reset origin/reason plus a VBUS scope trace |
| USB CDC loss | USB transport disappeared while PD may remain healthy | continuing device uptime or an independent trace channel |
| CC detach | the Type-C attachment ended | CC/VBUS predicate and a typed detach event |
| Application load cutoff | only the external load gate changed | load-supervisor record or direct gate measurement |

The [hardware interface](../examples/ch32x035-usb-pd-sink-firmware/docs/hardware_interface.md)
defines the reference VBUS/load boundary. A product can use a different
external comparator or gate, but it must give diagnostics distinct names for
transport, attachment, protocol, and load state.

Host console timestamps are receipt times, not device event times. Records may
have accumulated before USB connected or while the host was stalled. Timing
claims require device-side monotonic timestamps or a logic analyzer. Put a
per-boot sequence number and device timestamp around trace records in the
application; the library's eight-byte numeric event intentionally contains
neither.

## Use the smallest telemetry that answers the question

Instrumentation can create or hide the failure. Synchronous formatting,
continuous high-rate telemetry, and a blocking USB writer have all disturbed
PD timing in real firmware. Use this ladder and stop as soon as one layer
localizes the fault.

| Level | Use | Cost and constraints |
|---|---|---|
| Normal compact control | Attachment, contract, typed Hard Reset cause, protocol loss, and exception summaries | Default for reproduction. Keep unsolicited traffic bounded and lossy. |
| Numeric protocol trace | Typed attach/detach plus TX, GoodCRC, valid RX headers, retry/error, Hard Reset, and EPR keepalive ordering | Enable the default-off `numeric-trace` feature and install its callback. The callback must perform only one bounded RAM copy. |
| Driver-boundary trace | Determine whether loss occurs between the policy task, HAL call, ISR, and DMA | Temporary application/HAL diagnostic instrumentation; there is no shipped profile or stable public feature for this tier. Timestamping an ISR changes it, so measure and remove it afterward. |
| Formatter-heavy trace | Human exploration when compact records are insufficient | Dedicated diagnostic image only. Never infer timing equivalence with the normal image. |

`usb-epr-diagnostic` is currently a compatibility alias of `usb-epr`; it does
not enable numeric or driver-boundary tracing. `usb-epr-text` supplies the
reference ASCII console, not a complete internal driver trace. Enabling
`usbpd/log` or adding synchronous driver formatting requires a deliberately
separate diagnostic build.

At the revision that added the CH32X035 TX-to-RX repair, adding
`numeric-trace` to the G8U6 `usb-epr-uninterrupted` feature set changed the
complete image from 50,256 to 51,728 flash bytes and from 4,120 to 4,128
static-RAM bytes, before an application-owned ring buffer. The corresponding
`usb-epr-text` profile was 58,400 flash bytes and 7,320 static-RAM bytes; that
difference also includes its text transport and queues and is not a pure
formatter measurement. Re-measure both size and behavior after every
toolchain or application change.

With `numeric-trace` disabled, its module, callback storage, event calls, and
critical-section dependency are absent. With it enabled, event construction
and a short callback lookup still execute even when no callback is installed.
The ABI begins at protocol transactions: combine it with the application's
existing typed attach/detach events. It emits a valid `RxMessage`, not generic
driver RX-begin/RX-complete events; add those only as temporary
driver-boundary records when that distinction is required.

## Numeric trace without backpressure

The feature exposes
[`set_numeric_trace_callback`](../vendor/usbpd/src/numeric_trace.rs). The
callback receives one `Copy`, C-layout, eight-byte `NumericTraceEvent` and
runs synchronously in the protocol task. It must not format, allocate, wait,
perform USB or other I/O, acquire an application mutex, or call back into the
PD stack.

A robust application adapter does only this in the callback:

```text
read monotonic tick and next sequence
copy { boot_id, tick, sequence, NumericTraceEvent } into a fixed RAM slot
advance an overwrite-oldest producer index
return
```

A separate low-priority task drains the ring through a lossy, nonblocking
transport. A slow or disconnected host must never backpressure PD, safety, or
load cutoff. Application workload such as ADC, touch, or display updates is
not library telemetry; disable it only as an isolation experiment.

Make trace loss explicit:

- assign every attempted record a monotonically increasing sequence number;
- keep a per-boot total of overwritten records;
- report the delta since the last reported total, not the same cumulative loss
  after every reconnect;
- retain unread RAM records across a USB reconnect when the MCU did not reboot;
- include the boot ID so sequence resets cannot be mistaken for wraparound;
- treat a sequence gap or `TRACE lost` as loss of observability, not proof of a
  protocol failure.

Ordinary detach and successful periodic keepalives generally do not belong in
a persistent flash exception log. Store unusual reset/protocol summaries and
the trace-loss counters; keep high-rate success records in bounded RAM.

## Numeric event decoding

ABI version 1 is defined in
[`vendor/usbpd/src/numeric_trace.rs`](../vendor/usbpd/src/numeric_trace.rs)
and re-exported as `pd_sink::numeric_trace`. The fixed record is:

```text
kind:u8 | code:u8 | message_id:u8 | counter:u8 | header:u16 | detail:u16
```

All multi-byte fields use the target's C layout; CH32X035 is little-endian.
Unavailable values are `0xff` and `0xffff`. Message events store the raw PD
Header in `header`; extended messages store the Extended Header in `detail`.

| `kind` | Event | Meaning of `code` |
|---:|---|---|
| 1 | `TxStart` | message-specific; inspect Header |
| 2 | `GoodCrcWait` | 0 software |
| 3 | `GoodCrcReceived` | 0 software, 1 hardware |
| 4 | `TxHardwareRetry` | 1 hardware path |
| 5 | `TxRetry` | transmit reason below |
| 6 | `TxSuccess` | software/hardware path |
| 7 | `TxFailure` | transmit reason below |
| 8 | `RxMessage` | extended-control type when applicable |
| 9 | `GoodCrcTransmitted` | 0 software, 1 hardware |
| 10 | `RxRetransmission` | inspect Header and MessageID |
| 11 | `ProtocolError` | protocol-error code below |
| 12 | `HardReset` | Hard Reset phase below |
| 13 | `EprKeepAlive` | keepalive phase below |

Transmit-reason codes are 1 driver-discarded, 2 GoodCRC timeout, 3 Hard
Reset, 4 detached, 5 retries exceeded, 6 acknowledgement mismatch, and 255
other.

Protocol-error codes are:

| Code | Error | Code | Error |
|---:|---|---:|---|
| 1 | RX discarded | 8 | RX acknowledgement mismatch |
| 2 | RX detached | 9 | TX discarded |
| 3 | RX Soft Reset | 10 | TX detached |
| 4 | RX Hard Reset | 11 | TX Hard Reset |
| 5 | RX timeout | 12 | local TX validation |
| 6 | RX unsupported | 13 | TX retries exceeded |
| 7 | RX parse error | 14 | unexpected message |

For local TX validation, `detail` is 1 unsupported unchunked extended
messages, 2 invalid AVS voltage alignment, or 3 a required multi-chunk
transmission. Hard Reset phases are 1 received, 2 transmit start, 3 transmit
retry, 4 transmit complete, and 5 transmit failure. Its `detail` uses the
[stable Hard Reset cause codes](control_protocol.md#compact-control-protocol)
when `hard-reset-reasons` is enabled; cause 255 means unspecified, while
`0xffff` means the numeric detail is unavailable. EPR keepalive phases are 1
request, 2 acknowledged, 3 timeout, 4 unexpected response, and 5 protocol
failure.

Treat the Rust enums as authoritative. They are `non_exhaustive`; decoders
must preserve and display unknown future values instead of rejecting the
record.

## Isolation sequence

This order found a real full-application timing fault without repeatedly
changing policy timers:

1. Reproduce with the exact PD-only public reference and the same source,
   port, cable, orientation, and requested contract.
2. Run the exact full application workload with normal compact diagnostics.
3. Enable the bounded numeric trace and reproduce without changing policy.
4. Separately remove or restore USB telemetry, display transfers, touch
   scanning, ADC work, and load-supervisor reporting.
5. Replace blocking work with bounded ISR work, nonblocking/DMA peripherals,
   unchanged-frame suppression, and lossy USB where appropriate.
6. Add temporary driver-boundary timestamps or a logic-analyzer trigger only
   after the higher boundary has been isolated.
7. Return to the normal image and confirm the repair without diagnostic
   instrumentation.

This sequence does not mean every application should use DMA. It means a PD
deadline must not depend on unrelated application I/O.

### Split or externally powered cables

Separately powering the MCU/USB side while the PD Source cycles VBUS is an
excellent way to retain post-failure records. It also changes the experiment:

- it cannot validate the product's source-VBUS sensing or detach semantics;
- it can keep USB alive through a Source Hard Reset that normally removes MCU
  power;
- forcing presence while CC remains attached can create an artificial session;
- conclusions about protocol ordering may remain valid, while conclusions
  about power loss, reattach, and load cutoff do not.

Label every capture as direct-powered or split-powered and repeat final safety
tests on the real power path.

## Symptom-driven playbooks

| Symptom | Minimum next evidence | Do not conclude yet |
|---|---|---|
| No `Source_Capabilities` | VBUS predicate, selected CC orientation, attach event, then RX begin/completion | that the Source lacks PD merely because the host saw no line |
| Immediate or repeating Hard Reset | typed origin and cause, boot ID/uptime, first preceding protocol error | that the MCU rebooted or the Source initiated every reset |
| EPR keepalive failure | keepalive request/phase plus TX, GoodCRC, RX, and Hard Reset records | that the keepalive timer is too short before locating the missing transaction |
| Missed GoodCRC | TX start/completion and GoodCRC wait, then temporary HAL/ISR or analyzer timing | that the Source rejected the message; GoodCRC is link acknowledgement |
| Stable until USB/telemetry starts | sequence/loss counters and one-at-a-time workload removal | that the charger or policy engine is unstable |
| Web Serial loss | device boot ID/uptime after reconnect, independent VBUS/CC evidence | detach, MCU reset, and USB loss are the same event |
| VBUS off-on flash | Hard Reset origin/cause and simultaneous VBUS/MCU-rail capture | a brownout or firmware reset without measuring the MCU rail |
| `TRACE lost` | overwritten delta, sequence gap, host connection state | protocol messages were necessarily lost |
| First plug works, replug fails | new boot/session IDs, VBUS waiter state, CC orientation, retained Source contract | permanent hardware damage or an EPR policy fault |

If the minimal evidence identifies a lower boundary, instrument that boundary
only. More logs are not automatically better evidence.

## Case study: immediate response lost after TX

The following is a clearly labelled historical case study from a private
full-workload CH32X035 consumer board. Product UI details are irrelevant to
the reusable failure.

**Facts:** A PD analyzer showed that the Source received the Sink's EPR
KeepAlive request, transmitted its immediate GoodCRC, and then sent the EPR
KeepAlive Acknowledgement. The Sink nevertheless reported transmission
failure because it missed the request's GoodCRC. Compact policy records alone
could not distinguish scheduler delay from a lower driver gap.

Temporary driver-boundary timestamps showed that the old HAL completed TX,
woke the executor, and armed RX 54-83 microseconds later. WCH's documented
sequence instead changes directly from TX to RX. Diagnostic firmware build
`0x00070003`, using library revision `3813021`, disabled the CC transmitter
and pre-armed a stable RX DMA buffer in the USBPD TX-end ISR before waking the
executor. Six full-workload captures then contained 288 EPR keepalive requests
and 288 acknowledgements.

**Inference supported by those facts:** the sustained failure was the HAL
TX-to-RX blind window, not an EPR policy timer or general application
scheduler failure. The stable DMA address was part of the repair because an
async PHY value can move while peripheral DMA still refers to its buffer.
The reusable implementation is in the
[CH32X035 USB-PD HAL](../vendor/ch32-hal/src/usbpd/mod.rs), with automatic
GoodCRC-versus-turnaround selection in the
[public CH32X035 adapter](../crates/pd-sink/src/ch32x035.rs).

**Superseded hypothesis:** application workload initially appeared to delay
the policy task. It did make the old race easier to trigger, but changing
application scheduling alone was not the reusable fix. This is why a failure
near a protocol deadline should be traced through policy, driver, ISR, and DMA
before changing the deadline.

## Issue-report checklist

Before filing a hardware issue, include:

- repository/library revision, artifact hash or build ID, profile, and exact
  features;
- board revision and VBUS-present/load-cutoff implementation;
- source model and port, other active ports, cable identity/rating, and both
  plug orientations tried;
- direct versus split/external MCU power;
- boot/session ID, device uptime, reset cause if known, and whether USB
  disappeared;
- requested supply and last confirmed contract;
- first unexpected typed event with a short raw record window before and after
  it;
- numeric-trace ABI version, sequence range, and overwritten-record delta;
- scope/analyzer evidence with its clock relationship stated;
- whether the exact public reference passes and which single workload layer
  makes the failure return.

Raw captures and a small reproduction are more useful than a long formatted
console transcript. Remove secrets and proprietary application data before
attaching them.
