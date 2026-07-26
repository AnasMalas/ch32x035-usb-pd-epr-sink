# USB control transports

The reference firmware exposes one of two mutually exclusive USB CDC
application protocols. USB CDC's configured baud rate is metadata only; both
use the MCU's USBFS peripheral on PC16/PC17.

## `usb-control`

`usb-control` is the normal product-facing feature. It sends versioned binary
frames no larger than 63 bytes, so a complete frame fits in one full-speed USB
packet. Every frame contains:

```text
"PD" | version | kind | sequence | payload length | payload | CRC-8
```

Payload integers are little-endian. The CRC-8 uses polynomial `0x07` over the
bytes beginning with `version` and ending with the payload. Sequence zero is
used for unsolicited events; command responses copy the command sequence.

Commands are typed forms of the stable command surface: identity,
capabilities, plans, Source_Info, EPR entry/capability retrieval/exit, status,
general Source Status, PPS_Status, help, voltage requests, and direct-PDO
requests. Events cover command status, device limits and UID, lifecycle and
reset state, raw Source PDO words, request plans and contracts, controller
errors, Source_Info, Source Alert, general/PPS status and optional-query
failure, request disposition, EPR state, and integration errors.
An accepted Request that exactly matches the active contract is emitted as a
compact contract-refresh plan stage. This lets a host show PPS maintenance
without repeating the full negotiation transcript.

The firmware does not format those values as text. In particular, a complete
11-object EPR capability list is one event containing eleven raw 32-bit PDOs.
The browser validates and decodes those words, then produces the readable
table, cards, and diagnostic lines. This saves both formatting code in flash
and text queues in RAM.

The reusable framing and data model live in
[`crates/pd-sink/src/control.rs`](../crates/pd-sink/src/control.rs). The USB CDC
adapter is intentionally reference-firmware code in
[`examples/usb-console/src/control_transport.rs`](../examples/usb-console/src/control_transport.rs),
so an application can carry the same protocol over another packet transport.
The library also emits typed `SinkEvent` values. Human-readable line formatting,
the command prompt, USB ownership, and the executor are part of the reference
example, not hidden behavior owned by `pd-sink`.

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

The flash-constrained development text-console profile retains the older
direction/recovery line. The normal compact-control profile carries the cause,
and the browser renders its descriptive name.

## `dev-text-console`

`dev-text-console` retains the line-oriented ASCII protocol used during early
bring-up. It is convenient with an ordinary serial terminal and includes the
human-readable format strings and a larger log queue in firmware. The old
`usb-console` feature is a compatibility alias for this feature.
The flash-heavy `plans` preview is available in the normal compact-control
image, not in text-console profiles.

Select only one application protocol in a firmware image. `usb-epr-dual-log`
combines the development text console with SDI logging and is intended for
diagnosis only.

## Browser selection

The browser keeps USB CDC as the physical interface. Desktop Chromium uses the
operating system's serial port through Web Serial. Android Chromium, which has
no Web Serial API, claims the same CDC-ACM interfaces through WebUSB and uses
their bulk endpoints directly. WebUSB is available only in a secure context,
so the packaged local `file:` page is desktop-only and the Android copy must
be served over HTTPS. This is a host-side transport choice; it does not require
a second firmware protocol or prevent other serial software from using the
device after the browser disconnects.

USB PD Control waits for device output before sending a command. A valid
`PD`, version-1 frame selects `usb-control`; a complete printable line selects
`dev-text-console`. For compact control, the browser encodes every UI or raw
console command as a typed command frame and translates all returned events.
For development text, it sends and parses the existing ASCII directly.
The text firmware emits raw PDO and Status words where that saves target flash;
the browser applies the same decoder used for compact binary events, so both
transports present the same table and telemetry.

The production `usb-safe-5v`, `usb-pps`, `usb-epr`, and opt-in
`usb-epr-50v` profiles use `usb-control`. Use `usb-epr-text` for a
conventional serial terminal.

The text console reports a successful identical maintenance Request as one
`Contract refresh confirmed` line. It does not expose a command that disables
PPS maintenance, because stopping those Requests would allow the Source to
drop the PPS contract.

## Reading diagnostic values

- `RDO=0x...` is the exact 32-bit USB PD Request Data Object transmitted over
  CC. It is not a console command.
- `raw=0x...` on a PDO line is the exact 32-bit source advertisement before
  decoding.
- `RAW Serial` or `RAW WebUSB` is exact USB CDC host-transport data. It is not
  a PD packet capture, and browser read chunks need not match USB packet
  boundaries. The GUI retains the latest 1,000 chunks even while Raw stream is
  off and reveals them when the toggle is enabled.
- `Contract refresh confirmed` is a completed PPS maintenance Request. EPR
  keepalive exchanges are much more frequent and intentionally remain silent;
  a failed exchange is visible as a Hard Reset with its cause.
