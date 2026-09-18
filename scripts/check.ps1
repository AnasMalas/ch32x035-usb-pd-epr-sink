[CmdletBinding()]
param(
    [ValidatePattern('^[A-Za-z0-9_.-]+$')]
    [string]$HostTarget,
    [switch]$RequireNode
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $workspace 'target' }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $userProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    if (-not [string]::IsNullOrWhiteSpace($userProfile)) {
        $cargoBin = Join-Path $userProfile '.cargo/bin'
        $cargoExecutable = if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
            'cargo.exe'
        }
        else {
            'cargo'
        }

        if (Test-Path -LiteralPath (Join-Path $cargoBin $cargoExecutable) -PathType Leaf) {
            $env:Path = "$cargoBin$([IO.Path]::PathSeparator)$env:Path"
        }
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Cargo is not installed. Run .\scripts\bootstrap.ps1 first.'
}

if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
    throw 'rustc is not installed. Run .\scripts\bootstrap.ps1 first.'
}

$nodeAvailable = $null -ne (Get-Command node -ErrorAction SilentlyContinue)
if ($RequireNode -and -not $nodeAvailable) {
    throw 'Node.js is required for browser protocol tests but was not found.'
}

if ([string]::IsNullOrWhiteSpace($HostTarget)) {
    $rustcVersion = @(rustc -vV)
    if ($LASTEXITCODE -ne 0) { throw "rustc -vV failed with exit code $LASTEXITCODE" }

    $hostRecord = $rustcVersion | Where-Object { $_ -match '^host:\s+\S+$' } | Select-Object -First 1
    if (-not $hostRecord) {
        throw 'Could not determine the native Rust host target.'
    }
    $HostTarget = ($hostRecord -replace '^host:\s+', '').Trim()
}

Write-Host "Native host checks use target $HostTarget."

$firmwarePackage = 'ch32x035-usb-pd-epr-sink-reference'
$ccWakeProbePackage = 'ch32x035-usbpd-cc-wake-probe'
$halTestPackage = 'ch32x035-usb-pd-epr-sink-hal-tests'
$referenceChip = 'ch32x035f8u6'
$rev0Chip = 'ch32x035g8u6'
$supportedChips = @(
    'ch32x035c8t6',
    'ch32x035f7p6',
    'ch32x035f8u6',
    'ch32x035g8r6',
    'ch32x035g8u6',
    'ch32x035r8t6'
)
$usbControlEprFeatures = "$referenceChip,usb-control,rich-telemetry,epr-capable-hardware"
$diagnosticEprFeatures = "$referenceChip,usb-control,rich-telemetry,epr-capable-hardware,output-default-off"
$textConsoleEprFeatures = "$referenceChip,dev-text-console,epr-capable-hardware"
$rev0UsbSafeFeatures = "$rev0Chip,usb-control,rich-telemetry,output-default-off,rev0-board"
$rev0UsbPpsFeatures = "$rev0Chip,usb-control,rich-telemetry,pps-capable-hardware,output-default-off,rev0-board"
$rev0UsbEprFeatures = "$rev0Chip,usb-control,rich-telemetry,epr-capable-hardware,output-default-off,rev0-board"
$rev0LeanUsbEprFeatures = "$rev0Chip,usb-control,epr-capable-hardware,output-default-off,rev0-board"
$rev0UsbEprTraceFeatures = "$rev0UsbEprFeatures,numeric-trace"
$rev0UsbEprDriverTraceFeatures = "$rev0UsbEprFeatures,driver-boundary-trace"
$rev0UsbEprBlackBoxFeatures = "$rev0UsbEprFeatures,persistent-black-box"
$rev0UsbEprDeepBlackBoxFeatures = "$rev0UsbEprFeatures,deep-black-box"
$rev0ValidationFeatures = "$rev0Chip,dev-text-console,output-default-off,rev0-validation"
$firmwareFeatureSets = @(
    "$referenceChip,usb-control,rich-telemetry",
    "$referenceChip,usb-control,rich-telemetry,pps-capable-hardware",
    $usbControlEprFeatures,
    $diagnosticEprFeatures,
    "$referenceChip,usb-control,rich-telemetry,epr-50v-compatible-hardware",
    $textConsoleEprFeatures,
    $rev0UsbSafeFeatures,
    $rev0UsbPpsFeatures,
    $rev0UsbEprFeatures,
    $rev0LeanUsbEprFeatures,
    $rev0UsbEprDriverTraceFeatures,
    $rev0UsbEprBlackBoxFeatures,
    $rev0UsbEprDeepBlackBoxFeatures,
    $rev0ValidationFeatures
)

