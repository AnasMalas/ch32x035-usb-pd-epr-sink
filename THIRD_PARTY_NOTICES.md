# Third-party notices

`LICENSE-MIT` and `LICENSE-APACHE` cover project-owned code. This project also
contains maintained descendants of the following open-source projects.
Upstream license texts remain with vendored source when provided; the
`licenses/` directory contains terms and attribution records for inherited
code whose retained upstream tree did not include a standalone license text.

## ch32-hal

- Project: <https://github.com/ch32-rs/ch32-hal>
- Base commit: `71a2985804962e248efc09e3bb4f21d4459b50e8`
- Copyright: Copyright (c) 2023 Andelf
- License: MIT OR Apache-2.0
- Local record: [`vendor/ch32-hal/UPSTREAM.md`](vendor/ch32-hal/UPSTREAM.md)
- License texts: [`vendor/ch32-hal/LICENSE-MIT`](vendor/ch32-hal/LICENSE-MIT)
  and [`vendor/ch32-hal/LICENSE-APACHE`](vendor/ch32-hal/LICENSE-APACHE)

## usbpd and usbpd-traits

- Project: <https://github.com/elagil/usbpd>
- Base commit: `3c9d3953a156793c52685be56dc70732abc84b7d`
- Upstream author metadata: Adrian Figueroa
- License declared in both package manifests: MIT
- Local records: [`vendor/usbpd/UPSTREAM.md`](vendor/usbpd/UPSTREAM.md) and
  [`vendor/usbpd-traits/UPSTREAM.md`](vendor/usbpd-traits/UPSTREAM.md)
- License text: [`licenses/usbpd-MIT.txt`](licenses/usbpd-MIT.txt)

The pinned upstream repository did not contain a repository-level license file.
This repository therefore preserves the package-manifest declaration, author
metadata, provenance, and MIT terms. Upstream clarification remains a gate for
a tagged binary release.

## usb-pd-rs

The upstream `usbpd` project states that its message parsing inherits code from
[`fmckeogh/usb-pd-rs`](https://github.com/fmckeogh/usb-pd-rs).

- Copyright: Copyright (c) 2023 Ferdia McKeogh
- License: MIT
- License text: [`licenses/usb-pd-rs-MIT.txt`](licenses/usb-pd-rs-MIT.txt)

## Registry and Git dependencies

The remaining Cargo dependencies are consumed without local source
modification. `Cargo.lock` records their exact resolved versions for firmware
and reference firmware builds. A tagged binary release should also include a
generated complete dependency and license report.
