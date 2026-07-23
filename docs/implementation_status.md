# USB-PD implementation status

Status at Git branch `feat/pd-stack`, 2026-07-14. “Byte-tested” means a desktop
test drives the real vendored policy/protocol engine and inspects transmitted
PD frames. It does not replace first-board analyzer evidence.

| Requirement | Software status | Evidence before hardware | Remaining gate |
|---|---|---|---|
| Safe attachment | Complete | Every new session clears intent and first requests fixed 5 V | Scope VBUS/load sequencing |
| Full source discovery | Complete | SPR list plus chunked EPR list, positions 1–11 retained and printed; invalid mandatory 5 V offers are rejected | Test several sources/cables |
| Direct PDO selection | Complete | `pdo N max` plans every requestable family across positions 1-11; adjustable PDOs select their advertised maximum; fixed 48 V uses a two-object EPR Request | Analyzer acceptance and real VBUS |
| PPS | Complete in software | Arbitrary in-range voltage at 20 mV resolution; 19.4 V and 5 s refresh byte-tested; bounded compatibility mode retains real 3.6/4.5 V or >5 A proprietary advertisements while requests remain capped to 5 A | Verify a noncanonical APDO and long-run refresh with an analyzer |
| SPR AVS | Complete | First-class vendor/product decoding plus a real policy-engine 19.4 V Request at 100 mV resolution | Find/test a PD 3.2 source offering it |
| EPR AVS | Complete | Standard 15-48 V range, 100 mV resolution, and 5 A/PDP cap; bounded source extensions from 5-50 V are labeled compatible and require explicit opt-in plus a matching sink limit; AOHI `0xd230328c`, 19.4 V, and a nominal 50 V two-object request are byte-tested | Exercise the AOHI standard and compatibility ranges on isolated VBUS |
| Battery/variable source PDOs | Deliberately unsupported | Valid offers remain position-preserving and visible in capability reports, but the product planner never creates a battery/variable RDO | None for this product |
| Available-current report | Complete in policy | Caps by offer, protocol, Source_Info present PDP, board, configured cable, sink power, and user demand; confidence is printed | Configure real board/cable limits; compare with source behavior |
| EPR lifecycle | Complete in software | Enter/Ack/Success, chunk retrieval, fixed 48 V, AVS, keepalive, SPR pre-exit contract, Exit, reset-origin reporting, a PA6-independent 2 s Hard Reset recovery window, and a two-attempt automatic EPR circuit breaker | Analyzer timing and failure cases |
| Source capability queries | Complete | Source_Info, Sink Capabilities, EPR Sink Capabilities, and 24-byte SKEDB response exercised | Verify with real source requests |
| PD 3.x collision avoidance | Complete in software | CH32 1.23 V active-CC sampler; pending AMS/source response/PD 2.0 bypass traces | Measure CC levels and timing |
| Deferred requests and PPS maintenance | Complete in software | `Wait` retains DPM intent across source AMS traffic, retries after SinkRequestTimer, and identical periodic PPS refreshes keep the load enabled | Verify retry delay and uninterrupted load on a controllable source |
| Malformed wire traffic | Complete for supported receive paths | Exact object-count/frame-length checks, bounded chunk assembly, reserved-value fallbacks, Soft Reset recovery, and invalid-5-V Hard Reset regressions | Inject malformed frames with an analyzer |
| Charger interoperability | Reviewed pre-hardware | Noncanonical Anker/AOHi/Baseus PPS offers, Anker/UGREEN EPR capture form, optional-query refusal or silence, slow transition, reconnect, malformed negotiation, and dynamic-capability paths are mapped in `charger_interoperability.md` and regression-tested | Run the matrix on named physical sources |
| Disconnect safety | Firmware complete when PA6 is real; hardware required | First bad PA6 sample cancels PD I/O, invalidates contract, and requests load-off; Hard Reset recovery remains functional with PA6 held high, while ordinary physical detach detection still requires PA6 or an equivalent supervisor | Build the independent hardware gate and scope it |
| User interface | Complete in software | Native USB CDC commands plus LinkE logging; a local Web Serial browser interface provides contract/capability summaries, explicit EPR entry, fixed/PPS/AVS control, per-MCU persistent 5/28/48/50 V GUI ceilings constrained by the firmware's reported limit, direct PDO control, and optional raw session logs | Re-test the new identity and persistent-setting flow on CH32 silicon, then replace development USB identifiers before distribution |

## Deliberate non-goals for this application

- General outbound multi-chunk Extended Messages are not implemented. Current
  transmitted payloads are at most 24 bytes and fit one legal chunk; larger
  payloads fail safely instead of producing a malformed frame.
- The sink advertises one fixed-5-V operating requirement in EPR Sink
  Capabilities. A future product that needs to advertise an EPR APDO itself
  would require the larger model and outbound chunk state machine.
- `PPS_Status`, USB-IF certification machinery, alternate modes, role swaps,
  VDMs, and a complete formal Type-C state machine are outside the present
  “works as a configurable sink” path.
- The USB/SKEDB identifiers remain WCH development identifiers and must be
  replaced before distribution.

## Hardware completion criteria

Follow `first_board_verification.md`. The implementation should not be called
hardware-proven until USB ISP and CDC work, source/EPR capability discovery is
captured, SinkTxNG deferral is measured, PPS and AVS 19.4 V are verified, fixed
48 V is verified on a protected setup, and cable removal independently drops
the physical load gate.
