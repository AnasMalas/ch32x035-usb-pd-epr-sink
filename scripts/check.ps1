[CmdletBinding()]
param(
    [ValidatePattern('^[A-Za-z0-9_.-]+$')]
    [string]$HostTarget,
    [switch]$RequireNode
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot

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
$usbControlEprFeatures = 'usb-control,epr-capable-hardware'
$textConsoleEprFeatures = 'dev-text-console,epr-capable-hardware'
$firmwareFeatureSets = @(
    'usb-control',
    'usb-control,pps-capable-hardware',
    $usbControlEprFeatures,
    'usb-control,epr-50v-compatible-hardware',
    $textConsoleEprFeatures
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

    $availableFeatures = @($firmwareMetadata.features.PSObject.Properties.Name)
    foreach ($featureSet in $firmwareFeatureSets) {
        foreach ($feature in $featureSet.Split(',')) {
            if ($availableFeatures -notcontains $feature) {
                throw "Firmware feature '$feature' from '$featureSet' is missing from Cargo metadata."
            }
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

    cargo clippy -p ch32x035-usb-pd-epr-sink -p ch32x035-usb-pd-epr-sink-protocol-tests --all-targets --target $HostTarget --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "host clippy failed with exit code $LASTEXITCODE" }

    cargo clippy --manifest-path vendor/usbpd/Cargo.toml --all-targets --target $HostTarget -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $usbControlEprFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "USB-control EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p $firmwarePackage --release --locked --no-default-features --features $textConsoleEprFeatures -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "development text-console EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink -p ch32x035-usb-pd-epr-sink-protocol-tests --target $HostTarget --locked
    if ($LASTEXITCODE -ne 0) { throw "host tests failed with exit code $LASTEXITCODE" }

    cargo test --manifest-path vendor/usbpd/Cargo.toml --target $HostTarget
    if ($LASTEXITCODE -ne 0) { throw "vendored usbpd tests failed with exit code $LASTEXITCODE" }

    cargo build -p $firmwarePackage --release --locked
    if ($LASTEXITCODE -ne 0) { throw "safe 5 V firmware build failed with exit code $LASTEXITCODE" }

    foreach ($feature in $firmwareFeatureSets) {
        cargo build -p $firmwarePackage --release --locked --no-default-features --features $feature
        if ($LASTEXITCODE -ne 0) { throw "$feature firmware build failed with exit code $LASTEXITCODE" }
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
