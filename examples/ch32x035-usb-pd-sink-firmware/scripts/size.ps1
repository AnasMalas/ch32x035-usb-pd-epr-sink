[CmdletBinding()]
param(
    [string]$Firmware = 'ch32x035-usb-pd-epr-sink-reference.elf',
    [ValidateRange(1, 1024)]
    [int]$FlashKiB = 62,
    [ValidateRange(0, 1048576)]
    [int]$FlashBytes = 0,
    [ValidateRange(1, 1024)]
    [int]$RamKiB = 20
)

$ErrorActionPreference = 'Stop'
$example = Split-Path -Parent $PSScriptRoot
$examples = Split-Path -Parent $example
$artifactDirectory = Join-Path $examples 'generated-artifacts'
$firmwarePath = if ([IO.Path]::IsPathRooted($Firmware)) { $Firmware } else { Join-Path $artifactDirectory $Firmware }

if (-not (Test-Path -LiteralPath $firmwarePath -PathType Leaf)) {
    throw "Firmware not found at $firmwarePath."
}

$bytes = [IO.File]::ReadAllBytes($firmwarePath)
if ($bytes.Length -lt 52 -or $bytes[0] -ne 0x7f -or $bytes[1] -ne 0x45 -or $bytes[2] -ne 0x4c -or
    $bytes[3] -ne 0x46 -or $bytes[4] -ne 1 -or $bytes[5] -ne 1) {
    throw 'Expected a 32-bit little-endian ELF file.'
}

function Read-U16([int]$Offset) {
    [BitConverter]::ToUInt16($bytes, $Offset)
}

function Read-U32([int]$Offset) {
    [BitConverter]::ToUInt32($bytes, $Offset)
}

$programOffset = Read-U32 28
$programEntrySize = Read-U16 42
$programCount = Read-U16 44
$flashUsed = [uint64]0

for ($index = 0; $index -lt $programCount; $index++) {
    $header = $programOffset + $index * $programEntrySize
    $type = Read-U32 $header
    if ($type -ne 1) { continue }

    $physicalAddress = Read-U32 ($header + 12)
    $fileSize = Read-U32 ($header + 16)
    if ($fileSize -gt 0 -and $physicalAddress -lt 0x20000000) {
        $end = [uint64]$physicalAddress + [uint64]$fileSize
        if ($end -gt $flashUsed) { $flashUsed = $end }
    }
}

$sectionOffset = Read-U32 32
$sectionEntrySize = Read-U16 46
$sectionCount = Read-U16 48
$staticRam = [uint64]0

for ($index = 0; $index -lt $sectionCount; $index++) {
    $header = $sectionOffset + $index * $sectionEntrySize
    $flags = Read-U32 ($header + 8)
    $address = Read-U32 ($header + 12)
    $size = Read-U32 ($header + 20)
    if (($flags -band 2) -ne 0 -and $address -ge 0x20000000) {
        $staticRam += $size
    }
}

$flashLimit = if ($FlashBytes -gt 0) { [uint64]$FlashBytes } else { [uint64]$FlashKiB * 1024 }
$ramLimit = [uint64]$RamKiB * 1024
$flashFree = [int64]$flashLimit - [int64]$flashUsed
$ramFree = [int64]$ramLimit - [int64]$staticRam

Write-Host ("Flash image: {0} / {1} bytes ({2} free)" -f $flashUsed, $flashLimit, $flashFree)
Write-Host ("Static RAM:  {0} / {1} bytes ({2} left for stack/runtime)" -f $staticRam, $ramLimit, $ramFree)

if ($flashFree -lt 0) { throw 'Firmware exceeds the configured application flash region.' }
if ($ramFree -lt 0) { throw 'Static allocations exceed MCU RAM.' }
