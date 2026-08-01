# CH32X035 USB-PD sink firmware example

This example binds the reusable sink library to a concrete CH32X035F8U6
application: PA6 source-VBUS sensing, PB12 load-enable policy, USB CDC or LinkE
diagnostics, build profiles, and bounded restart behavior. None of those pins
or transports are requirements of the core crate.

Run the commands below from the repository root. Read the
[hardware interface](docs/hardware_interface.md) and
[first-board verification](docs/first_board_verification.md) before enabling
PPS or EPR on a new board.

## First safe run on Windows

This repository supplies firmware source and a pin-level hardware contract,
not a reference schematic or finished board. You need to know how your board
enters the CH32X035 factory USB ISP boot mode and how its PA6/PB12 safety
signals are implemented.

1. Install Git and clone this repository.
2. Install Rust through <https://rustup.rs/>.
3. Install Microsoft Visual Studio Build Tools with the C++ workload and a
   Windows SDK. Host-side Rust tests need `link.exe`.
4. Open PowerShell in this repository.
5. Install the pinned dependencies and USB-ISP flasher:

```powershell
.\scripts\bootstrap.ps1
.\examples\ch32x035-usb-pd-sink-firmware\scripts\install-wchisp.ps1
```

6. Read the [hardware interface](docs/hardware_interface.md), prove the
   default-off load gate, and enter your board's factory USB ISP mode.
7. Build and program the 5 V-only compact-control profile:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\program.ps1 -Profile usb-safe-5v
```

8. Leave ISP mode, reset normally, and launch the browser client:

```powershell
.\examples\browser-usb-pd-control-client\scripts\launch.ps1
```

Select the CH32 CDC device. A successful first pass reports source
capabilities and confirms PDO 1 at fixed 5 V. Continue with
[first-board verification](docs/first_board_verification.md); do not flash a
PPS or EPR profile merely because the safe firmware negotiates correctly.

## Firmware profiles

| Profile | Diagnostics/control | Declared hardware policy |
|---|---|---|
| `safe-5v` | LinkE SDI | fixed 5 V |
| `usb-safe-5v` | compact USB control | fixed 5 V |
| `usb-pps` | compact USB control | PPS through 21 V |
| `usb-epr` | compact USB control | standard EPR through 48 V |
| `usb-epr-50v` | compact USB control | explicit non-standard 50 V compatibility |
| `usb-epr-text` | ASCII USB console | standard EPR through 48 V |

These profiles are board assertions, not software-only unlocks. Do not select
one whose voltage, current, or power exceeds the complete connector,
protection, switch, measurement, and load path.

The compact reference firmware disables initialization of the CH32X035's
general DMA1 controller because none of its application tasks use DMA1.
USBFS endpoint DMA and USB-PD packet DMA are separate, peripheral-local
engines and remain enabled. An application that adds ADC, touch sampling, or
another general-DMA user should set `hal::Config::enable_dma` to `true` (the
HAL default).

## Build profiles

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-safe-5v
```

Cargo uses the repository's standard `target/` directory unless the caller
sets `CARGO_TARGET_DIR`:

```text
<repository>\target
```

Selected ELF files are copied to `examples/generated-artifacts/`.

Useful interactive builds are:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-safe-5v
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-pps
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-50v
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-text
```

`usb-epr` is the normal compact binary control image. The opt-in
`usb-epr-50v` image raises only the configured sink ceiling; normal AVS
selection remains within 15-48 V. `usb-epr-text` retains the direct ASCII
console for bring-up. Build output reports the current flash and static-RAM
usage; the [crate guide](../../crates/pd-sink/README.md#flash-use)
shows representative linked costs for common integration paths.

## USB ISP and runtime CDC

PC16 is D- and PC17 is D+. The same physical pair serves two mutually
exclusive programs:

- the factory ROM USB ISP while the MCU is booted in ISP mode;
- the application CDC-ACM console after normal reset.

Build and flash an explicit profile:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile usb-epr
```

