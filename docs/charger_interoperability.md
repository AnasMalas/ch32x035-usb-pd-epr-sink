# Charger interoperability notes

These notes record source behavior that materially affects product policy.
They are not a compliance verdict on a charger model or brand. Raw PDOs and
analyzer traces take precedence over labels, ratings, and source-reported
telemetry.

## Policy derived from testing

| Observed behavior | Firmware rule |
|---|---|
| Commercial PPS sources advertise bounded but noncanonical ranges, including 3.3/3.6/4.5 V minima and fields above 5 A | Preserve a complete offer only when it remains within 3.3-21 V and has nonzero current. Label it `compatible`, cap every request to 5 A and configured board limits, and never invent an endpoint |
| Some EPR AVS offers extend below 15 V or to nominal 50 V | Normal selection uses only the 15-48 V standards-valid intersection. The complete 5-50 V bounded range requires explicit compatibility selection and a matching board limit |
| Multi-port sources replace their capability table when another port or load changes | Discard an encoded pending request, retain user intent only if the new table can satisfy it, otherwise clear intent and request fixed 5 V |
| Sources may initially advertise only 5 V, interrupt EPR entry with fresh SPR capabilities, or defer Sink traffic with SinkTxNG | Treat new capabilities as current truth, re-plan from them, preserve deferred commands, and bound automatic EPR attempts |
| Optional Source_Info, Status, and PPS_Status fields are frequently unsupported or inconsistent | Display them as source-reported telemetry. Refusal, timeout, or malformed optional data must not invalidate an otherwise healthy contract |
| PPS current-limit behavior varies from sustained regulation to a later Hard Reset | Requested current is a negotiated ceiling, not guaranteed electronic current limiting. Hardware overcurrent protection remains mandatory |
| Poor, extended, or non-EPR-marked cables reduce advertised current, prevent EPR entry, or cause detach/PHY loss | Obey the reduced offer, bound recovery, turn the load off on protocol loss, and advise users to retry with a direct certified cable |
| Some sources transition voltage slowly or answer a Request with `Wait` | Keep the load off for a changed operating point until `PS_RDY`; preserve an unchanged contract during `Wait` and retry only after SinkRequestTimer |

## AOHi AOC-C022 observations

The 140 W port and 240 W port have distinct tables:

- The 140 W port ends at fixed 28 V/5 A and advertises an EPR AVS range through
  28 V with 140 W PDP.
- The 240 W port can advertise fixed 28/36/48 V at 5 A and EPR AVS through
  48 V with 240 W PDP.

The 240 W port has also produced a genuine 140 W-limited EPR table: fixed
36 V/3.88 A, fixed 48 V/2.91 A, and raw AVS `0xd7c0968c` with 140 W PDP. A
later explicit 5 A demand was followed by the full 240 W table, which then
survived reconnects and MCU resets. Earlier firmware had declared a 140 W EPR
Operational PDP; current firmware declares 240 W consistently in EPR Mode
Enter and Sink Capabilities Extended. Whether the charger retained a
per-port allocation or reacted to a capability-mismatch exchange has not yet
been captured.

AOHi Source_Info has reported internally inconsistent values such as
`present=240 W`, `maximum=240 W`, and `reported=1 W`. The raw Source PDOs
therefore remain authoritative for request planning.

## Confirmed regressions

- Five-second PPS refresh remains periodic while one-second PPS telemetry is
  active.
- Fixed/AVS 48 V can transition to PPS, fixed 20 V, and fixed 5 V without the
  previous keepalive-related sink reset.
- Independently restarting the MCU while CC and the source remain connected
  recovers without the previous Hard Reset loop. A single resynchronizing Hard
  Reset may still occur when the source retains an old contract.
- A source-signaled Hard Reset remains labeled as received while the sink is
  waiting for an EPR keepalive response.
- Desktop Web Serial and Android WebUSB use the same compact CDC firmware.

## Remaining evidence

Capture the AOHi 240 W port from a completely discharged source state using
current firmware, including the outgoing EPR Mode Enter object and the first
EPR capability table. If the 140 W table returns, preserve the complete
sequence around an explicit 5 A demand to distinguish a spontaneous update
from a Capability Mismatch response.

Hardware-specific failures should be retained as short raw traces and turned
into host protocol regressions whenever they can be represented as PD
messages. CC analog thresholds, VBUS timing, and power-path cutoff remain
scope/analyzer tests.
