[CmdletBinding()]
param(
    [switch]$InstallWchisp
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $workspace 'target' }

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    $rustup = Join-Path $cargoBin 'rustup.exe'

    if (Test-Path -LiteralPath $rustup -PathType Leaf) {
        $env:Path = "$cargoBin;$env:Path"
    }
    else {
        throw 'Rustup is not installed. Install it from https://rustup.rs/, open a new PowerShell window, and run this script again.'
    }
}

Push-Location $workspace
try {
    Write-Host 'Activating the repository-pinned Rust toolchain...'
    rustup show active-toolchain
    if ($LASTEXITCODE -ne 0) { throw "rustup failed with exit code $LASTEXITCODE" }

    Write-Host 'Fetching dependencies and creating Cargo.lock...'
    cargo fetch
    if ($LASTEXITCODE -ne 0) { throw "cargo fetch failed with exit code $LASTEXITCODE" }

    Write-Host 'Building the release firmware...'
    cargo build --workspace --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    $artifactDirectory = Join-Path $workspace 'artifacts'
    $builtFirmware = Join-Path $targetDirectory 'riscv32imc-unknown-none-elf\release\ch32x035-usb-pd-epr-sink-reference'
    New-Item -ItemType Directory -Path $artifactDirectory -Force | Out-Null
    Copy-Item -LiteralPath $builtFirmware -Destination (Join-Path $artifactDirectory 'ch32x035-usb-pd-epr-sink-reference.elf') -Force

    if ($InstallWchisp) {
        Write-Host 'Installing wchisp 0.3.0...'
        cargo install wchisp --version 0.3.0 --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo install wchisp failed with exit code $LASTEXITCODE" }
    }
}
finally {
    Pop-Location
}

Write-Host 'Native CH32X035 development environment is ready.'