Push-Location $workspace
try {
    & (Join-Path $PSScriptRoot 'check-tooling.ps1')
    & (Join-Path $PSScriptRoot 'check-docs.ps1')

    $metadataJson = cargo metadata --no-deps --locked --format-version 1
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed with exit code $LASTEXITCODE" }
    $metadata = $metadataJson | ConvertFrom-Json
    $firmwareMetadata = $metadata.packages |
        Where-Object { $_.name -eq $firmwarePackage } |
        Select-Object -First 1
    if (-not $firmwareMetadata) {
        throw "Firmware package '$firmwarePackage' is missing from Cargo metadata."
    }
    $ccWakeProbeMetadata = $metadata.packages |
        Where-Object { $_.name -eq $ccWakeProbePackage } |
        Select-Object -First 1
    if (-not $ccWakeProbeMetadata) {
        throw "CC wake probe package '$ccWakeProbePackage' is missing from Cargo metadata."
    }

    $availableFeatures = @($firmwareMetadata.features.PSObject.Properties.Name)
    foreach ($featureSet in $firmwareFeatureSets) {
        foreach ($feature in $featureSet.Split(',')) {
            if ($availableFeatures -notcontains $feature) {
                throw "Firmware feature '$feature' from '$featureSet' is missing from Cargo metadata."
            }
        }
    }
    foreach ($chip in $supportedChips) {
        if ($availableFeatures -notcontains $chip) {
            throw "Supported firmware chip feature '$chip' is missing from Cargo metadata."
        }
        if (@($ccWakeProbeMetadata.features.PSObject.Properties.Name) -notcontains $chip) {
            throw "Supported CC wake probe chip feature '$chip' is missing from Cargo metadata."
        }
    }

    cargo fmt --all --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed with exit code $LASTEXITCODE" }

    foreach ($manifest in @(
        'vendor/ch32-hal/Cargo.toml',
        'vendor/usbpd/Cargo.toml',
        'vendor/usbpd-traits/Cargo.toml'
    )) {
        cargo fmt --manifest-path $manifest -- --check
        if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed for $manifest with exit code $LASTEXITCODE" }
    }

    cargo clippy -p ch32x035-usb-pd-epr-sink -p ch32x035-usb-pd-epr-sink-protocol-tests -p $halTestPackage --all-targets --target $HostTarget --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "host clippy failed with exit code $LASTEXITCODE" }

    cargo clippy --manifest-path vendor/usbpd/Cargo.toml --all-targets --target $HostTarget -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd clippy failed with exit code $LASTEXITCODE" }

    cargo clippy --manifest-path vendor/usbpd/Cargo.toml --target $HostTarget --no-default-features --lib -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "vendored sink-only usbpd clippy failed with exit code $LASTEXITCODE" }

    cargo clippy --manifest-path vendor/usbpd/Cargo.toml --all-targets --target $HostTarget --features numeric-trace,hard-reset-reasons -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd numeric-trace clippy failed with exit code $LASTEXITCODE" }

    cargo clippy --manifest-path vendor/usbpd/Cargo.toml --all-targets --target $HostTarget --no-default-features --features initial-capabilities-fallback,hard-reset-reasons -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd initial-capabilities fallback clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p ch32x035-usb-pd-epr-sink --all-targets --target $HostTarget --locked --no-default-features --features initial-capabilities-fallback,hard-reset-reasons -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "public initial-capabilities fallback clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p ch32x035-usb-pd-epr-sink --all-targets --target $HostTarget --locked --no-default-features --features black-box,numeric-trace,hard-reset-reasons -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "public black-box clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $usbControlEprFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "USB-control EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $textConsoleEprFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "development text-console EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0ValidationFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 validation firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0UsbEprFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 compact EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0LeanUsbEprFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 basic-telemetry EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0UsbEprTraceFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 numeric-trace EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0UsbEprDriverTraceFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 driver-boundary-trace EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0UsbEprBlackBoxFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 persistent black-box firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $rev0UsbEprDeepBlackBoxFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "rev0 deep black-box firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $ccWakeProbePackage --release --locked --no-default-features --features $referenceChip -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "CC wake probe clippy failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink -p ch32x035-usb-pd-epr-sink-protocol-tests -p $halTestPackage --target $HostTarget --locked
    if ($LASTEXITCODE -ne 0) { throw "host tests failed with exit code $LASTEXITCODE" }

    cargo test --manifest-path vendor/usbpd/Cargo.toml --target $HostTarget
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd tests failed with exit code $LASTEXITCODE" }

    cargo test --manifest-path vendor/usbpd/Cargo.toml --target $HostTarget --no-default-features --test sink_without_source
    if ($LASTEXITCODE -ne 0) { throw "vendored sink-only usbpd tests failed with exit code $LASTEXITCODE" }

    cargo test --manifest-path vendor/usbpd/Cargo.toml --target $HostTarget --features numeric-trace,hard-reset-reasons
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd numeric-trace tests failed with exit code $LASTEXITCODE" }

    cargo test --manifest-path vendor/usbpd/Cargo.toml --target $HostTarget --no-default-features --features initial-capabilities-fallback,hard-reset-reasons
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd initial-capabilities fallback tests failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink --target $HostTarget --locked --no-default-features --features hard-reset-reasons
    if ($LASTEXITCODE -ne 0) { throw "feature-off public library tests failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink --target $HostTarget --locked --no-default-features --features numeric-trace,hard-reset-reasons
    if ($LASTEXITCODE -ne 0) { throw "public numeric-trace tests failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink --target $HostTarget --locked --no-default-features --features black-box,numeric-trace,hard-reset-reasons
    if ($LASTEXITCODE -ne 0) { throw "public black-box tests failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink --target $HostTarget --locked --no-default-features --features initial-capabilities-fallback,hard-reset-reasons
    if ($LASTEXITCODE -ne 0) { throw "public initial-capabilities fallback tests failed with exit code $LASTEXITCODE" }

    foreach ($chip in $supportedChips) {
        # F7P6 has only 48 KiB of application flash. Prove its complete
        # pin/PHY/session integration with the fitting console-free 5 V
        # reference; the GUI-capable EPR example is intentionally documented
        # as too large for that package. All 62 KiB packages link the complete
        # compact USB/EPR reference here.
        $packageCheckFeatures = if ($chip -eq 'ch32x035f7p6') {
            $chip
        }
        else {
            "$chip,usb-control,rich-telemetry,epr-capable-hardware"
        }
        cargo build -p $firmwarePackage --release --locked --no-default-features --features $packageCheckFeatures
        if ($LASTEXITCODE -ne 0) { throw "$chip reference firmware build failed with exit code $LASTEXITCODE" }

        $firmwareElf = Join-Path $targetDirectory "riscv32imc-unknown-none-elf/release/$firmwarePackage"
        $applicationFlashBytes = if ($chip -eq 'ch32x035f7p6') { 48 * 1024 } else { 62 * 1024 }
        & (Join-Path $PSScriptRoot 'check-elf-layout.ps1') `
            -Firmware $firmwareElf `
            -ApplicationFlashBytes $applicationFlashBytes

        cargo check -p $ccWakeProbePackage --release --locked --no-default-features --features $chip
        if ($LASTEXITCODE -ne 0) { throw "$chip CC wake probe check failed with exit code $LASTEXITCODE" }
    }

    cargo build -p $firmwarePackage --release --locked
    if ($LASTEXITCODE -ne 0) { throw "safe 5 V firmware build failed with exit code $LASTEXITCODE" }

    foreach ($feature in $firmwareFeatureSets) {
        cargo build -p $firmwarePackage --release --locked --no-default-features --features $feature
        if ($LASTEXITCODE -ne 0) { throw "$feature firmware build failed with exit code $LASTEXITCODE" }
        if ($feature -in @($rev0UsbSafeFeatures, $rev0UsbPpsFeatures, $rev0UsbEprFeatures, $rev0LeanUsbEprFeatures, $rev0UsbEprDriverTraceFeatures, $rev0UsbEprBlackBoxFeatures, $rev0UsbEprDeepBlackBoxFeatures, $rev0ValidationFeatures)) {
            $firmwareElf = Join-Path $targetDirectory "riscv32imc-unknown-none-elf/release/$firmwarePackage"
            $applicationFlashBytes = if ($feature -in @($rev0UsbEprBlackBoxFeatures, $rev0UsbEprDeepBlackBoxFeatures)) { 0xF600 } else { 62 * 1024 }
            & (Join-Path $PSScriptRoot 'check-elf-layout.ps1') `
                -Firmware $firmwareElf `
                -ApplicationFlashBytes $applicationFlashBytes
        }
    }

    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        $profileBuild = Join-Path $workspace 'examples/ch32x035-usb-pd-sink-firmware/scripts/build.ps1'
        foreach ($profile in @(
            'rev0-validation',
            'usb-safe-5v',
            'usb-pps',
            'usb-epr',
            'usb-epr-uninterrupted',
            'usb-epr-diagnostic',
            'usb-epr-black-box',
            'usb-epr-deep-black-box',
            'usb-epr-50v',
            'usb-epr-text'
        )) {
            $wrongChipRejected = $false
            try {
                & $profileBuild -Profile $profile -Chip $referenceChip
            }
            catch {
                if ($_.Exception.Message -notlike '*requires -Chip ch32x035g8u6*') { throw }
                $wrongChipRejected = $true
            }
            if (-not $wrongChipRejected) {
                throw "$profile profile accepted a non-G8U6 chip."
            }

            & $profileBuild -Profile $profile
            $profileArtifact = Join-Path $workspace "examples/generated-artifacts/ch32x035-usb-pd-epr-sink-reference-$rev0Chip-$profile.elf"
            if (-not (Test-Path -LiteralPath $profileArtifact -PathType Leaf)) {
                throw "$profile did not stage $profileArtifact"
            }
            $applicationFlashBytes = if ($profile -in @('usb-epr-black-box', 'usb-epr-deep-black-box')) { 0xF600 } else { 62 * 1024 }
            & (Join-Path $PSScriptRoot 'check-elf-layout.ps1') `
                -Firmware $profileArtifact `
                -ApplicationFlashBytes $applicationFlashBytes
        }
    }

    $guiCheckPath = Join-Path ([System.IO.Path]::GetTempPath()) "usb-pd-control-$([guid]::NewGuid().ToString('N')).html"
    try {
        $packageGui = Join-Path $workspace 'examples/browser-usb-pd-control-client/scripts/package.ps1'
        $packagedGui = & $packageGui -Output $guiCheckPath
        $gui = [System.IO.File]::ReadAllText($packagedGui)
        foreach ($required in @(
            'Condense stream: On',
            'Raw stream: Off',
            'navigator.serial.requestPort',
            'navigator.usb.requestDevice'
        )) {
            if (-not $gui.Contains($required)) {
                throw "standalone GUI is missing '$required'"
            }
        }
        foreach ($external in @(
            'href="styles.css"',
            'src="protocol.js"',
            'src="app.js"'
        )) {
            if ($gui.Contains($external)) {
                throw "standalone GUI still references '$external'"
            }
        }
    }
    finally {
        if (Test-Path -LiteralPath $guiCheckPath) {
            Remove-Item -LiteralPath $guiCheckPath -Force
        }
    }

    if ($nodeAvailable) {
        node --check examples/browser-usb-pd-control-client/app.js
        if ($LASTEXITCODE -ne 0) { throw "browser application syntax check failed with exit code $LASTEXITCODE" }
        node examples/browser-usb-pd-control-client/console.test.js
        if ($LASTEXITCODE -ne 0) { throw "browser console structure tests failed with exit code $LASTEXITCODE" }
        node examples/browser-usb-pd-control-client/protocol.test.js
        if ($LASTEXITCODE -ne 0) { throw "browser protocol tests failed with exit code $LASTEXITCODE" }
    }
    else {
        Write-Host 'Node.js is not installed; browser protocol tests were skipped (the GUI itself does not require Node.js).'
    }
}
finally {
    Pop-Location
}
