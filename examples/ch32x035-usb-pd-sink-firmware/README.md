# CH32X035 USB-PD sink firmware example

This example binds the reusable sink library to concrete CH32X035 board
profiles: VBUS sensing, load-enable policy, USB CDC or LinkE diagnostics,
build profiles, and bounded restart behavior. The scripted `usb-*` profiles
target the public G8U6 rev0 board's OPA1/PB10 wiring. The non-USB `safe-5v`
profile and custom Cargo builds retain the alternate PA6/PA7 binding. None of
those pins or transports are requirements of the core crate.

Run the commands below from the repository root. Read the
[hardware interface](docs/hardware_interface.md) and
[first-board verification](docs/first_board_verification.md) before enabling
PPS or EPR on a new board. If behavior changes with a cable, Source, USB host,
or application workload, start with the repository-wide
[hardware debugging guide](../../docs/debugging.md) before adding synchronous
logging or changing PD timers.

For the public CH32X035G8U6 rev0 board, use the dedicated
[rev0 validation procedure](docs/rev0_validation.md). It is fixed at 5 V,
uses the internal OPA1 detector, and keeps PB10 off until an explicit
`output-on` command is permitted by the active power policy and VBUS state.
The same command can control a qualified non-PD USB-A supply after the PD
session is classified as unmanaged; passive PD retries do not own PB10.

## First safe run on Windows

This repository supplies firmware source and a pin-level hardware contract,
not a reference schematic or finished board. You need to know how the rev0
board enters the CH32X035 factory USB ISP boot mode and how its OPA1/PB10
safety signals are implemented.

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
| `rev0-validation` | ASCII USB console, PB10 default off | G8U6 rev0, fixed 5 V |
| `usb-safe-5v` | compact USB control, PB10 default off | G8U6 rev0, fixed 5 V |
| `usb-pps` | compact USB control, PB10 default off | G8U6 rev0, PPS through 21 V |
| `usb-epr` | compact USB control, PB10 default off | G8U6 rev0, standard EPR through 48 V |
| `usb-epr-uninterrupted` | compact USB control, PB10 held across contract transitions | G8U6 rev0, standard EPR through 48 V |
| `usb-epr-diagnostic` | compatibility alias of `usb-epr` | G8U6 rev0, standard EPR through 48 V |
| `usb-epr-black-box` | compact USB control plus persistent high-level incident history | G8U6 rev0 with 5 V VDD, standard EPR through 48 V |
| `usb-epr-deep-black-box` | compact USB control plus a persistent 16-record numeric Hard Reset trace | G8U6 rev0 with 5 V VDD, standard EPR through 48 V |
| `usb-epr-50v` | compact USB control, PB10 default off | G8U6 rev0, explicit non-standard 50 V compatibility |
| `usb-epr-text` | explicit ASCII troubleshooting console, PB10 default off | G8U6 rev0, standard EPR through 48 V |

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

The scripted `usb-*` profiles and `rev0-validation` select
`ch32x035g8u6` automatically and reject another explicit `-Chip`, because the
OPA1/PB1/PB5 bonded-pad detector is package-specific:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-safe-5v
```

Omitting `-Profile`, or building the firmware package with Cargo defaults,
selects this compact `usb-safe-5v` G8U6 rev0 configuration.

The scripted USB profiles retain the full GUI capability/status surface by
enabling `rich-telemetry`. A product that needs the same compact control,
contract, safety, PPS, and EPR behavior but can omit raw capability tables,
plan previews, and live source-status presentation may leave that feature out
of a direct Cargo build. The reference browser reads the advertised device
flag, disables the unavailable controls, and does not poll them. At this
revision that choice reduces the complete G8U6 EPR reference by 1,272 flash
bytes with no static-RAM change.

The `safe-5v` PA6/PA7 profile and direct Cargo builds still support
`ch32x035c8t6`, `ch32x035f7p6`, `ch32x035f8u6`, `ch32x035g8r6`,
`ch32x035g8u6`, and `ch32x035r8t6`. CH32X033 is not supported because it has
USB but not the integrated USB-PD peripheral.

The CH32X035F7P6 has only a 48 KiB application region. At this revision the
the console-free fixed-5 V integration links at 39,504 bytes. The current
compact USB/GUI EPR feature set is 49,264 bytes and therefore exceeds F7P6's
48 KiB application region by 112 bytes; the larger development text-console
EPR feature set also does not fit. Treat F7P6 as a size-constrained manual
integration target, not as a scripted rev0 profile. The linker will fail
rather than emit an oversized image.

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
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile rev0-validation
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-pps
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-uninterrupted
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-diagnostic
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-black-box
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-deep-black-box
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-50v
.\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile usb-epr-text
```

