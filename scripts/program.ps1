[CmdletBinding()]
param(
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
    [string]$Profile = 'safe-5v'
)

$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'build.ps1') -Profile $Profile
& (Join-Path $PSScriptRoot 'flash.ps1') -Profile $Profile
