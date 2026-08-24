[CmdletBinding()]
param(
    [ValidateSet(
        'safe-5v',
        'rev0-validation',
        'usb-safe-5v',
        'usb-pps',
        'usb-epr',
        'usb-epr-diagnostic',
        'usb-epr-50v',
        'usb-epr-text'
    )]
    [string]$Profile = 'usb-safe-5v',

    [ValidateSet(
        'ch32x035c8t6',
        'ch32x035f7p6',
        'ch32x035f8u6',
        'ch32x035g8r6',
        'ch32x035g8u6',
        'ch32x035r8t6'
    )]
    [string]$Chip = 'ch32x035f8u6'
)

$ErrorActionPreference = 'Stop'
$example = Split-Path -Parent $PSScriptRoot
$examples = Split-Path -Parent $example
$workspace = Split-Path -Parent $examples
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $workspace 'target' }
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Cargo is not installed. Run .\scripts\bootstrap.ps1 first.'
}

$rev0Profiles = @(
    'rev0-validation',
    'usb-safe-5v',
    'usb-pps',
    'usb-epr',
    'usb-epr-diagnostic',
    'usb-epr-50v',
    'usb-epr-text'
)

if ($Profile -in $rev0Profiles) {
    if ($PSBoundParameters.ContainsKey('Chip') -and $Chip -ne 'ch32x035g8u6') {
        throw "Profile '$Profile' requires -Chip ch32x035g8u6."
    }
    $Chip = 'ch32x035g8u6'
}

Push-Location $workspace
try {
    $profileFeatures = switch ($Profile) {
        'safe-5v' { 'sdi-log' }
        'rev0-validation' { 'dev-text-console,output-default-off,rev0-validation' }
        'usb-safe-5v' { 'usb-control,output-default-off,rev0-board' }
        'usb-pps' { 'usb-control,pps-capable-hardware,output-default-off,rev0-board' }
        'usb-epr' { 'usb-control,epr-capable-hardware,output-default-off,rev0-board' }
        'usb-epr-diagnostic' { 'usb-control,epr-capable-hardware,output-default-off,rev0-board' }
        'usb-epr-50v' { 'usb-control,epr-50v-compatible-hardware,output-default-off,rev0-board' }
        'usb-epr-text' { 'dev-text-console,epr-capable-hardware,output-default-off,rev0-board' }
    }
    $selectedFeatures = "$Chip,$profileFeatures"

    $arguments = @(
        'build',
        '-p',
        'ch32x035-usb-pd-epr-sink-reference',
        '--release',
        '--locked',
        '--no-default-features',
        '--features',
        $selectedFeatures
    )

    cargo @arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    $artifactDirectory = Join-Path $examples 'generated-artifacts'
    $builtFirmware = Join-Path $targetDirectory 'riscv32imc-unknown-none-elf\release\ch32x035-usb-pd-epr-sink-reference'
    New-Item -ItemType Directory -Path $artifactDirectory -Force | Out-Null
    $profileArtifact = Join-Path $artifactDirectory "ch32x035-usb-pd-epr-sink-reference-$Chip-$Profile.elf"
    Copy-Item -LiteralPath $builtFirmware -Destination $profileArtifact -Force
    $applicationFlashKiB = if ($Chip -eq 'ch32x035f7p6') { 48 } else { 62 }
    & (Join-Path $PSScriptRoot 'size.ps1') -Firmware $profileArtifact -FlashKiB $applicationFlashKiB
    Write-Host "Built profile '$Profile' for '$Chip': $profileArtifact"
    Write-Host "Flash it with: .\examples\ch32x035-usb-pd-sink-firmware\scripts\flash.ps1 -Profile $Profile -Chip $Chip"
    if ($Profile -eq 'safe-5v' -and $Chip -eq 'ch32x035f8u6') {
        Copy-Item -LiteralPath $builtFirmware -Destination (Join-Path $artifactDirectory 'ch32x035-usb-pd-epr-sink-reference.elf') -Force
    }
}
finally {
    Pop-Location
}
