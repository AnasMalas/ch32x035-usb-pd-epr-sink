# Compact control protocol

The reusable crate defines versioned command/event framing independently of
USB, serial ports, or a user interface. It does not enumerate a device or
connect to a GUI. An application chooses a packet transport and a host chooses
how to present the typed values.

Frames are no larger than 63 bytes, so a complete frame can fit in one
full-speed USB packet. Every frame contains:

```text
"PD" | version | kind | sequence | payload length | payload | CRC-8
```

Payload integers are little-endian. The CRC-8 uses polynomial `0x07` over the
bytes beginning with `version` and ending with the payload. Sequence zero is
used for unsolicited events; command responses copy the command sequence.

Commands are typed forms of the stable command surface: identity,
capabilities, plans, Source_Info, EPR entry/capability retrieval/exit, status,
general Source Status, PPS_Status, help, voltage requests, and direct-PDO
requests. Protocol v1 also assigns new, backward-compatible command IDs for
`output-on` and `output-off`; existing IDs are unchanged. Those commands alter
only the application load latch, not the PD contract or EPR state. Events cover
command status, device limits and UID, lifecycle and
reset state, raw Source PDO words, request plans and contracts, controller
errors, Source_Info, Source Alert, general/PPS status and optional-query
failure, request disposition, EPR state, and integration errors.
An accepted Request that exactly matches the active contract is emitted as a
compact contract-refresh plan stage. This lets a host show PPS maintenance
without repeating the full negotiation transcript.

The compact event does not format those values as text. In particular, a
complete 11-object EPR capability list is one event containing eleven raw
32-bit PDOs. A host validates and decodes those words for its own presentation.
This lets embedded applications avoid carrying text formatting and queues.

## Optional rich telemetry

The default-on `rich-telemetry` crate feature owns the host-facing raw
capability list, plan previews, Source_Info, Alert, general Source Status, PPS
Status, and their explicit query commands. A size-constrained application can
disable that feature while retaining compact USB control, voltage and PDO
requests, EPR control, contract/transition reports, output commands, and all
lifecycle, fault, recovery, and Hard Reset events. The PD engine still parses
Source_Info and status messages and still follows Alerts as required; only
their optional host presentation is removed.

Protocol v1 does not change. Without `rich-telemetry`, command IDs `0x02`,
`0x03`, `0x04`, `0x0a`, and `0x0b` return the existing Unsupported command
status, and the corresponding rich event kinds are not emitted. Bit 2
(`RICH_TELEMETRY_SUPPORTED`) in `DeviceInfo.flags` tells a host whether to show
those controls or poll PPS Status. The reference browser accepts older
firmware without this bit and treats it as rich-capable for compatibility.

The reusable framing and data model live in
[`crates/pd-sink/src/control.rs`](../crates/pd-sink/src/control.rs). The library
also emits typed `SinkEvent` values. Human-readable line formatting, the
command prompt, USB ownership, and the executor are application concerns, not
hidden behavior owned by `pd-sink`.

The Hard Reset event contains direction, recovery interval, and a stable cause
category such as `source-signaled`, `power-transition-failure`, or
`epr-keepalive-failed`. Current payloads append the cause byte to the original
five-byte direction/interval payload; hosts should accept the legacy form and
label its cause as unspecified. The cause codes are:

| Code | Cause |
|---:|---|
| 0 | source-signaled |
| 1 | invalid-source-capabilities |
| 2 | soft-reset-failed |
| 3 | source-capabilities-timeout |
| 4 | request-response-timeout |
| 5 | power-transition-failure |
| 6 | epr-capabilities-timeout |
| 7 | epr-protocol-error |
| 8 | epr-keepalive-failed |

The `StatusQueryFailed` event has a two-byte payload. Query codes are 0 general
Source Status, 1 PPS Status, and 2 Source Info. Failure codes are 0 not
supported by the partner, 1 rejected, 2 deferred, 3 timed out, and 4
unavailable in the negotiated PD revision. Code 4 is a local refusal: the
query was not placed on the wire and the existing contract remains valid.

Reference implementations are kept with the examples:

- [`examples/ch32x035-usb-pd-sink-firmware/src/control_transport.rs`](../examples/ch32x035-usb-pd-sink-firmware/src/control_transport.rs)
  carries compact frames over USB CDC.
- [`examples/browser-usb-pd-control-client/`](../examples/browser-usb-pd-control-client/)
  is a browser host that
  translates typed frames into controls, tables, and diagnostic lines.
- The reference firmware's optional ASCII console is a separate application
  protocol documented in its
  [`README`](../examples/ch32x035-usb-pd-sink-firmware/README.md).
