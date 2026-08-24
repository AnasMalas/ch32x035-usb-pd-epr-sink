# CH32X035G8U6 rev0 validation

This procedure is for the public rev0 board only. The `rev0-validation`
firmware is deliberately fixed at 5 V. It validates the current public PD
library branch without enabling PPS or EPR and keeps the active-high PB10 load
request low until every software permission is present.

The scripted `usb-safe-5v`, `usb-pps`, and `usb-epr*` profiles use the same
G8U6 OPA1/PB10 board binding. Compact USB control is the default; only the
explicit `rev0-validation` and `usb-epr-text` profiles use ASCII transport.

## Exact rev0 implementation

| Function | Rev0 connection | Firmware configuration |
|---|---|---|
| MCU supply | VDD = 5 V | CH32X035G8U6 |
| USB-PD CC1 / CC2 | PC14 / PC15 | integrated USB-PD PHY |
| divided VBUS | 100 kohm / 10.2 kohm node tied to PC3 and PB4 | both GPIO cells analog/input; OPA1 positive input PB4 (`PSEL1=10`) |
| threshold reference | PB6, approximately 0.42 V | OPA1 negative input PB6 (`NSEL1=001`) |
| OPA1 output | PB5 | OPA mode selects PB5 (`MODE1=1`); PB5 GPIO driver disabled in analog-input mode |
| detector observation | PB1 on the shared PB1/PB5 package pad | floating digital input with EXTI1; never an output or pulled-up input |
| load request | PB10 | active high; initialized low before OPA or PD setup |
| diagnostic USB | PC16 / PC17 | compact CDC by default; ASCII only for explicit text profiles |

