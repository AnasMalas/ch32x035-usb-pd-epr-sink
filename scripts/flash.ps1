[CmdletBinding(DefaultParameterSetName = 'ByProfile')]
param(
    [Parameter(ParameterSetName = 'ByProfile')]
    [ValidateSet(
        'safe-5v',
        'usb-safe-5v',
        'usb-pps',
        'usb-epr',
        'usb-epr-text',
        'usb-epr-dual-log',
        'pps-19v4',
        'epr-avs-19v4',
        'epr-fixed-48v'
    )]
    [string]$Profile = 'safe-5v',

    [Parameter(Mandatory, ParameterSetName = 'ByFirmware')]
    [string]$Firmware
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

if (-not (Get-Command wchisp -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath (Join-Path $cargoBin 'wchisp.exe'))) {
    $env:Path = "$cargoBin;$env:Path"
}

if (-not (Get-Command wchisp -ErrorAction SilentlyContinue)) {
    throw 'wchisp is not installed. Run .\scripts\bootstrap.ps1 -InstallWchisp or install a prebuilt wchisp release.'
}

$firmwarePath = if ($PSCmdlet.ParameterSetName -eq 'ByProfile') {
    Join-Path $workspace "artifacts\ch32x035-usb-pd-epr-sink-example-$Profile.elf"
}
else {
    Join-Path $workspace $Firmware
}

if (-not (Test-Path -LiteralPath $firmwarePath -PathType Leaf)) {
    if ($PSCmdlet.ParameterSetName -eq 'ByProfile') {
        throw "Firmware profile '$Profile' not found at $firmwarePath. Run .\scripts\build.ps1 -Profile $Profile first."
    }
    throw "Firmware not found at $firmwarePath. Run .\scripts\build.ps1 first."
}

$firmwareItem = Get-Item -LiteralPath $firmwarePath
$firmwareHash = (Get-FileHash -LiteralPath $firmwarePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($PSCmdlet.ParameterSetName -eq 'ByProfile') {
    Write-Host "Profile:  $Profile"
}
Write-Host "Firmware: $($firmwareItem.FullName)"
Write-Host "Modified: $($firmwareItem.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))"
Write-Host "SHA-256:  $firmwareHash"
Write-Host 'Place the CH32X035 in USB ISP mode, then press Enter.'
[void](Read-Host)
wchisp flash $firmwarePath
if ($LASTEXITCODE -ne 0) { throw "wchisp failed with exit code $LASTEXITCODE" }
