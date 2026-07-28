# First-board verification

This procedure stops at safe 5 V, full source-capability discovery, and detach
cutoff. Do not issue a high-voltage command during this pass.

For an initial board, complete the optional isolated fixture guidance and
sections 1-4 first. Sections 5-9 are advanced protocol, reconnect, EPR, status,
and evidence checks for qualification or regression work; they are not
prerequisites for opening the GUI at safe 5 V.

## Optional isolated protocol bring-up

Before the load-switch hardware is fitted, the PD protocol and USB console may
be exercised with a deliberately split cable:

- route only CC and ground between the PD source and the board;
- route 5 V VBUS, D+/D-, and ground from the PC to the board;
- keep the source's VBUS physically isolated from the board and any load;
- assert PA6 from the MCU's own 3.3 V rail through a removable jumper,
  preferably with a 1 kΩ to 4.7 kΩ series resistor; do not drive PA6 from a supply
  that can remain on while MCU VDD is off; and
- leave PB12 unloaded only when it is not connected to a switch or MOSFET gate.
  Any fitted gate still requires a hardware pull-down and a defined off state.

This is a protocol fixture, not a power-path test. PA6 will remain high after
the CC cable is removed, so firmware cannot observe a real detach. The policy
engine may eventually time out, reset the PD peripheral, resample CC, and then
recover when the source is reconnected. That fallback can take seconds and is
not evidence of prompt detach handling. Remove the PA6 jumper long enough to
produce a falling edge, or reset the MCU, after every source disconnect and
before changing sources. A high-voltage request may be observed only on the
isolated source side with appropriately rated instruments; the board side must
remain at the PC's 5 V and must not power a load.

This fixture is sufficient to check CDC control, Source_Capabilities discovery,
5 V negotiation, request construction, and basic source interoperability. It
does not verify cable-removal cutoff, PB12 behavior, the load switch, discharge,
or any 48 V board hardware. Complete sections 1 and 2 before connecting source
VBUS or a load to the board.

## 1. Unpowered inspection

- Confirm the MCU part and orientation, exposed-pad connection, rails, and
  decoupling.
- Confirm PC14/PC15 reach CC1/CC2 and PC16/PC17 reach D-/D+ without crossover.
- Measure the independent CC1 and CC2 Rd terminations and record their actual
  resistance to ground.
- Confirm PA6 is driven only by 3.3 V-safe VBUS-present logic.
- Measure PB12 and the actual load-switch gate low with the MCU unpowered and
  held in reset.
- Prove with a meter that debug USB VBUS and PD VBUS cannot backfeed each
  other.

Record the board revision, source model, cable marking, and measurement setup.

## 2. Prove the hardware cutoff at 5 V

Power the PD input from a current-limited 5 V source without relying on
firmware. Scope:

1. connector VBUS;
2. PA6 `VBUS_PRESENT`;
3. PB12 `LOAD_ENABLE`;
4. the actual load-switch gate/output.

Remove the cable repeatedly and save a capture showing that the physical gate
falls from the hardware term even if PB12 is held high. This is the prerequisite
for every later EPR test.

## 3. Verify both boot paths

Build the runtime image:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-safe-5v
```

Enter the factory USB ISP boot mode and flash:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile usb-safe-5v
```