`rev0-validation` is the human-readable, fixed-5 V public-board bring-up
image; follow its [dedicated procedure](docs/rev0_validation.md).
`usb-epr` is the normal compact binary control image. All scripted USB
profiles keep the application-owned output latch off until a permitted
`output-on` command arrives and include typed Hard Reset causes. Normal
profiles select the safe automatic-restore transition policy: PB10 is
inhibited for an unsafe contract change, the user latch is preserved, and
PB10 returns only after PS_RDY. The opt-in `usb-epr-uninterrupted` profile
keeps PB10 asserted across contract transitions. It does not guarantee a
regulated or dip-free output while the Source changes VBUS, and must be used
only when the complete downstream path and load tolerate every requested
voltage. Detector loss, detach, Hard Reset, protocol loss, and terminal faults
remain unconditional shutoffs.
`usb-epr-diagnostic` remains as a compatibility alias. The black-box profiles
retain compact protocol v1 and add an independent three-byte `BB<page>` query
understood by `scripts/query-black-box.ps1`. `usb-epr-black-box` stores only
unusual high-level session events; `usb-epr-deep-black-box` also captures the
most recent formatter-free numeric PD events in a 16-record ring, appends the
final cause summary when available, and then freezes that same ring. Ordinary
detach is not an incident. Both profiles reserve the
final two 256-byte flash pages, so their linker-owned application limit is
62,976 bytes rather than 63,488. They erase only the inactive page at boot,
then the 4.0 V PVD handler first clears and latches off active-high PB10 and programs the
already-erased page from SRAM. This backend is restricted to the validated
rev0 board's 5 V VDD and hold-up behavior; do not copy its threshold into a
3.3 V design. See the
[persistent black-box guide](../../docs/debugging.md#persistent-reference-black-box).

The opt-in
`usb-epr-50v` image raises only the configured sink ceiling; normal AVS
selection remains within 15-48 V. `usb-epr-text` retains the direct ASCII
console only for explicit troubleshooting; compact control remains the
default. Build output reports the current flash and static-RAM
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

### Read a persistent black box

After a suspected fault and reboot, reconnect the diagnostic image and run:

```powershell
.\examples\ch32x035-usb-pd-sink-firmware\scripts\query-black-box.ps1
```

Pass `-Port COM7` when automatic port discovery is ambiguous. The script
decodes the restored flash generation or current RAM snapshot, including Hard
Reset causes and, for the deep profile, PD headers, GoodCRC/retry state,
protocol errors, and EPR keepalive phases. Retrieval shares the existing CDC
sender queue but does not change compact control protocol v1.

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
The RISC-V target also enables `mir-opt-level=3` in `.cargo/config.toml`; this
size optimization is tied to the pinned compiler and must be remeasured during
any toolchain update.

### Updating dependencies or Rust

Treat either as a deliberate source change:

1. change the exact version/revision or dated toolchain;
2. update `Cargo.lock` if needed;
3. run `scripts/check.ps1`;
4. compare flash use for `usb-epr`, `usb-epr-50v`, and `usb-epr-text`;
5. commit the lockfile/toolchain change with the code that required it.

Do not point the project at moving Git branches or an unpinned nightly.
