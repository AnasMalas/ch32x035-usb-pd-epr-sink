[CmdletBinding(DefaultParameterSetName = 'ByProfile')]
param(
    [Parameter(ParameterSetName = 'ByProfile')]
    [ValidateSet(
        'safe-5v',
        'usb-safe-5v',
        'usb-pps',
        'usb-epr',
        'usb-epr-diagnostic',
        'usb-epr-50v',
        'usb-epr-text'
    )]
    [string]$Profile = 'safe-5v',

    [Parameter(ParameterSetName = 'ByProfile')]
    [ValidateSet(
        'ch32x035c8t6',
        'ch32x035f7p6',
        'ch32x035f8u6',
        'ch32x035g8r6',
        'ch32x035g8u6',
        'ch32x035r8t6'
    )]
    [string]$Chip = 'ch32x035f8u6',

    [Parameter(Mandatory, ParameterSetName = 'ByFirmware')]
    [string]$Firmware
)

$ErrorActionPreference = 'Stop'
$example = Split-Path -Parent $PSScriptRoot
$examples = Split-Path -Parent $example
$workspace = Split-Path -Parent $examples
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Get-Command wchisp -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path $cargoBin 'wchisp.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command wchisp -ErrorAction SilentlyContinue)) {
    throw 'wchisp is not installed. Run .\examples\ch32x035-usb-pd-sink-firmware\scripts\install-wchisp.ps1 or install a prebuilt wchisp release.'
}

$firmwarePath = if ($PSCmdlet.ParameterSetName -eq 'ByProfile') {
    Join-Path $examples "generated-artifacts\ch32x035-usb-pd-epr-sink-reference-$Chip-$Profile.elf"
}
else {
    Join-Path $workspace $Firmware
}

if (-not (Test-Path -LiteralPath $firmwarePath -PathType Leaf)) {
    if ($PSCmdlet.ParameterSetName -eq 'ByProfile') {
        throw "Firmware profile '$Profile' for '$Chip' not found at $firmwarePath. Run .\examples\ch32x035-usb-pd-sink-firmware\scripts\build.ps1 -Profile $Profile -Chip $Chip first."
    }
    throw "Firmware not found at $firmwarePath. Run the reference firmware build script first."
}

$firmwareItem = Get-Item -LiteralPath $firmwarePath
$firmwareHash = (Get-FileHash -LiteralPath $firmwarePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($PSCmdlet.ParameterSetName -eq 'ByProfile') {
    Write-Host "Profile:  $Profile"
    Write-Host "Chip:     $Chip"
}
Write-Host "Firmware: $($firmwareItem.FullName)"
Write-Host "Modified: $($firmwareItem.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))"
Write-Host "SHA-256:  $firmwareHash"
Write-Host 'Place the CH32X035 in USB ISP mode, then press Enter.'
[void](Read-Host)
wchisp flash $firmwarePath
if ($LASTEXITCODE -ne 0) { throw "wchisp failed with exit code $LASTEXITCODE" }
