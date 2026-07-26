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

& (Join-Path $PSScriptRoot 'build.ps1') -Profile $Profile
& (Join-Path $PSScriptRoot 'flash.ps1') -Profile $Profile
