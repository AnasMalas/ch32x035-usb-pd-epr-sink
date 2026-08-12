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

Profiles are hardware assertions, not merely UI presets. Start with
`usb-safe-5v` on unverified hardware and read the parent
[firmware example README](../README.md) before enabling PPS or EPR.
`build.ps1`, `flash.ps1`, and `program.ps1` accept `-Chip`; it defaults to
`ch32x035f8u6` and keeps artifacts for different packages separate.

`console.ps1` does not decode the compact `usb-control` protocol. Use the
[browser control client](../../browser-usb-pd-control-client/) with the normal
compact-control profiles.
