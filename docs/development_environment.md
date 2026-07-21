# Native CH32X035 development environment

## Source of truth

The repository defines the complete build:

- `rust-toolchain.toml` pins Rust, rustfmt, Clippy, and the RISC-V target;
- `Cargo.lock` pins the resolved dependency graph;
- the three `vendor/` crates contain the exact HAL/PD revisions plus local
  repairs;
- `.cargo/config.toml` selects `riscv32imc-unknown-none-elf`;
- `examples/usb-console/build.rs` supplies the CH32 linker arguments.

Docker is unnecessary. VS Code with rust-analyzer is a convenient editor, but
the PowerShell scripts and Cargo files are the reproducible interface.

The project currently pins a dated nightly because that is the compiler
snapshot used to validate this embedded dependency set. Application code does
not intentionally depend on nightly syntax; moving to stable should be done as
a tested toolchain change rather than by following a moving channel.

## One-time Windows setup

1. Install Git.
2. Install Rust through <https://rustup.rs/>.
3. Install Microsoft Visual Studio Build Tools with the C++ workload and a
   Windows SDK. Host-side Rust tests need `link.exe`.
4. Open PowerShell in this repository.
5. Run:

```powershell
.\scripts\bootstrap.ps1 -InstallWchisp
```

This installs the pinned Rust components, downloads locked dependencies,
builds the host tests, and optionally installs the USB-ISP flasher.

## Daily workflow

```powershell
.\scripts\check.ps1
.\scripts\build.ps1 -Profile usb-safe-5v
```

`check.ps1` runs formatting, warning-as-error Clippy for the host policy/test
crates, all desktop tests, and every firmware feature combination used for
bring-up. Cargo uses the repository's standard `target/` directory unless the
caller sets `CARGO_TARGET_DIR`:

```text
<repository>\target
```

Selected ELF files are copied to `artifacts/`.

Useful interactive builds are:

```powershell
.\scripts\build.ps1 -Profile usb-safe-5v
.\scripts\build.ps1 -Profile usb-pps
.\scripts\build.ps1 -Profile usb-epr
.\scripts\build.ps1 -Profile usb-epr-dual-log
```

The dual-log image is useful when the CDC path itself is under test. At the
current library-extraction checkpoint, `usb-epr` uses 61,936 of 63,488 flash
bytes (1,552 free), while dual-log uses 60,896 bytes (2,592 free). Both reserve
7,656 of 20,480 static RAM bytes, leaving 12,824 bytes for stack/runtime.
Dual-log is smaller
because it deliberately omits some detailed interactive responses and now
formats each log line only once, but it remains a bring-up diagnostic rather
than the primary user image.

## USB ISP and runtime CDC

PC16 is D- and PC17 is D+. The same physical pair serves two mutually
exclusive programs:

- the factory ROM USB ISP while the MCU is booted in ISP mode;
- the application CDC-ACM console after normal reset.

Build and flash an explicit profile:

```powershell
.\scripts\build.ps1 -Profile usb-epr
.\scripts\flash.ps1 -Profile usb-epr
```

`flash.ps1` selects an already-built artifact and never rebuilds it. To build
and immediately program the same explicit profile in one command, use:

```powershell
.\scripts\program.ps1 -Profile usb-epr
```

The lower-level `flash.ps1 -Firmware <path>` form remains available when an
exact archived or reviewed ELF must be programmed.

After leaving ISP mode and resetting normally, list and open the COM port:

```powershell
.\scripts\console.ps1 -List
.\scripts\console.ps1 -Port COM7
```

For a scripted smoke test:

```powershell
.\scripts\console.ps1 -Port COM7 -Send status,caps -ListenSeconds 5
```

The baud-rate argument is conventional metadata for USB CDC; there is no UART
baud clock in the data path. The firmware accepts ASCII lines terminated by
LF or CRLF. Type `help` for the complete command grammar.

USB logging is non-blocking with respect to the PD task. A disconnected or
slow host may lose diagnostic lines, but it cannot stop negotiation or the
load supervisor.

## Updating dependencies or Rust

Treat either as a deliberate source change:

1. change the exact version/revision or dated toolchain;
2. update `Cargo.lock` if needed;
3. run `scripts/check.ps1`;
4. compare flash use for `usb-epr` and `usb-epr-dual-log`;
5. commit the lockfile/toolchain change with the code that required it.

Do not point the project at moving Git branches or an unpinned nightly.
