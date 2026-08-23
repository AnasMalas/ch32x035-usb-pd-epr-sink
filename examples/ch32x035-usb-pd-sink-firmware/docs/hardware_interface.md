# CH32X035 hardware/firmware interface

This is the schematic contract assumed by the firmware. The GPIO names are
common to every supported CH32X035 package; physical package pins differ.
Confirm the package drawing against the current
[WCH datasheet](https://www.wch-ic.com/downloads/CH32X035DS0_PDF.html) before
laying out a board.

## Reserved pins

| MCU pin | R8T6 | C8T6 | G8U6 | G8R6 | F8U6 | F7P6 | Direction/function |
|---|---:|---:|---:|---:|---:|---:|---|
| PC14 | 54 | 38 | 28 | 4 | 19 | 2 | USB-PD analog, CC1 |
| PC15 | 55 | 39 | 1 | 5 | 20 | 3 | USB-PD analog, CC2 |
| PC16 | 44 | 32 | 26 | 2 | 17 | 17 | USB D- for factory ISP and runtime CDC |
| PC17 | 45 | 33 | 27 | 3 | 18 | 18 | USB D+ for factory ISP and runtime CDC |
| PC18 | 46 | 34 | 25 | 28 | 14 | 19 | LinkE SDI data |
| PC19 | 49 | 37 | 24 | 1 | 16 | 20 | LinkE clock |
| PA6 | 22 | 16 | 11 | 13 | 8 | 12 | input, active-high `VBUS_PRESENT` from external 3.3 V logic |
| PA7 | 23 | 17 | 12 | 17 | 9 | 13 | output, active-high `LOAD_ENABLE` firmware request |

The table describes the normal profiles. The public G8U6 rev0 board has a
separate, fixed-5 V [`rev0-validation` profile](rev0_validation.md): OPA1
compares PB4 against PB6, PB1 reads the PB5 OPA output through the package's
documented PB1/PB5 bond, and PB10 is the active-high load request. Those exact
pins remain example-local and are not requirements of the reusable library.

PA6 and PA7 were selected partly to leave a hardware-assisted cutoff route
available: PA6 exposes OPA/comparator-related input and TIM1 break alternate
functions, while PA7 exposes a TIM1 complementary-output alternate function.
The reference firmware intentionally configures PA6 as an EXTI GPIO and PA7
as a GPIO output today. Do not claim or depend on the analog-to-break path
until its remap, polarity, startup state, and fault latency have been verified
on hardware.

CC1/CC2 and D+/D- are independent interfaces. USB PD negotiation uses only CC;
the PD stack has no USB 2 dependency. PC16/PC17 may therefore go to a separate
USB connector used for flashing, serial commands, and diagnostics.

Populate an independent passive Rd from each CC pin to ground. The nominal
Type-C sink value is 5.1 kohm; component tolerance and routing leakage must
keep both terminations within the current USB Type-C specification limits.
The CH32X035 does not supply these pull-downs internally.

After a PD 3.x explicit contract, the source uses its active-CC Rp level for
collision avoidance: 1.5 A means SinkTxNG and 3 A means SinkTxOK. Before the
first message of a sink-initiated AMS, firmware temporarily selects the
CH32X035 1.23 V comparator threshold, samples the active CC pin after 2 us,
then restores the normal 0.66 V receive threshold. This test is not used for
PD 1.0/2.0 partners. Its analog margin depends on the real Rd network and must
be checked on the assembled board.

Route D+/D- as a short differential pair and add appropriate USB ESD
protection. The application enables the CH32X035 internal 1.5 kohm D+ pull-up
for full-speed operation. Its development descriptor declares a bus-powered
100 mA device. If the debug connector can power the board, keep its total
pre-configuration load within that declaration; if the PD input can also
power the board, prevent either connector from back-powering the other.

## Non-negotiable power-path behavior

PA6 must never connect directly to VBUS. Feed it from a 48 V-capable
comparator, supervisor, or isolated power-good circuit with a 3.3 V-safe
output, hysteresis, and a defined low state whenever the cable, comparator
supply, or MCU supply is absent.

Do not tie PA6 permanently high in a finished, separately powered controller.
The present CH32 driver samples CC while resetting the PD peripheral; it does
not provide an independent continuous CC-detach event. With PA6 high, cable
removal is discovered only indirectly after policy timeouts, and PA7 can
remain asserted in the meantime. PA6 may be omitted only if it is replaced by
a separately verified continuous port/power supervisor and an equivalent
hardware-default-off load gate.

PA7 requires an external pull-down so the load defaults off during reset,
flashing, an unpowered MCU, or crashed firmware. Implement the effective
load-switch enable in hardware as:

```text
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

`VBUS_PRESENT` and `HARDWARE_OK` must reach the load switch without firmware.
The PA6 interrupt is a second cutoff path and cancels blocked PD I/O; it is not
the primary anti-spark guarantee.

The switch, FETs, connector, protection, discharge path, measurement network,
spacing, and passives must be rated for the worst supported EPR condition and
fault energy. Standard operation is nominally capped at 48 V, which can reach
50.4 V at the standard positive tolerance. The opt-in `usb-epr-50v`
compatibility profile can request a genuinely advertised nominal 50 V; because
that extension is non-standard, do not assume the standard 48 V tolerance is
its worst case. Give the complete path explicit margin above 50 V and qualify
the actual sources used. A successful firmware negotiation does not establish
those ratings.

## Firmware behavior

- PA7 is driven low before attach debounce.
- PA6 must remain high for 100 ms before attachment is accepted.
- The first PA6 falling edge drives PA7 low without detach debounce.
- Every PD receive/transmit operation races against cable removal.
- A new/changing request disables the load until `Accept` and `PS_RDY`.
- Hard reset, detach, protocol loss, and unknown state force load-off.
- Hard Reset recovery actively receives for a fixed two-second window and
  does not require a PA6 edge; PA6 held high is therefore usable for the
  isolated protocol fixture.
- Reconnect clears capabilities, EPR mode, user intent, queued commands, and
  confirmed current before requesting 5 V.
- An EPR-capable image may enter EPR to read capabilities, but its automatic
  discovery request remains fixed 5 V.

## Bring-up checks before high voltage

1. With no firmware and with the MCU held in reset, verify the actual load gate
   is off.
2. Measure both external CC-to-ground Rd terminations before connecting a
   source.
3. At 5 V, scope PA6, PA7, and the physical switch gate while repeatedly
   removing the cable. Confirm the hardware term cuts the gate without MCU
   activity.
4. Verify PA7 cannot override low `VBUS_PRESENT` or `HARDWARE_OK`.
5. Flash `usb-safe-5v`; confirm PA7 rises only after `PS_RDY`.
6. Verify USB ISP, normal boot CDC enumeration, and LinkE do not back-power or
   contend with the PD input.
7. Exercise SinkTxNG/SinkTxOK using the procedure in
   `first_board_verification.md` when a controllable source fixture is
   available.
8. Repeat detach/reconnect and hard-reset tests before enabling PPS/EPR.
9. Test low, middle, and high points of each advertised PPS/AVS range, then
   fixed 48 V on a current-limited protected bench setup.
10. If the product intentionally supports a nominal 50 V compatibility offer,
    repeat the protected test with `usb-epr-50v` only after establishing the
    source's actual maximum and the complete path's voltage margin.

The detailed checklist and expected diagnostic transcript are in
`first_board_verification.md`.
