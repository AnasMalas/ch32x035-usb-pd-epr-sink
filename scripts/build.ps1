[CmdletBinding()]
param(
    [ValidateSet(
        'safe-5v',
        'usb-safe-5v',
        'usb-pps',
        'usb-epr',
        'usb-epr-50v',
        'usb-epr-text'
    )]
    [string]$Profile = 'safe-5v'
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $workspace 'target' }
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Cargo is not installed. Run .\scripts\bootstrap.ps1 first.'
}

Push-Location $workspace
try {
    $profileArguments = switch ($Profile) {
        'safe-5v' { @() }
        'usb-safe-5v' { @('--no-default-features', '--features', 'usb-control') }
        'usb-pps' { @('--no-default-features', '--features', 'usb-control,pps-capable-hardware') }
        'usb-epr' { @('--no-default-features', '--features', 'usb-control,epr-capable-hardware') }
        'usb-epr-50v' { @('--no-default-features', '--features', 'usb-control,epr-50v-compatible-hardware') }
        'usb-epr-text' { @('--no-default-features', '--features', 'dev-text-console,epr-capable-hardware') }
    }

    $arguments = @('build', '-p', 'ch32x035-usb-pd-epr-sink-reference', '--release', '--locked')
    $arguments += $profileArguments

    cargo @arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    $artifactDirectory = Join-Path $workspace 'artifacts'
    $builtFirmware = Join-Path $targetDirectory 'riscv32imc-unknown-none-elf\release\ch32x035-usb-pd-epr-sink-reference'
    New-Item -ItemType Directory -Path $artifactDirectory -Force | Out-Null
    $profileArtifact = Join-Path $artifactDirectory "ch32x035-usb-pd-epr-sink-reference-$Profile.elf"
    Copy-Item -LiteralPath $builtFirmware -Destination $profileArtifact -Force
    & (Join-Path $PSScriptRoot 'size.ps1') -Firmware $profileArtifact
    Write-Host "Built profile '$Profile': $profileArtifact"
    Write-Host "Flash it with: .\scripts\flash.ps1 -Profile $Profile"
    if ($Profile -eq 'safe-5v') {
        Copy-Item -LiteralPath $builtFirmware -Destination (Join-Path $artifactDirectory 'ch32x035-usb-pd-epr-sink-reference.elf') -Force
    }
}
finally {
    Pop-Location
}
