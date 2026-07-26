# Repository-wide scripts

Run these PowerShell scripts from the repository root:

| Script | Purpose |
|---|---|
| `bootstrap.ps1` | Activate the pinned Rust toolchain, fetch dependencies, and build the workspace |
| `check.ps1` | Run documentation checks, formatting, Clippy, host tests, all supported firmware-profile builds, browser packaging, and optional Node.js protocol tests |
| `check-docs.ps1` | Validate local Markdown links without building firmware |

These scripts validate the whole repository. Board programming and serial
console tools live with the
[embedded firmware example](../examples/ch32x035-usb-pd-sink-firmware/scripts/);
browser packaging and launching live with the
[browser control-client example](../examples/browser-usb-pd-control-client/scripts/).