Leave ISP mode and reset normally. The same D+/D- pins should now enumerate as
a CDC COM port rather than the ROM bootloader. Find it with:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\console.ps1 -List
```

If Windows does not enumerate it, capture the Device Manager error and a USB
trace before changing PD code. The CDC layer uses development VID/PID
`1A86:FE0C`.

## 4. Establish and inspect the safe contract

Start the local GUI, connect the CDC device in the browser, then attach an
ordinary PD source:

```powershell
.\examples\browser-usb-pd-control-client\scripts\launch.ps1
```

Expected ordering is:

```text
Attached; PD starts at 5 V
Source caps: kind=SPR count=... EPR=...
PDO1 fixed 5000mV ...
Requesting PDO1 fixed=5000mV EPR=0
Contract ready PDO1 fixed=5000mV EPR=0
Contract ready req=max src=...mA usable=...mA confidence=... limit=... mismatch=...
Source_Info: ...                 # or no line if unsupported
```

Run:

```text
caps
plans
status
source-info
request 5000 max fixed
pdo 1 max
```

Although `pdo N max` now works for every requestable PDO family, use it only
for PDO 1 during this safe procedure. On an adjustable PDO it deliberately
means the offer's maximum voltage; arbitrary endpoints use
`pdo N adjust mV [mA|max]` in the later protected high-voltage phase.

`plans` is the safe exception because it never transmits its calculations. In
this `usb-safe-5v` profile, PDO 1 should plan normally while higher-voltage
offers report `voltage-limit`. Confirm with the analyzer that the command emits
no Request and with the meter that VBUS remains 5 V. Its final
`Contract ready` lines must still identify PDO 1 at fixed 5 V.

Check VBUS independently with a meter. `usable=X mA` is the firmware's answer
to how much current the load may draw for the confirmed contract; it is not a
measurement of current already flowing.

If `caps` reports a noncanonical commercial PPS APDO, it should label that PDO
`compatible` and preserve the raw advertised range/current. Stay at 5 V in this
procedure: confirm the listing and analyzer decode, but do not request the APDO.
Ranges below 3.3 V, above 21 V, inverted, or advertising zero current must be
reported as malformed rather than requestable.

A bounded EPR AVS offer outside 15-48 V, such as AOHI's 5-28 V/140 W
`0xd230328c` or a nominal 15-50 V offer, is likewise reported as `compatible`.
Normal EPR AVS selection exposes only its standards-valid intersection. The
full advertised range requires `epr-avs-nonstandard` or an advanced direct-PDO
adjustment and must only be exercised on an isolated source-side fixture during
bring-up. Above 48 V also requires the explicit `usb-epr-50v` firmware profile;
a normal `usb-epr` build must reject it at its sink limit.

## 5. Exercise PD 3.x SinkTx collision avoidance

This check needs a PD source emulator or analyzer fixture that can control Rp
after an explicit contract. It stays at 5 V. If the available analyzer cannot
control Rp, record this item as pending rather than inferring it from normal
traffic.

1. Establish a PD 3.x 5 V contract and record the active-CC voltage at the
   source's 1.5 A and 3 A Rp levels. Confirm the CH32 1.23 V threshold lies
   between the measured levels with useful margin.
2. Hold the source at 1.5 A Rp (SinkTxNG), issue `source-info`, and verify the
   sink does not send `Get_Source_Info` while that level remains asserted.
3. While the command is pending, have the source send `Get_Sink_Cap`. Verify
   the sink immediately returns `Sink_Capabilities`; a response belonging to
   the source's AMS must not wait behind the gate.
4. Change Rp to 3 A (SinkTxOK). Verify the deferred `Get_Source_Info` is then
   sent once, with no reset or lost command.
5. Repeat a user renegotiation with a PD 2.0 source advertising 1.5 A. It must
   proceed normally because PD 2.0 does not use Rp for SinkTx collision
   avoidance.
6. If the fixture can inject raw frames, send a truncated data frame and a
   short non-final Extended Message chunk. Verify the sink initiates Soft Reset
   recovery and remains responsive; it must not hang or re-enable the load
   during recovery.
7. From an explicit 5 V contract, issue another 5 V request and have the
   source answer `Wait`. Before 100 ms expires, send `Get_Sink_Cap`; verify the
   response is immediate. After SinkRequestTimer, verify the sink sends the
   same requested RDO again, accepts `PS_RDY`, and does not pulse the load gate
   low because the encoded operating point did not change.

Save the CC voltage measurements and analyzer trace. This is the physical
proof corresponding to the scripted `epr_flow`, `pd2_flow`, and
`malformed_frames` regressions.

## 6. Verify reconnect safety

Queue `status`, remove the PD cable at awkward times, and reconnect at least 20
times. For every removal verify:

- the hardware gate falls immediately;
- PB12 falls on the first PA6 low observation;
- no previous command executes after reconnect;
- the next request is PDO 1 at fixed 5 V;
- the prior confirmed current and EPR state are gone.

Also test debug-USB removal/reconnect while PD remains attached. Losing the
console may drop log lines, but it must not change the power contract or load
gate.

## 7. Retrieve the complete EPR list while staying at 5 V

Flash the interactive EPR profile only after the board's entire power path is
rated for the configured EPR limits, or while using the isolated protocol
fixture above where source VBUS has no electrical path to the board or a load:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile usb-epr
```

