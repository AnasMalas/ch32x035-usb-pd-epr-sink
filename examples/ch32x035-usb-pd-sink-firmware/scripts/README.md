# CH32X035 USB-PD sink firmware example scripts

Run these PowerShell scripts from the repository root:

| Script | Purpose |
|---|---|
| `install-wchisp.ps1` | Install the pinned `wchisp` USB-ISP programmer |
| `build.ps1` | Build one explicit hardware/profile configuration and copy its ELF to `examples/generated-artifacts/` |
| `size.ps1` | Report ELF flash and static-RAM use against the configured CH32X035 limits |
| `flash.ps1` | Program an existing ELF after displaying its path, timestamp, and SHA-256 |
| `program.ps1` | Build and then flash the same selected profile |
| `console.ps1` | List serial ports or open an ASCII pass-through terminal for `dev-text-console` firmware |
| `query-black-box.ps1` | Retrieve and decode the high-level or deep persistent PD black box from its opt-in diagnostic profile |

Profiles are hardware assertions, not merely UI presets. Start with
`usb-safe-5v` on unverified hardware and read the parent
[firmware example README](../README.md) before enabling PPS or EPR.
Every scripted `usb-*` profile selects the public rev0
`ch32x035g8u6` board binding and rejects another `-Chip`. The non-USB
`safe-5v` profile retains the generic PA6/PA7 binding and package selection.
Omitting `-Profile` selects compact `usb-safe-5v`.

`console.ps1` does not decode the compact `usb-control` protocol. Use the
[browser control client](../../browser-usb-pd-control-client/) with the normal
compact-control profiles. Text transport is explicit opt-in, never the
default.
