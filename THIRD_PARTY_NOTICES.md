# Third-party notices

This project contains maintained descendants of the following open-source
projects. Their licenses permit modification and redistribution; attribution
and the applicable license texts are retained here and in the vendored source
directories.

## ch32-hal

- Project: <https://github.com/ch32-rs/ch32-hal>
- Base commit: `71a2985804962e248efc09e3bb4f21d4459b50e8`
- Copyright: Copyright (c) 2023 Andelf
- License: MIT OR Apache-2.0
- Local record: [`vendor/ch32-hal/UPSTREAM.md`](vendor/ch32-hal/UPSTREAM.md)
- License texts: [`licenses/ch32-hal-MIT.txt`](licenses/ch32-hal-MIT.txt) and
  [`licenses/ch32-hal-APACHE-2.0.txt`](licenses/ch32-hal-APACHE-2.0.txt)

## usbpd and usbpd-traits

- Project: <https://github.com/elagil/usbpd>
- Base commit: `3c9d3953a156793c52685be56dc70732abc84b7d`
- Upstream author metadata: Adrian Figueroa
- License declared in both package manifests: MIT
- Local records: [`vendor/usbpd/UPSTREAM.md`](vendor/usbpd/UPSTREAM.md) and
  [`vendor/usbpd-traits/UPSTREAM.md`](vendor/usbpd-traits/UPSTREAM.md)
- License text: [`licenses/usbpd-MIT.txt`](licenses/usbpd-MIT.txt)

The pinned upstream repository did not contain a repository-level license file.
Before a public release, this project will seek upstream clarification and will
continue preserving the manifest declaration, author metadata, provenance, and
MIT terms in the meantime.

## usb-pd-rs

The upstream `usbpd` project states that its message parsing inherits code from
[`fmckeogh/usb-pd-rs`](https://github.com/fmckeogh/usb-pd-rs).

- Copyright: Copyright (c) 2023 Ferdia McKeogh
- License: MIT
- License text: [`licenses/usb-pd-rs-MIT.txt`](licenses/usb-pd-rs-MIT.txt)

## Registry and Git dependencies

The remaining Cargo dependencies are consumed without local source
modification. `Cargo.lock` records their exact resolved versions for firmware
and reference-firmware builds. A generated complete dependency/license report
remains a release gate before tagged binary distribution.