Use `usb-epr-50v` instead only for a deliberate compatibility test on a path
rated with margin above nominal 50 V. A measured value near 50 V after a source
advertised 48 V may simply be the standard 48 V output at positive tolerance;
the firmware only permits a nominal 50 V request when the raw APDO advertises
it.

With an EPR source and cable, expect the initial 5 V SPR contract, a
`Source_Info` attempt, then:

```text
EPR discovery: enter attempt=1/2; hold 5 V
Source caps: kind=EPR count=... EPR=...
PDO8 ...
PDO9 EPR-AVS ...
Requesting PDO1 fixed=5000mV EPR=1
Contract ready PDO1 fixed=5000mV EPR=1
```

Run `caps`, then `plans`, and preserve both complete outputs. In this profile,
`plans` should produce the maximum legal plan and bounded usable current
for every requestable PDO in the full list. Confirm with an analyzer that it
transmits no Request, and that its final contract is still fixed 5 V on PDO 1.
Without an analyzer, the isolated fixture may instead use the source-side meter
to confirm VBUS remains at 5 V; this is useful functional evidence but does not
verify packet contents or protocol timing.
The development text-console images omit this verbose command to preserve
protocol flash headroom. If EPR entry fails, record the source/cable
combination and PD analyzer trace. A Hard Reset causes a two-second active
receive window and at most one automatic EPR retry. A second failure must
settle at SPR rather than cycling; a manual user request may still retry EPR.

With a controllable source, also request `EPR_Sink_Capabilities` and
`Sink_Capabilities_Extended`. Verify the former is a valid one-PDO, 10-byte
frame and the latter is a 30-byte frame with a 24-byte SKEDB. Byte 22 of that
SKEDB (EPR Operational PDP) must be 240 W for the reference `usb-epr`
profile, matching the EPR Enter data byte, maximum PDP, and local request
ceiling.

## 8. Verify source status and PPS mode

Establish a PPS contract and use **Read PPS status** in the browser. With no
load, a supporting Source should normally report CV plus source-side voltage
and current; unsupported measurement fields are legal and must be displayed as
unavailable without disturbing the contract.

With a current-limited electronic load or other controlled load, increase the
draw through the requested PPS current. If the Source enters current-limit
operation, verify that the GUI changes to CL. A conforming Source should also
send Alert and cause the firmware to read general Status automatically. Use
the optional one-second PPS refresh only to observe live values or to
accommodate a Source that omits that Alert. A refused, deferred, unsupported,
or timed-out query must leave the existing contract active.

The reported current has coarse accuracy and CL is charger regulator state,
not a substitute for board overcurrent protection. Verify that the
application-owned CL indicator is cleared on detach, Hard Reset, and protocol
loss.

## 9. Reporting validation results

For each source/cable pair, save:

- complete SPR and EPR PDO output;
- complete `plans` output plus an analyzer interval showing no transmitted
  Request and unchanged 5 V while it ran;
- any `compatible` PPS line together with the analyzer's raw APDO decode;
- Source_Info output or its absence;
- PPS_Status CV/CL output, the corresponding Alert/general Status sequence,
  and any source that refuses or times out the optional queries;
- the 5 V request RDO and EPR 5 V request/PDO copy from an analyzer;
- EPR Sink Capabilities and Sink Capabilities Extended response frames;
- the SinkTxNG defer/source-response/SinkTxOK resume analyzer trace;
- the `Wait`/source-AMS/exact-retry analyzer trace;
- attach, `PS_RDY`, PB12, and detach scope captures;
- USB enumeration identifiers and COM-port behavior;
- firmware Git commit and build profile.

A useful issue report includes the smallest complete trace, the source and
cable identity, the firmware commit/profile, and the first point where observed
behavior diverges from this procedure. Message-level reproductions can become
scripted host regressions; register- or timing-specific failures need a minimal
analyzer or scope reproduction for the CH32 HAL.