For an explicitly qualified 50 V compatibility path, substitute
`usb-epr-50v` in both commands. The normal profile is deliberately capped at
48 V.

`flash.ps1` selects an already-built artifact and never rebuilds it. To build
and immediately program the same explicit profile in one command, use:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\program.ps1 -Profile usb-epr
```

The lower-level `flash.ps1 -Firmware <path>` form remains available when an
exact archived or reviewed ELF must be programmed.

After leaving ISP mode and resetting normally, launch the browser GUI. It
packages one offline HTML file, speaks the compact protocol directly, and
translates device events locally:

```powershell
.\examples\browser-usb-pd-control-client\scripts\launch.ps1
```

The launcher exits after opening
`examples\generated-artifacts\usb-pd-control.html`; it does not leave a
localhost server running. Use `-NoBrowser` to package only or `-Output` to
choose a distributable destination.

For direct ASCII terminal work, build and flash `usb-epr-text`, then list and
open the COM port:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-text
.\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile usb-epr-text
.\examples\ch32x035-usb-pd-sink-firmware\scripts\console.ps1 -List
.\examples\ch32x035-usb-pd-sink-firmware\scripts\console.ps1 -Port COM7
```

For a scripted smoke test:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\console.ps1 -Port COM7 -Send status,caps -ListenSeconds 5
```

The baud-rate argument is conventional metadata for USB CDC; there is no UART
baud clock in the data path. The development text firmware accepts ASCII lines
terminated by LF or CRLF. Type `help` for the complete command grammar.
`console.ps1` is deliberately an ASCII pass-through terminal: it neither
decodes nor translates the compact `usb-control` protocol. Use the browser GUI
with the standard compact-control profiles.

`usb-control` and `dev-text-console` are mutually exclusive USB application
protocols. The text profile keeps human-readable formatting and a larger queue
in firmware, so it omits the flash-heavy `plans` preview and the compact Hard
Reset cause field. LinkE SDI is a separate diagnostic output, not part of
either USB protocol.

The text console reports a successful identical maintenance Request as one
`Contract refresh confirmed` line. It cannot disable PPS maintenance because
stopping those Requests would allow the Source to drop the PPS contract.

USB logging is non-blocking with respect to the PD task. A disconnected or
slow host may lose diagnostic lines, but it cannot stop negotiation or the
load supervisor.

## Development and maintenance

### Repository checks

Before committing a change, run:

```powershell
.\scripts\check.ps1 -RequireNode
```

It checks documentation, formatting, warning-free Clippy, desktop policy
tests, every reference firmware profile, browser packaging, and browser
protocol tests. See [`CONTRIBUTING.md`](../../CONTRIBUTING.md) for repository
boundaries and evidence requirements.

### Reproducible build inputs

The repository defines the complete build:

- `rust-toolchain.toml` pins Rust, rustfmt, Clippy, and the RISC-V target;
- `Cargo.lock` pins the resolved dependency graph;
- the three `vendor/` crates contain the exact HAL/PD revisions plus local
  repairs;
- `.cargo/config.toml` selects `riscv32imc-unknown-none-elf`;
- `examples/ch32x035-usb-pd-sink-firmware/build.rs` supplies the CH32 linker
  arguments.

Docker is unnecessary. VS Code with rust-analyzer is a convenient editor, but
the PowerShell scripts and Cargo files are the reproducible interface.

The project currently pins a dated nightly because that is the compiler
snapshot used to validate this embedded dependency set. Application code does
not intentionally depend on nightly syntax; moving to stable should be done as
a tested toolchain change rather than by following a moving channel.

### Updating dependencies or Rust

Treat either as a deliberate source change:

1. change the exact version/revision or dated toolchain;
2. update `Cargo.lock` if needed;
3. run `scripts/check.ps1`;
4. compare flash use for `usb-epr`, `usb-epr-50v`, and `usb-epr-text`;
5. commit the lockfile/toolchain change with the code that required it.

Do not point the project at moving Git branches or an unpinned nightly.
