# CH32X035 USB PD EPR Sink

> [!CAUTION]
> **Early development snapshot.** This repository is temporarily public to
> support browser and Android hardware testing. APIs, USB identifiers,
> behavior, and hardware assumptions may change without notice. It is not
> USB-IF certified or ready for production use. USB-PD EPR can expose hardware
> and loads to 48 V and high fault energy; use appropriately rated hardware,
> independent protection, and an isolated test setup.

Rust USB Power Delivery sink software for the WCH CH32X035. The project is
aimed at user-configurable power sinks that need fixed SPR/EPR PDOs, PPS, EPR
AVS, and an explicit report of the current permitted by the source, cable, and
configured hardware limits.

The repository remains under engineering review while its application
integration is extracted into a stable library API. It is not yet a crates.io
release or a claim of complete specification compliance.

## Implemented behavior

- Decode and validate all eleven SPR/EPR Source PDO positions.
- Plan fixed, PPS, SPR AVS, and EPR AVS requests.
- Encode exact 20 mV PPS requests from an advertised 3.3 V endpoint and exact
  100 mV AVS requests through the standard 48 V limit.
- Preserve explicitly selected, source-advertised EPR AVS compatibility ranges
  from 5 V through 50 V without exposing them to normal automatic selection.
- Enter and exit EPR mode, retrieve chunked EPR capabilities, and send EPR
  keepalives.
- Begin every attachment at fixed 5 V and discover capabilities without
  automatically selecting a high voltage.
- Report requested, source-advertised, and usable current plus the limiting
  reason and confidence.
- Query live PPS voltage/current/temperature and CV/CL regulator mode, decode
  general Source Status, and follow Source Alert changes without disturbing
  the active contract.
- Handle SinkTxOK/SinkTxNG, Soft Reset, Hard Reset, detach, bounded retries,
  source-owned AMS traffic, and tested real-source compatibility cases.
- Expose a compact product-facing USB control protocol, an optional development
  text console, and a static browser GUI that automatically supports either.

Battery and variable PDOs remain visible when advertised but are deliberately
not requestable because they are outside this project's sink use case.

## Safety boundary

Firmware negotiation does not make a board safe for 28-50 V. The complete
connector, switch, FETs, discharge path, protection, spacing, measurement
network, and load must be rated for the selected voltage and fault energy.
The normal EPR profile remains capped at nominal 48 V; 50 V is a deliberate
non-standard compatibility profile, not a new standards-valid EPR level.

The reference integration assumes a 3.3 V-safe `VBUS_PRESENT` input and a
firmware `LOAD_ENABLE` output. The effective hardware gate must remain:

```text
LOAD_ON = MCU_LOAD_ENABLE AND VBUS_PRESENT AND HARDWARE_OK
```

The VBUS and hardware-health terms must disable the power path without working
firmware. See [`docs/hardware_interface.md`](docs/hardware_interface.md) before
adapting the reference firmware.

## Repository layout

- `crates/pd-sink/` - the public `no_std` API: request planning, capability and
  contract models, the reusable stack policy manager, and an optional
  pin-agnostic CH32X035 PHY adapter.
- `vendor/usbpd*` - the maintained protocol and policy-engine descendant.
- `vendor/ch32-hal/` - the pinned CH32 HAL descendant with PD PHY repairs and
  the compact USBFS CDC implementation.
- `examples/usb-console/` - the complete hardware reference firmware and
  interactive command surface.
- `tests/protocol/` - host-scripted protocol, reset, PPS, EPR, and malformed
  frame tests.
- `tools/pd-control/` - optional browser GUI using standalone desktop Web
  Serial or HTTPS-hosted Android WebUSB CDC.
- `scripts/` - reproducible checks, profile builds, USB ISP flashing, serial
  console, and standalone-GUI packaging.
- `docs/` - publishable architecture, hardware contract, interoperability, and
  validation material. USB-IF specifications and third-party datasheet files
  are intentionally not redistributed.

## Development

The repository pins a dated Rust toolchain and all resolved dependencies.
Docker is not required. On Windows, install Rust through
[rustup](https://rustup.rs/) and Microsoft C++ Build Tools, then run:

```powershell
.\scripts\bootstrap.ps1
.\scripts\check.ps1
.\scripts\build.ps1 -Profile usb-epr
```

The interactive example always negotiates 5 V first. Its main commands include:

```text
caps
plans
status
source-status
pps-status
enter-epr
request <millivolts> max pps
request <millivolts> <milliamps> epr-avs
request 48000 2000 fixed
exit-epr
```

Launch the optional browser interface with:

```powershell
.\scripts\gui.ps1
```

This packages and opens `artifacts\usb-pd-control.html`, then exits; there is
no local server to keep running. The single file is an offline desktop
launcher: Chrome/Edge use the standard CDC COM port through Web Serial.
Android Chrome has no Web Serial and WebUSB requires an HTTPS secure context,
so use the GitHub Pages copy at
`https://anasmalas.github.io/ch32x035-usb-pd-epr-sink/` after its first
deployment. The source is exactly the same static application in
`tools/pd-control/`; `.github/workflows/pages.yml` publishes it over HTTPS.
WebUSB preserves the same CDC-ACM firmware, so ordinary serial software can
still use the device when the browser releases it.

The normal `usb-safe-5v`, `usb-pps`, and `usb-epr` profiles use compact binary
`usb-control`; the browser translates it into the readable interface and
diagnostic stream. Use `usb-epr-text` only when a direct ASCII serial console
is useful during development. The legacy `usb-console` Cargo feature remains a
compatibility alias for `dev-text-console`.

`usb-epr-50v` is an opt-in compatibility profile for hardware explicitly
rated beyond a non-standard nominal 50 V source request. It does not change
normal AVS selection, which stays inside the source's 15-48 V standard
intersection; use the compatibility preference or a direct PDO adjustment to
reach a genuinely advertised value above 48 V.

For application integration, enable the crate's `ch32x035` feature, provide
the small `SinkRuntime` and `Ch32x035Port` adapters, and keep the GPIO choices
in your application. See [`docs/integration.md`](docs/integration.md). The
reference firmware is a consumer of the same API; PA6 and PB12 are not fixed
library requirements.

## Current evidence and limitations

Host tests cover request encoding and the protocol flows represented in this
repository. Physical CH32X035 hardware has completed SPR, PPS, 28 V EPR, and a
fixed 48 V EPR contract with real chargers. Those tests established protocol
interoperability; they did not validate a connected 48 V load path.

Important remaining work includes stabilizing the new reusable runtime API,
validating the intended hardware gate and detach behavior on the target board,
replacing the development USB VID/PID before distribution, and expanding
interoperability testing.

## Origins and licensing

Project-owned code is offered under either the MIT License or Apache License
2.0. Maintained upstream descendants retain their original licensing and
provenance. See [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) and each
`vendor/*/UPSTREAM.md` file.

USB and USB-C are used descriptively. This project is not endorsed or certified
by USB-IF and does not distribute USB-IF specification documents.
