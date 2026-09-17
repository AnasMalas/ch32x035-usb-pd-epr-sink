# CH32X035 USB PD EPR Sink

> [!CAUTION]
> **Early development snapshot.** APIs, USB identifiers, and hardware
> assumptions may change. This project is not USB-IF certified or ready for
> production use. USB-PD EPR can expose hardware and loads to 48 V and high
> fault energy; use rated hardware, independent protection, and an isolated
> test setup.

A reusable Rust USB Power Delivery sink for the WCH CH32X035, with buildable
reference firmware and a browser control client. It supports fixed SPR/EPR
PDOs, PPS, SPR AVS, and EPR AVS while applying configured source, board,
cable, voltage, current, and power limits.

The crate is under API review and is not published to crates.io.

## Using the project

### Choose your path

| Goal | Start here |
|---|---|
| Build and flash a CH32X035 sink | [Reference firmware: first safe run](examples/ch32x035-usb-pd-sink-firmware/README.md#first-safe-run-on-windows) |
| Add the sink to another firmware | [Library crate guide](crates/pd-sink/README.md) |
| Control an already-flashed board | [Browser control client](examples/browser-usb-pd-control-client/README.md#start) |
| Isolate or persist a real hardware failure | [Hardware debugging and black-box guide](docs/debugging.md) |
| Characterize the experimental CC wake interrupt | [No-load CC wake probe](examples/ch32x035-usbpd-cc-wake-probe/README.md) |
| Understand or change the internals | [Documentation index](docs/README.md) and [contributing guide](CONTRIBUTING.md) |

The hosted [USB-PD Sink Control](https://anasmalas.com/ch32x035-usb-pd-epr-sink/)
page connects to compatible firmware from Android Chrome or desktop
Chrome/Edge. The page does not negotiate USB-PD by itself; it controls a
connected CH32X035 running the compact USB-control firmware.

### What is included

The repository contains the reusable `no_std` crate, package-selectable
CH32X035 firmware source, Web Serial/WebUSB client source, pinned maintained
dependencies, tests, and hardware/integration guidance.

This repository does **not** include a schematic, PCB, BOM, finished reference
board, or production-ready load switch; the firmware example documents only
the required pin and safety contract. It also does not currently provide
tagged binaries, a crates.io package, production USB VID/PID values, USB-IF
certification, or cable e-marker discovery. Cable capability is an
application-configured limit; a source offer is not proof of the cable's
rating.

### Safe first run on Windows

You need compatible CH32X035 hardware, its board-specific method for entering
the factory USB ISP boot mode, Git, Rust through [rustup](https://rustup.rs/),
and Microsoft C++ Build Tools with a Windows SDK.

First read the reference
[hardware interface](examples/ch32x035-usb-pd-sink-firmware/docs/hardware_interface.md).
Then clone the repository and prepare the pinned tools:

```powershell
git clone https://github.com/AnasMalas/ch32x035-usb-pd-epr-sink.git
cd ch32x035-usb-pd-epr-sink
.\scripts\bootstrap.ps1
.\examples\ch32x035-usb-pd-sink-firmware\scripts\install-wchisp.ps1
```

Enter the board's factory USB ISP mode and program the 5 V-only profile:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\program.ps1 -Profile usb-safe-5v
```

Leave ISP mode, reset the MCU normally, and open the browser client:

```powershell
.\examples\browser-usb-pd-control-client\scripts\launch.ps1
```

A successful first pass enumerates a CDC device, reports source capabilities,
and confirms PDO 1 at fixed 5 V. Do not select PPS or EPR until the complete
power path has passed the
[first-board verification](examples/ch32x035-usb-pd-sink-firmware/docs/first_board_verification.md).
The firmware guide documents the higher-voltage profiles and expected
observations.

### Integrate the library

Until a crates.io release is available, pin a reviewed repository commit:

```toml
[dependencies.pd-sink]
package = "ch32x035-usb-pd-epr-sink"
git = "https://github.com/AnasMalas/ch32x035-usb-pd-epr-sink"
rev = "<reviewed commit SHA>"
default-features = false
features = ["ch32x035f8u6"]
```

Your application keeps ownership of `main`, the executor, GPIO assignments,
VBUS-present sensing, the hardware load gate, commands, and diagnostics. The
crate owns PD parsing, request planning, contract state, recovery, and the
optional pin-agnostic CH32X035 PHY adapter. See the
[crate guide](crates/pd-sink/README.md) and
[integration guide](docs/integration.md) for the required adapters and startup
sequence. The crate guide also shows representative linked flash use.

### Supported behavior

The sink decodes all eleven SPR/EPR PDO positions; requests fixed, PPS, SPR
AVS, and EPR AVS supplies; starts every attachment at fixed 5 V; maintains
adjustable/EPR contracts; exposes source telemetry and usable-current limits;
and performs bounded protocol recovery. Explicit compatible ranges cover
advertised PPS endpoints from 3.3 V and bounded EPR AVS through nominal 50 V.
Battery and variable PDOs remain visible but are not requestable.

### Hardware safety boundary

Successful negotiation does not make a board safe for EPR. The connector,
switch, FETs, discharge path, protection, spacing, measurement network, and
load must all be rated for the selected voltage and fault energy.

The effective hardware gate must remain:

```text
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

`VBUS_PRESENT` and `HARDWARE_OK` must disable the power path without working
firmware. Scripted USB profiles use the G8U6 rev0 OPA1/PB10 binding; the
alternate PA6/PA7 binding remains an example for custom applications. Neither
is a library requirement. PD load permission is one application input, not
exclusive ownership of the load: products may let an explicit user latch
control a non-PD USB-A supply when no usable PD session is active. The library
does not require an ADC for that choice. Contract transitions separately offer
manual re-arm, automatic restore after PS_RDY, and deliberately uninterrupted
policies; physical fault and detector cutoffs remain unconditional.

### Documentation by task

| Task | Document |
|---|---|
| Embed the crate | [Integration](docs/integration.md) |
| Build and flash the example | [CH32X035 firmware](examples/ch32x035-usb-pd-sink-firmware/README.md) |
| Design the board interface | [Hardware interface](examples/ch32x035-usb-pd-sink-firmware/docs/hardware_interface.md) |
| Validate a new board | [First-board verification](examples/ch32x035-usb-pd-sink-firmware/docs/first_board_verification.md) |
| Debug timing, resets, or lost messages | [Hardware debugging](docs/debugging.md) |
| Use the GUI | [Browser control client](examples/browser-usb-pd-control-client/README.md) |
| Implement another host | [Compact control protocol](docs/control_protocol.md) |
| Understand the layers | [Architecture](docs/architecture.md) |
| Compare real sources | [Interoperability](docs/charger_interoperability.md) |

## Developing and maintaining

Repository checks, ownership rules, dependency policy, and evidence
expectations live in [CONTRIBUTING.md](CONTRIBUTING.md). Current host and
physical coverage is summarized in [architecture](docs/architecture.md) and
[interoperability](docs/charger_interoperability.md); it does not qualify a
downstream product.

Project-owned code is offered under either the MIT License or Apache License
2.0. Maintained upstream descendants retain their original licensing and
provenance. See [third-party notices](THIRD_PARTY_NOTICES.md) and each
`vendor/*/UPSTREAM.md`.

USB and USB-C are used descriptively. This project is not endorsed or
certified by USB-IF and does not redistribute USB-IF specification documents.
