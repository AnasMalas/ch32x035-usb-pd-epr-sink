# Repository-wide scripts

Run these PowerShell scripts from the repository root:

| Script | Purpose |
|---|---|
| `bootstrap.ps1` | Activate the pinned Rust toolchain, fetch dependencies, and build the workspace |
| `check.ps1` | Run documentation checks, formatting, Clippy, host tests, all supported firmware-profile builds, browser packaging, and optional Node.js protocol tests |
| `check-docs.ps1` | Validate local Markdown links without building firmware |
| `check-tooling.ps1` | Enforce that GitHub CI delegates to `check.ps1` instead of copying Cargo commands |

These scripts validate the whole repository. Board programming and serial
console tools live with the
[embedded firmware example](../examples/ch32x035-usb-pd-sink-firmware/scripts/);
browser packaging and launching live with the
[browser control-client example](../examples/browser-usb-pd-control-client/scripts/).

`check.ps1` detects the native Rust host target on developer machines and CI
runners. CI runs it on Ubuntu and Windows with `-RequireNode`, so a missing
Node.js installation cannot silently skip the browser protocol tests. The
workflow provisions Node.js 24 explicitly rather than relying on the runner's
mutable default.