The current [WCH CH32X035 datasheet v2.2](https://www.wch-ic.com/downloads/CH32X035DS0_PDF.html)
shows PB1, PB5, OPA1 output `O1O1`, and `T1BK` together on G8U6 QFN28 pin 16
(section 2.1, PDF page 13 / printed page 12). Its pin-definition note 5
(PDF page 20 / printed page 19) says PB1 and PB5 are internally shorted and
must not both be configured as outputs.

The current [WCH CH32X035 reference manual v1.9](https://www.wch-ic.com/downloads/CH32X035RM_PDF.html)
maps OPA1 PB4(+), PB6(-), and PB5 output in sections 17.2.1 and 17.3.3
(pages 208 and 211-212). GPIO table 8-8 (page 64) requires the OPA inputs and
output GPIO cell to use analog-input mode. The firmware follows that mapping,
then observes the externally visible shared package pad through PB1 as a
floating EXTI input.

This G8U6 bonded-pad loopback is a strong inference from WCH's documented
bond and mux topology, not a hidden internal signal route named by WCH. OPA1
is the sole intended driver. Do not configure PB1 as a GPIO/alternate-function
output, do not enable a PB5 GPIO/alternate-function output, and do not let any
external circuit drive QFN28 pin 16. The pad can be probed because the OPA
waveform is present on the package pin. No PA3-to-PB1/PB5 jumper is used.
Actual PB1 input/EXTI observation of this bonded pad remains a required rev0
bench check because WCH does not name this particular loopback application.
PB1 has no documented controlled pull-down. Register readback can reject an
invalid OPA configuration, but it cannot prove that OPA1 physically drives or
that PB1 observes the bonded pad. An unavailable physical path could therefore
leave PB1 floating. Before the first `output-on`, prove raw low with VBUS
absent and exercise both detector states. Any unattended or production use
also requires a suitably weak external pad pull-down, validated against OPA1
drive and PB1 input thresholds, plus an independent default-off load gate.

## Detector meaning and threshold

The detector is active high:

- high means initialized OPA1 reports divided VBUS above the board's PB6
  reference, after 100 ms of uninterrupted high qualification;
- published low means VBUS is below that threshold or detector initialization
  is unavailable; the first observed low cancels attach and drives PB10 low
  with no intentional deassertion debounce;
- high is only a coarse minimum-VBUS predicate. It is not a VBUS measurement
  and does not prove that VBUS equals the negotiated contract.

With the nominal values, the common rising/falling crossing is:

```text
0.42 V * (100 kohm + 10.2 kohm) / 10.2 kohm = 4.538 V
```

OPA1 has no documented or configurable hysteresis, so firmware must not rely
on any. At VDD = 5 V and the datasheet's 0.5 V common-mode test condition, WCH
specifies OPA input offset as typically +/-5 mV and at most +/-13 mV (table
3-24, PDF pages 36-37 / printed pages 35-36). Referred through this divider,
those values indicate approximately +/-54 mV and +/-140 mV at VBUS. The
actual crossing is near 0.42 V common mode, so do not treat the latter as a
guaranteed bound there. It also excludes PB6 reference accuracy, resistor
tolerance, leakage, noise, and board drop. Therefore 4.538 V is nominal, not
a guaranteed limit; measure the rising and falling crossings on each hardware
population and derive the full tolerance before using this detector outside
validation.

The software provides no intentional low-side debounce. A maximum physical
OPA-low-to-PB10-low latency cannot be established by source inspection; scope
it on rev0 and record the worst case. This software path is not a substitute
for a hardware-default-off load gate.

## Why TIM1 brake is not used

Reference-manual section 17.3.1 (page 210) states that the dedicated
OPA-to-TIM brake signal can only be active high and requires the timer brake
polarity to be high. Here OPA1 is high when VBUS is valid. Enabling that route
would therefore brake at valid VBUS and release when VBUS falls: the reverse
of a falling-VBUS cutoff. Following reset, `OPA_CFGR1` is `0x0080`, so
`BKIN_EN1=0` and `POLL_LOCK=1` (register table and section 17.3.1, pages
209-210). This example never writes `POLL_KEY`, leaving that reversed-polarity
route disabled and locked. Unexpected brake-route or OPA configuration
readback is treated as detector-initialization failure and PB10 stays low.

PB1/EXTI1 supplies the validation firmware's software observation instead.
An external-pad-to-TIM1-brake experiment could use a different timer input
path and polarity, but WCH does not guarantee the simultaneous OPA-output and
timer-input use needed here. This profile does not enable or depend on it.

## Build, flash, and test

Disconnect the product load first and monitor PB10 or the downstream switch
gate. Build the profile from the repository root; the script selects G8U6:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile rev0-validation
```

Put the MCU in factory USB ISP mode and flash the already-built artifact:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile rev0-validation
```

If PD VBUS is the board's only supply, enter ISP and flash through that normal
powered connection; do not add a second supply merely for this procedure. If
a board variant has separate debug and PD connectors, first prove that their
VBUS rails cannot backfeed one another.

Leave ISP mode and power-cycle normally, then list and open the application
CDC port:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\console.ps1 -List
.\examples\ch32x035-usb-pd-sink-firmware\scripts\console.ps1 -Port COM7
```

Use a current-limited 5 V source for the first pass.

1. At reset and with no VBUS, verify PB10 and the physical load gate are low.
   The console must report detector raw low; abort if it reports high.
   If rev0 cannot be powered with PD VBUS absent, this fail-low check is not
   possible; do not run a load-enable test without adding the qualified weak
   pad pull-down or using a fixture that establishes the same fail-safe state.
2. Attach 5 V. Expect an OPA raw-high line, a 100 ms qualification line,
   `attach published`, `Attached; PD starts at 5 V`, source capabilities, and
   a confirmed 5 V session outcome.
3. Confirm PB10 remains low even after the PD permission becomes true. Send
   `status` and `caps` to capture the session, then send `output-on`. PB10 may
   rise only when raw and qualified VBUS and the user latch are true, with the
   active PD policy either permitted or explicitly unmanaged. Send
   `output-off` before changing bench wiring.
   Repeat with a current-limited USB-A-to-C source that provides no PD
   messages: after the partner is treated as PD-unmanaged, `output-on` must be
   able to raise PB10 using the user latch and the qualified VBUS predicate;
   passive PD retries must not pulse it low. This confirms control policy, not
   the source voltage or available current.
4. For a detector-only threshold sweep, disconnect the ordinary PD source and
   CC pins completely. Power VDD through a verified isolated arrangement and
   drive only PD VBUS from a current-limited bench supply. Never parallel a
   bench supply with a USB-C source. Sweep around 4.54 V and record the actual
   OPA/PB1 crossings and any chatter; no analog hysteresis is assumed.
5. For a live-contract cutoff test, use a programmable PD source emulator or
   a rated series cutoff fixture designed to ramp/remove VBUS. Do not backdrive
   an ordinary charger. Re-enable with a fresh `output-on`, then scope OPA
   output/QFN28 pin 16, PB10, and the switch gate while VBUS drops below the
   measured crossing. PB10 must fall on the first observed detector low.
6. Detector low, a real managed-PD permission loss, detach, and Hard Reset
   clear the rev0 user latch. An ordinary configured voltage transition
   preserves the latch while PB10 is inhibited, then restores PB10 after
   PS_RDY. Issue a fresh `output-on` after safety cutoff; then send
   `output-off` before the next reconnect. Repeat attach/detach,
   cable reversal, failed negotiation, and reconnect tests. Save the console
   transcript with the firmware SHA-256, source/cable identity, measured
   thresholds, and cutoff latency.

## Compact PPS and EPR regression

Run high-voltage tests only after the fixed-5 V procedure passes and the
complete upstream path is already known to tolerate the requested voltage.
Keep PB10 and the product load off throughout the first protocol regression.
The GUI's confirmed contract proves protocol agreement, not the actual VBUS
voltage; use at least a suitably rated meter if electrical voltage evidence is
required.

Test the compact PPS artifact first:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-pps
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile usb-pps
.\examples\browser-usb-pd-control-client\scripts\launch.ps1
```

Capture `device`, `caps`, and `plans`, then request only values advertised by
the connected source. Exercise a low, middle, and high PPS value, request
`pps-status` after each, and finish by requesting fixed 5 V. Do not send
`output-on` at the higher voltages.

Then test standard compact EPR, still with PB10 off:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile usb-epr
```

Capture the automatic EPR entry outcome, `epr-caps`, and `plans` before making
an EPR request. Exercise only the source's advertised low, middle, and high
standard EPR points, never above nominal 48 V with this profile. Finish with
`exit-epr` and confirm fixed 5 V. `usb-epr-50v` remains a separate nonstandard
opt-in and is not part of this regression.

At nominal 21 V, 48 V, and the standard 50.4 V positive limit, the divider
node is approximately 1.94 V, 4.44 V, and 4.66 V respectively. Qualify
component tolerances and pin-rail margin before the upper EPR test. The
roughly 4.54 V detector remains only a minimum-VBUS predicate: it cannot tell
48 V from an erroneous 9 V. Bridging one lower 5.1 kohm resistor also does not
force detector-low at high V; perform that synthetic detector test at 5 V.

## Rev0 facts still requiring bench evidence

- PB1 digital readback and EXTI edge behavior while OPA1 drives the documented
  shared PB1/PB5 package pad;
- actual PB6 reference voltage, divider tolerances, rising/falling crossings,
  noise sensitivity, and any incidental hysteresis;
- worst-case OPA-low-to-PB10-low and physical-gate cutoff latency;
- an external PB10 pull-down and a physical gate that remain off during reset,
  ISP, an unpowered MCU, or crashed firmware; and
- absence of an external or alternate-function driver on QFN28 pin 16.

The unmodified rev0 pad has no documented internal pull-down. Until a weak
external fail-safe pull-down is fitted and qualified, a broken OPA-to-PB1
observation path is not guaranteed to publish low; the manual raw-low test and
explicit user latch limit this validation image but do not make it production
fail-safe.

Until those checks pass, this is validation firmware, not evidence that the
rev0 power path is production-safe or that VBUS matches a PD contract.
