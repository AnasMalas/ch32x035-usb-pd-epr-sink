[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot

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

    Write-Host 'Building the release workspace...'
    cargo build --workspace --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

Write-Host 'Native workspace is ready.'
