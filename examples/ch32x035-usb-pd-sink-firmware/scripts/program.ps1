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
    [string]$Profile = 'safe-5v',

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

& (Join-Path $PSScriptRoot 'build.ps1') -Profile $Profile -Chip $Chip
& (Join-Path $PSScriptRoot 'flash.ps1') -Profile $Profile -Chip $Chip
