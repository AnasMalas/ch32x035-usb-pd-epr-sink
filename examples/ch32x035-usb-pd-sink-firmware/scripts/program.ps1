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

& (Join-Path $PSScriptRoot 'build.ps1') -Profile $Profile -Chip $Chip
& (Join-Path $PSScriptRoot 'flash.ps1') -Profile $Profile -Chip $Chip
