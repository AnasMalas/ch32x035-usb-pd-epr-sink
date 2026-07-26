[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Cargo is not installed. Run .\scripts\bootstrap.ps1 first.'
}

Push-Location $workspace
try {
    & (Join-Path $PSScriptRoot 'check-docs.ps1')

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

    cargo clippy -p ch32x035-usb-pd-epr-sink -p ch32x035-usb-pd-epr-sink-protocol-tests --all-targets --target x86_64-pc-windows-msvc --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "host clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p ch32x035-usb-pd-epr-sink-reference --release --locked --no-default-features --features 'usb-control,epr-capable-hardware' -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "USB-control EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo clippy -p ch32x035-usb-pd-epr-sink-reference --release --locked --no-default-features --features 'dev-text-console,epr-capable-hardware' -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "development text-console EPR firmware clippy failed with exit code $LASTEXITCODE" }

    cargo test -p ch32x035-usb-pd-epr-sink -p ch32x035-usb-pd-epr-sink-protocol-tests --target x86_64-pc-windows-msvc --locked
    if ($LASTEXITCODE -ne 0) { throw "host tests failed with exit code $LASTEXITCODE" }

    cargo build -p ch32x035-usb-pd-epr-sink-reference --release --locked
    if ($LASTEXITCODE -ne 0) { throw "safe 5 V firmware build failed with exit code $LASTEXITCODE" }

    foreach ($feature in @(
        'usb-control',
        'usb-control,pps-capable-hardware',
        'usb-control,epr-capable-hardware',
        'usb-control,epr-50v-compatible-hardware',
        'dev-text-console,epr-capable-hardware'
    )) {
        cargo build -p ch32x035-usb-pd-epr-sink-reference --release --locked --no-default-features --features $feature
        if ($LASTEXITCODE -ne 0) { throw "$feature firmware build failed with exit code $LASTEXITCODE" }
    }

    $guiCheckPath = Join-Path ([System.IO.Path]::GetTempPath()) "usb-pd-control-$([guid]::NewGuid().ToString('N')).html"
    try {
        $packagedGui = & (Join-Path $PSScriptRoot 'package-gui.ps1') -Output $guiCheckPath
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

    if (Get-Command node -ErrorAction SilentlyContinue) {
        node tools/pd-control/protocol.test.js
        if ($LASTEXITCODE -ne 0) { throw "browser protocol tests failed with exit code $LASTEXITCODE" }
    }
    else {
        Write-Host 'Node.js is not installed; browser protocol tests were skipped (the GUI itself does not require Node.js).'
    }
}
finally {
    Pop-Location
}
