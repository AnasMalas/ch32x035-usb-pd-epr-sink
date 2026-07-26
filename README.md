# CH32X035 USB PD EPR Sink

> [!CAUTION]
> **Early development snapshot.** APIs, USB identifiers, behavior, and
> hardware assumptions may change without notice. This project is not USB-IF
> certified or ready for production use. USB-PD EPR can expose hardware and
> loads to 48 V and high fault energy; use appropriately rated hardware,
> independent protection, and an isolated test setup.

A reusable Rust USB Power Delivery sink for the WCH CH32X035, plus a buildable
reference firmware and browser control application. It supports fixed SPR/EPR
PDOs, PPS, SPR AVS, and EPR AVS while reporting the current permitted by the
source offer and configured board, cable, and power limits.

The crate is still under API review and is not published to crates.io.

## Supported behavior

- Decode and validate all eleven SPR/EPR Source PDO positions.
- Plan fixed, PPS, SPR AVS, and EPR AVS requests.
- Preserve explicitly selected, source-advertised PPS endpoints from 3.3 V and
  bounded EPR AVS compatibility ranges through nominal 50 V.
- Enter and exit EPR mode, retrieve chunked EPR capabilities, and maintain PPS
  and EPR contracts.
- Start every attachment at fixed 5 V; high voltage requires an explicit
  application or user request.
- Report requested, source-advertised, and usable current, including the
  limiting reason and confidence.
- Query Source_Info, Source Status, PPS_Status, and follow Source Alerts.
- Recover from SinkTxNG, Soft Reset, Hard Reset, detach, malformed traffic,
  source-owned AMS traffic, and bounded retry failures.

Battery and variable source PDOs remain visible when advertised but are not
requestable because they are outside this project's intended sink use case.

## Safety boundary

Successful negotiation does not make a board safe for EPR. The connector,
switch, FETs, discharge path, protection, spacing, measurement network, and
load must all be rated for the selected voltage and fault energy.

The reference firmware assumes a 3.3 V-safe `VBUS_PRESENT` input and a
firmware `LOAD_ENABLE` output. The effective hardware gate must remain:

```text
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

`VBUS_PRESENT` and `HARDWARE_OK` must disable the power path without working
firmware. Read [the reference hardware interface](examples/reference-firmware/docs/hardware_interface.md)
before adapting the example.

## Project boundaries

| Area | Role | Owns |
|---|---|---|
| `crates/pd-sink/` | Reusable core | PDO models, request planning, contracts, sink policy integration, typed commands/events, compact control framing, and optional pin-agnostic CH32 PHY integration |
| `vendor/usbpd*` | Maintained core dependency | PD protocol, policy engine, messages, counters, and timers |
| `vendor/ch32-hal/` | Maintained hardware dependency | CH32 clocks, interrupts, USB-PD PHY, and USBFS CDC primitives |
| `tests/protocol/` | Core verification | Scripted wire-level policy-engine tests |
| `examples/reference-firmware/` | Reference device application | Executor, PA6/PB12 policy, USB CDC ownership, SDI/text formatting, board profiles, flashing, and board verification |
| `examples/pd-control/` | Reference host application | Desktop Web Serial and Android WebUSB client for the compact control protocol |
| `examples/artifacts/` | Generated example output | Ignored local ELF, HTML, map, and capture staging; only its README is tracked |
| `scripts/` | Repository tooling | Workspace bootstrap, checks, and documentation-link validation |

The core emits typed observations and load requests; it does not own a logger,
USB endpoint, GUI, executor, LED, or board pin. SDI output and the development
text console are properties of the reference application.

## Quick start

On Windows, install Rust with [rustup](https://rustup.rs/) and Microsoft C++
Build Tools, then run:

```powershell
.\scripts\bootstrap.ps1
.\examples\reference-firmware\scripts\install-wchisp.ps1
.\scripts\check.ps1
.\examples\reference-firmware\scripts\program.ps1 -Profile usb-epr
.\examples\pd-control\scripts\launch.ps1
```

The reference `program.ps1` builds and flashes the same selected profile.
`flash.ps1` only flashes an existing artifact and prints its timestamp and
SHA-256 first.

The reference profiles are:

| Profile | Diagnostics/control | Hardware policy |
|---|---|---|
| `safe-5v` | LinkE SDI | fixed 5 V |
| `usb-safe-5v` | compact USB control | fixed 5 V |
| `usb-pps` | compact USB control | PPS through 21 V |
| `usb-epr` | compact USB control | standard EPR through 48 V |
| `usb-epr-50v` | compact USB control | explicit non-standard 50 V compatibility |
| `usb-epr-text` | ASCII USB console | standard EPR through 48 V |

These are board assertions, not software-only unlocks. Do not select a profile
whose voltage, current, or power exceeds the complete hardware path.

The normal USB profiles use the compact binary protocol. The browser turns
typed events into readable capabilities, contracts, telemetry, and diagnostic
lines. `usb-epr-text` is retained for direct serial-terminal bring-up.

The packaged desktop GUI is a single offline HTML file:

```powershell
.\examples\pd-control\scripts\launch.ps1
```

Desktop Chrome/Edge use Web Serial. Android Chrome uses WebUSB and therefore
needs the HTTPS copy at
[anasmalas.com/ch32x035-usb-pd-epr-sink](https://anasmalas.com/ch32x035-usb-pd-epr-sink/).
Both transports use the same CDC-ACM firmware.

## Documentation

- [Integration](docs/integration.md) — application authors consuming the core
  crate.
- [Architecture](docs/architecture.md) — maintainers changing layer
  boundaries or protocol behavior.
- [Reference hardware interface](examples/reference-firmware/docs/hardware_interface.md)
  — schematic and safety requirements for the example.
- [Reference hardware validation](examples/reference-firmware/docs/first_board_verification.md)
  — repeatable
  bring-up and regression procedure.
- [Control protocol](docs/control_protocol.md) — host and GUI implementers.
- [Reference firmware](examples/reference-firmware/README.md) — contributors
  building and flashing the device example.
- [Browser control example](examples/pd-control/README.md) — desktop and
  Android host-client users.
- [Interoperability](docs/charger_interoperability.md) — measured source
  behavior and the conservative policy used in response.

## Validation status and scope

Host tests cover request encoding, real policy-engine flows, malformed
messages, PPS refresh, EPR keepalive, reset origin, and lifecycle recovery.
Physical CH32X035 testing has completed SPR, PPS, fixed 28/36/48 V, EPR AVS,
long-running PPS refresh with telemetry, independent MCU restart recovery,
desktop Web Serial, and Android WebUSB.

These results do not qualify a downstream product. Before deployment, an
integrator must verify its hardware load gate and real source-VBUS sensing,
perform protected loaded EPR tests, assign appropriate USB identifiers, and
repeat interoperability testing against its supported sources and cables.

## Origins and licensing

Project-owned code is offered under either the MIT License or Apache License
2.0. Maintained upstream descendants retain their original licensing and
provenance. See [third-party notices](THIRD_PARTY_NOTICES.md) and each
`vendor/*/UPSTREAM.md`.

USB and USB-C are used descriptively. This project is not endorsed or
certified by USB-IF and does not redistribute USB-IF specification documents.
