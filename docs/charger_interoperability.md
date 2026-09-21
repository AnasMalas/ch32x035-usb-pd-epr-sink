# Source interoperability observations

These results document source behavior that materially affects sink policy.
They are not compliance verdicts on a charger model or brand. Raw PDOs and
analyzer traces take precedence over product labels, advertised ratings, and
source-reported telemetry.

## Firmware policy grounded in observed sources

| Observed behavior | Firmware rule |
|---|---|
| Commercial PPS sources advertise bounded but noncanonical ranges, including 3.3/3.6/4.5 V minima and fields above 5 A | Preserve a complete offer only when it remains within 3.3-21 V and has nonzero current. Label it `compatible`, cap every request to 5 A and configured board limits, and never invent an endpoint |
| Some EPR AVS offers extend below 15 V or to nominal 50 V | Normal selection uses only the 15-48 V standards-valid intersection. The complete 5-50 V bounded range requires explicit compatibility selection and a matching board limit |
| Multi-port sources replace their capability table when another port or load changes | Discard an encoded pending request, retain user intent only if the new table can satisfy it, otherwise clear intent and request fixed 5 V |
| Sources may initially advertise only 5 V, interrupt EPR entry with fresh SPR capabilities, or defer Sink traffic with SinkTxNG | Treat new capabilities as current truth, re-plan from them, preserve deferred commands, and bound automatic EPR attempts |
| Optional Source_Info, Status, and PPS_Status fields are frequently unsupported or inconsistent | Display them as source-reported telemetry. Refusal, timeout, or malformed optional data must not invalidate an otherwise healthy contract |
| PD 2.0 uses a four-bit message-type field, so PD 3.x optional-query codes can alias older control messages | Lock the negotiated SOP revision from non-GoodCRC partner traffic, never transmit PD 3.x-only queries on PD 2.0, and report a typed local refusal |
| PPS current-limit behavior varies from sustained regulation to a later Hard Reset | Requested current is a negotiated ceiling, not guaranteed electronic current limiting. Hardware overcurrent protection remains mandatory |
| Poor, extended, or non-EPR-marked cables reduce advertised current, prevent EPR entry, or cause detach/PHY loss | Obey the reduced offer, bound recovery, turn the load off on protocol loss, and advise users to retry with a direct certified cable |
| Some sources transition voltage slowly or answer a Request with `Wait` | Keep the load off for a changed operating point until `PS_RDY`; preserve an unchanged contract during `Wait` and retry only after SinkRequestTimer |
| A Source can send Accept/PS_RDY yet electrically dip VBUS outside the new contract during a downward transition | Treat the message exchange and VBUS waveform as separate evidence. Direct high-to-5 V requests are valid; diagnose the rail with a scope or qualified detector rather than adding a brand-specific default sequence |

## AOHi AOC-C022 case study

Testing identified distinct capability tables for the 140 W and 240 W ports:

- The 140 W port ends at fixed 28 V/5 A and advertises an EPR AVS range through
  28 V with 140 W PDP.
- The 240 W port can advertise fixed 28/36/48 V at 5 A and EPR AVS through
  48 V with 240 W PDP.

The 240 W port also returned a genuine 140 W-limited EPR table in one session:
fixed 36 V/3.88 A, fixed 48 V/2.91 A, and raw AVS `0xd7c0968c` with 140 W
PDP. An explicit 5 A demand was followed by the full 240 W table, which
continued to appear after reconnects and MCU resets. Firmware revisions before
`0fb61b2` declared a 140 W EPR Operational PDP; the reference profile after
that change declares 240 W consistently in EPR Mode Enter and Sink
Capabilities Extended. The available trace does not distinguish whether the
source retained a per-port allocation or reacted to a Capability Mismatch
exchange.

AOHi Source_Info has reported internally inconsistent values such as
`present=240 W`, `maximum=240 W`, and `reported=1 W`. The raw Source PDOs
therefore remain authoritative for request planning.

## Physical validation results

Hardware testing has verified that:

- five-second PPS refresh remains periodic while one-second PPS telemetry is
  active;
- fixed or AVS 48 V can transition to PPS, fixed 20 V, and fixed 5 V without
  the earlier keepalive-related sink reset;
- independently restarting the MCU while CC and the source remain connected
  recovers without the earlier Hard Reset loop; one resynchronizing Hard Reset
  may still occur when the source retains an old contract;
- a source-signaled Hard Reset remains labeled as received while the sink is
  waiting for an EPR keepalive response; and
- desktop Web Serial and Android WebUSB operate with the same compact CDC
  firmware.

## Reproducing and reporting open behavior

One open interoperability question is why the AOHi 240 W port returned a
140 W-limited EPR table before later returning 240 W. A useful reproduction
starts with a completely discharged source, records the outgoing EPR Mode
Enter object and first EPR capability table, and preserves the complete
sequence around any explicit 5 A demand. That evidence can distinguish a
spontaneous source update from a response to Capability Mismatch.

Source-specific issue reports should include a short raw trace, exact
source/port and cable identity, firmware commit and profile, and the first
unexpected state transition. Message-level failures can become host protocol
regressions. CC analog thresholds, VBUS timing, and power-path cutoff require
analyzer or scope evidence.
