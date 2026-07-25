# Firmware architecture

## Product objective

The CH32X035F8U6 is a pure USB-C power sink. A user can inspect every source
offer, select a fixed PDO, or ask for any in-range PPS/AVS voltage. The
result reports the exact wire-encoded voltage and a conservative usable
current derived from source and configured product limits.

## Layer boundaries

### CH32 hardware layer

The vendored `ch32-hal` owns clocks, interrupts, USBFS, and the USB-PD PHY. The
F8U6's linker map exposes a 62 KiB application region. Local repairs include a
34-byte PD receive buffer, strict transmit lengths, bounded error recovery,
detach cancellation, and active-CC sampling at the 1.23 V comparator threshold
for PD 3.x SinkTxOK.

The product-specific USBFS module is intentionally compact: fixed endpoint 0
control, endpoint 1 notification, endpoint 2 bulk OUT, and endpoint 3 bulk IN.
Control requests execute in the USB interrupt; application tasks exchange one
64-byte packet at a time through reset-safe async wrappers.

### USB-PD protocol and policy layer

The vendored `usbpd` engine supplies message parsing, protocol counters,
timers, and sink states. Local work adds chunked EPR Source Capabilities,
first-class SPR AVS decoding, mode-aware requests, entry/exit/failure
handling, EPR keepalive, bounded PHY
retries, Source_Info, lifecycle callbacks that invalidate contracts, and
revision-aware SinkTx collision avoidance. If a PD 3.x source advertises
SinkTxNG, the engine retains the exact pending command or refresh, continues
servicing source-initiated messages, polls CC, and transmits when SinkTxOK
appears. PD 1.0/2.0 sessions bypass that PD 3.x mechanism. Source queries for
ordinary Sink Capabilities, EPR Sink Capabilities, and the 24-byte Sink
Capabilities Extended block receive padded, object-count-correct frames; the
reference EPR profile's 140 W Operational PDP matches the EPR Enter message
while its independent maximum/request ceiling is 240 W. Receive parsing
requires the exact `2 + 4 * NumDO` wire length, bounds chunk assembly, rejects
out-of-order or inconsistent chunks, and maps malformed or reserved partner
traffic to protocol recovery instead of panicking.

### Public library and device policy manager

The default `crates/pd-sink` build has no MCU dependency. It owns:

- supported source PDO decoding and validity checks, with raw/position-preserving
  retention of unrequestable battery and variable offers;
- fixed/PPS/SPR-AVS/EPR-AVS selection and encoding;
- direct `max` selection for every requestable PDO position, with adjustable
  PDOs resolving to their advertised maximum voltage;
- bounded compatibility handling for noncanonical commercial PPS offers,
  while retaining the advertised voltage range and 5 A sink ceiling;
- source, board, cable, power, and requested-current limits;
- contract state and the user command grammar;
- EPR intent, discovery, and exit sequencing;
- safe 5 V fallback when changed capabilities invalidate a retained request;
- the detach safety model.

The reusable `SinkDevice` DPM converts the vendored stack's PDOs into this
integer-only model. Applications supply a `SinkRuntime` adapter for commands,
diagnostics, delays, and their firmware-controlled load request. Avoiding
floating-point unit conversions saves several kilobytes and makes capability
reports deterministic.

With the optional `ch32x035` feature, `Ch32x035UsbPdDriver` connects the DPM to
the integrated CH32X035 PHY. Its `Ch32x035Port` trait deliberately contains no
pin assignments: the application supplies VBUS-present waits, immediate load
disable, and diagnostics.

Every attachment starts with no retained user intent. The sequence for an
EPR-capable profile is:

```text
SPR capabilities -> request 5 V -> PS_RDY -> Source_Info
                 -> EPR entry attempt -> EPR capabilities
                 -> EPR request for fixed 5 V -> PS_RDY -> await user command
```

A Hard Reset invalidates the contract and holds the load off for a fixed
two-second source-recovery window without requiring a PA6 edge. The product
permits at most two automatic EPR entry attempts per physical attachment. If
the retry also fails, it remains usable in SPR and leaves further EPR retries
to explicit user commands. Software-session restarts preserve this budget and
use a bounded cooldown instead of creating a reset storm.

### Port and safety supervisor

The reference application's supervisor owns PA6 attach/detach and PB12 load
enable; neither pin is selected by the library. Its invariant is that reset,
detach, protocol loss, or an unconfirmed transition leaves the load off. The
separate hardware gate in `hardware_interface.md` remains the primary fast
cutoff. PA6 is not required to pulse for protocol recovery after Hard Reset;
this keeps the isolated always-high fixture usable while preserving PA6 as a
load-safety input.

### Diagnostics and commands

Messages are produced independently of transport. Builds can select LinkE
SDI, native USB CDC, both, or neither. USB output uses a bounded non-blocking
32-line queue sized to retain a complete 11-PDO planning report, so a missing
or slow host cannot block PD or safety work. USB input and delayed bench tasks
feed the same typed four-entry command queue.

In the normal compact USB-control image, `plans` dry-runs `Demand::Maximum`
against every current PDO and reports either the exact encoded voltage, usable
current, confidence, and limit or an explicit unsupported result. It neither
retains user intent nor starts a PD AMS, and finishes by reporting the
still-active contract. The development text-console profiles omit this verbose
planner to preserve flash; use the compact `usb-epr` image and browser for
complete capability-policy inspection.

`Alert`, general `Status`, and `PPS_Status` are delivered as typed application
events. A non-battery Source Alert schedules one `Get_Status` request so PPS
CV/CL transitions and fault/thermal changes can update a display or indicator
without polling. Applications may request PPS_Status manually or periodically
for source-side voltage/current measurements and compatibility with Sources
that fail to send the expected Alert.

## Verification strategy

- the `pd-sink` unit tests validate every requestable PDO family, battery/variable
  rejection without losing advertised positions, quantization, limits, command
  parsing, direct maximum selection across positions 1-11, every adjustable
  minimum/maximum endpoint, Source_Info limits across requestable families,
  controller behavior, contract lifecycle, safety debounce, reusable DPM
  contract/reset behavior, configuration validation, and automatic EPR entry.
- `tests/protocol` drives the real vendored policy engine with scripted wire
  messages, including typed SPR AVS and an ordinary adjustable Request,
  two-chunk EPR capabilities, 48 V fixed, an arbitrary EPR AVS point, legal EPR exit,
  Source_Info, PPS_Status, Alert-triggered general Status, an arbitrary PPS
  refresh, source traffic while a
  sink AMS is held by SinkTxNG, PD 2.0 bypass, detach, and bounded retry
  failure. A `Wait` trace proves the deferred user request is replanned after
  SinkRequestTimer even when the Source inserts its own AMS, and periodic PPS
  maintenance uses the same DPM/contract path as user requests. Adversarial
  traces cover truncated and oversized frames, malformed chunks, and reserved
  EPR values through the real sink policy engine.
- `scripts/check.ps1` builds every runtime and bench feature combination.
- `docs/first_board_verification.md` turns first hardware observations into a
  repeatable evidence checklist and future regression tests.

Desktop tests prove decisions and bytes, not CH32 register behavior or analog
timing. Those remain explicit first-board gates.
