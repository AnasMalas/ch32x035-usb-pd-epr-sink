[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Firmware,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 1048576)]
    [uint32]$ApplicationFlashBytes
)

$ErrorActionPreference = 'Stop'
$firmwarePath = (Resolve-Path -LiteralPath $Firmware).Path
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

$entry = [uint64](Read-U32 24)
$programOffset = Read-U32 28
$programEntrySize = Read-U16 42
$programCount = Read-U16 44
$flashSegments = @()
$executableSegments = @()

for ($index = 0; $index -lt $programCount; $index++) {
    $header = $programOffset + $index * $programEntrySize
    $type = Read-U32 $header
    if ($type -ne 1) { continue }

    $physicalAddress = [uint64](Read-U32 ($header + 12))
    $fileSize = [uint64](Read-U32 ($header + 16))
    $flags = Read-U32 ($header + 24)
    if ($fileSize -eq 0 -or $physicalAddress -ge 0x20000000) { continue }

    $end = $physicalAddress + $fileSize
    $segment = [pscustomobject]@{
        Start = $physicalAddress
        End = $end
        Executable = ($flags -band 1) -ne 0
    }
    $flashSegments += $segment
    if ($segment.Executable) { $executableSegments += $segment }
}

if ($flashSegments.Count -eq 0) {
    throw 'ELF has no loadable application-flash segment.'
}
if ($executableSegments.Count -eq 0) {
    throw 'ELF has no executable application-flash segment.'
}

$firstExecutable = [uint64](($executableSegments | Measure-Object -Property Start -Minimum).Minimum)
if ($firstExecutable -ne 0) {
    throw ('Executable image starts at 0x{0:x8}, expected the CH32X035 zero-address boot alias.' -f $firstExecutable)
}
if ($entry -ge $ApplicationFlashBytes) {
    throw ('ELF entry 0x{0:x8} is outside application flash 0x00000000..0x{1:x8}.' -f
        $entry, ($ApplicationFlashBytes - 1))
}

foreach ($segment in $flashSegments) {
    if ($segment.End -gt $ApplicationFlashBytes) {
        throw ('Loadable flash segment 0x{0:x8}..0x{1:x8} is outside application flash 0x00000000..0x{2:x8}.' -f
            $segment.Start, ($segment.End - 1), ($ApplicationFlashBytes - 1))
    }
}

$lastFlashByte = [uint64](($flashSegments | Measure-Object -Property End -Maximum).Maximum) - 1
Write-Host ('ELF layout valid: entry=0x{0:x8}, executable-origin=0x{1:x8}, last-flash-byte=0x{2:x8}' -f
    $entry, $firstExecutable, $lastFlashByte)
