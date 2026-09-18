# Contributing

This project welcomes focused bug reports and changes that improve the
reusable CH32X035 sink, its maintained protocol/HAL descendants, or the
reference applications. It is an early, safety-sensitive project: a passing
build is necessary, but changes that affect power-path behavior also need
clear protocol or hardware evidence.

## Repository boundaries

| Area | Owns |
|---|---|
| `crates/pd-sink/` | Reusable PDO models, request planning, contracts, typed commands/events, compact framing, and optional pin-agnostic CH32 integration |
| `vendor/usbpd*` | Maintained PD messages, protocol state, policy engine, counters, and timers |
| `vendor/ch32-hal/` | Maintained CH32 clocks, interrupts, USB-PD PHY, and USBFS primitives |
| `tests/protocol/` | Scripted wire-level tests through the real policy engine |
| `examples/ch32x035-usb-pd-sink-firmware/` | Executor, example pins, USB/SDI transports, text formatting, board profiles, flashing, and physical verification |
| `examples/ch32x035-usbpd-cc-wake-probe/` | No-load hardware characterization of the CH32X035 PD-port wake interrupt |
| `examples/browser-usb-pd-control-client/` | Desktop Web Serial and Android WebUSB controls and host-side formatting |
| `scripts/` | Checks that apply to the whole repository |

The core crate must not acquire an application entry point, executor, fixed
GPIO assignment, USB endpoint, GUI, logger, LED policy, or product-specific
setting. Put those concerns in the relevant example or downstream
application.

## Set up and check a change

The Windows bootstrap installs no system software; it activates the pinned
Rust toolchain, fetches locked dependencies, and builds the workspace after
Rust and Microsoft C++ Build Tools are already installed:

```powershell
.\scripts\bootstrap.ps1
```

Run the maintained repository check before submitting a change:

```powershell
.\scripts\check.ps1 -RequireNode
```

It validates documentation links, formatting, warning-free Clippy, desktop
unit and policy-engine tests, every supported firmware profile, browser
packaging, and browser protocol tests. GitHub Actions invokes this same script
on Ubuntu and Windows. CI workflow files must delegate to it rather than copy
package or feature lists.

The root bootstrap and the example build/programming scripts are currently
Windows-tested PowerShell workflows. `check.ps1` itself is supported on
PowerShell 7 on both Windows and Linux.

## Change the right layer

- Add general request, limit, contract, safety-state, command, or typed-event
  behavior to `crates/pd-sink`.
- Add PD wire messages, counters, timers, and policy-engine behavior to the
  maintained `vendor/usbpd*` descendants.
- Add CH32 register, interrupt, USB-PD PHY, or USBFS behavior to
  `vendor/ch32-hal`.
- Keep example pins, serial formatting, browser behavior, and board profiles
  in their example directories.
- Add host regressions for decisions and wire behavior. Do not claim that a
  host test proves CH32 analog timing, register behavior, or a physical load
  cutoff.

Document reusable invariants in `docs/`; keep firmware-only hardware and
bring-up details under the firmware example. Write public documentation for a
reader who was not present during development: state the observed evidence,
the resulting policy, and what remains unknown.

## Protocol and hardware evidence

A useful protocol bug report or pull request records:

- the firmware commit and profile;
- board revision and relevant power-path limits;
- source model and exact port;
- cable identity and whether it is EPR-rated;
- the smallest complete raw trace around the first divergence; and
- analyzer, scope, or meter evidence when the claim depends on CC voltage,
  VBUS timing, or load-gate behavior.

Every detach, Hard Reset, protocol-loss, or invalid-state path must leave the
firmware load request off. PD current negotiation is not a substitute for
hardware overcurrent or fault protection.

## Dependencies and toolchains

Vendored descendants are intentional because this project carries local PD
and CH32 repairs. For a dependency or Rust-toolchain change:

1. update the exact version, revision, or dated toolchain;
2. update `Cargo.lock` and the relevant `vendor/*/UPSTREAM.md`;
3. preserve upstream licensing and provenance;
4. run the full repository check;
5. compare flash use for `usb-epr`, `usb-epr-deep-black-box`, and `usb-epr-text`; and
6. commit the lockfile or toolchain change with the code that requires it.

Do not point a reproducible build at a moving Git branch or unpinned nightly.
Project-owned code is dual-licensed under MIT or Apache-2.0; retained upstream
code remains under its recorded license.

## Pull requests

Keep changes focused enough that a regression can be bisected. In the
description, identify the affected layer, explain the first failing behavior,
summarize the evidence, and list remaining physical verification explicitly.
Generated ELF, HTML, map, and capture files do not belong in commits; reviewed
release artifacts belong in a tagged GitHub Release.
