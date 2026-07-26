[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and
    (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Cargo is not installed. Run .\scripts\bootstrap.ps1 first.'
}

Write-Host 'Installing wchisp 0.3.0...'
cargo install wchisp --version 0.3.0 --locked
if ($LASTEXITCODE -ne 0) {
    throw "cargo install failed with exit code $LASTEXITCODE"
}
