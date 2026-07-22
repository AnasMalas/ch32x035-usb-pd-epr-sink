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
help, voltage requests, and direct-PDO requests. Events cover command status,
device limits and UID, lifecycle and reset state, raw Source PDO words, request
plans and contracts, controller errors, Source_Info, request disposition, EPR
state, and integration errors.

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

## `dev-text-console`

`dev-text-console` retains the line-oriented ASCII protocol used during early
bring-up. It is convenient with an ordinary serial terminal and includes the
human-readable format strings and a larger log queue in firmware. The old
`usb-console` feature is a compatibility alias for this feature.

Select only one application protocol in a firmware image. `usb-epr-dual-log`
combines the development text console with SDI logging and is intended for
diagnosis only.

## Browser selection

USB PD Control waits for device output before sending a command. A valid
`PD`, version-1 frame selects `usb-control`; a complete printable line selects
`dev-text-console`. For compact control, the browser encodes every UI or raw
console command as a typed command frame and translates all returned events.
For development text, it sends and parses the existing ASCII directly.

The production `usb-safe-5v`, `usb-pps`, and `usb-epr` profiles use
`usb-control`. Use `usb-epr-text` for a conventional serial terminal.
