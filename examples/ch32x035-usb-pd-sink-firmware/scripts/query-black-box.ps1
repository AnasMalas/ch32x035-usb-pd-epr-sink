[CmdletBinding()]
param(
    [string]$Port,
    [int]$TimeoutSeconds = 10,
    [switch]$Arm,
    [ValidateSet('Protocol', 'Link')]
    [string]$TraceLevel = 'Protocol'
)

$ErrorActionPreference = 'Stop'

function Find-ReferencePort {
    if ($Port) { return $Port }
    $available = [System.IO.Ports.SerialPort]::GetPortNames()
    $usbKey = 'HKLM:\SYSTEM\CurrentControlSet\Enum\USB\VID_1A86&PID_FE0C'
    if (Test-Path $usbKey) {
        foreach ($instance in Get-ChildItem $usbKey -ErrorAction SilentlyContinue) {
            $parameters = Join-Path $instance.PSPath 'Device Parameters'
            $candidate = (Get-ItemProperty $parameters -Name PortName -ErrorAction SilentlyContinue).PortName
            if ($candidate -and $available -contains $candidate) { return $candidate }
        }
    }
    return $null
}

function Read-BlackBoxPage([System.IO.Ports.SerialPort]$Serial, [byte]$Page) {
    $Serial.DiscardInBuffer()
    [byte[]]$request = 0x42, 0x42, $Page
    $Serial.Write($request, 0, $request.Length)
    $bytes = [System.Collections.Generic.List[byte]]::new()
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        while ($Serial.BytesToRead -gt 0) {
            $buffer = [byte[]]::new([Math]::Min(64, $Serial.BytesToRead))
            $read = $Serial.Read($buffer, 0, $buffer.Length)
            for ($index = 0; $index -lt $read; $index++) { $bytes.Add($buffer[$index]) }
        }
        while ($bytes.Count -ge 4 -and
            ($bytes[0] -ne 0x50 -or $bytes[1] -ne 0x44 -or $bytes[2] -ne 0x42 -or $bytes[3] -ne 0x42)) {
            $bytes.RemoveAt(0)
        }
        if ($bytes.Count -ge 6) {
            $length = [int]$bytes[5]
            if ($length -gt 16) { throw "Invalid black-box response length $length." }
            if ($bytes.Count -ge 6 + $length) {
                return [pscustomobject]@{
                    Kind = $bytes[4]
                    Payload = if ($length) { [byte[]]$bytes.GetRange(6, $length).ToArray() } else { [byte[]]::new(0) }
                }
            }
        }
        Start-Sleep -Milliseconds 10
    }
    throw "Timed out reading black-box page $Page. Flash a black-box profile first."
}

function Lookup($Table, [int]$Value, [string]$Fallback) {
    if ($Table.ContainsKey($Value)) { return $Table[$Value] }
    return "$Fallback($Value)"
}

$hardResetReasons = @{
    0='source-signaled'; 1='invalid-source-capabilities'; 2='soft-reset-failed'
    3='source-capabilities-timeout'; 4='request-response-timeout'; 5='power-transition-failure'
    6='EPR-capabilities-timeout'; 7='EPR-protocol-error'; 8='EPR-keepalive-failed'; 255='unspecified'
}
$applicationEvents = @{
    1='hard-reset'; 2='PHY-reset-failed'; 3='PHY-unstable'; 4='partner-timeout'
    5='protocol-recovery'; 6='terminal'; 7='EPR-entry-failed'; 8='VBUS-detector-low'
    9='power-fail-sample'
}
$vbusDetectorPaths = @{ 0='loop-observed-low'; 1='active-low-edge'; 2='output-enable-race' }
$txReasons = @{
    1='driver-discarded'; 2='GoodCRC-timeout'; 3='hard-reset'; 4='detached'
    5='retries-exceeded'; 6='acknowledge-mismatch'; 255='other'
}
$protocolErrors = @{
    1='RX-discarded'; 2='RX-detached'; 3='RX-soft-reset'; 4='RX-hard-reset'; 5='RX-timeout'
    6='RX-unsupported'; 7='RX-parse'; 8='RX-acknowledge-mismatch'; 9='TX-discarded'
    10='TX-detached'; 11='TX-hard-reset'; 12='TX-validation'; 13='TX-retries-exceeded'
    14='unexpected-message'
}
$hardResetPhases = @{ 1='received'; 2='transmit-start'; 3='transmit-retry'; 4='transmit-complete'; 5='transmit-failure' }
$keepAlivePhases = @{ 1='request'; 2='acknowledged'; 3='timeout'; 4='unexpected-response'; 5='protocol-failure' }
$paths = @{ 0='software'; 1='hardware' }
$eventNames = @{
    1='TX-start'; 2='GoodCRC-wait'; 3='GoodCRC-received'; 4='TX-hardware-retry'
    5='TX-retry'; 6='TX-success'; 7='TX-failure'; 8='RX-message'
    9='GoodCRC-transmitted'; 10='RX-retransmission'; 11='protocol-error'
    12='hard-reset'; 13='EPR-keepalive'
}
$controlMessages = @{
    1='GoodCRC'; 2='GotoMin'; 3='Accept'; 4='Reject'; 5='Ping'; 6='PS_RDY'
    7='Get_Source_Cap'; 8='Get_Sink_Cap'; 9='DR_Swap'; 10='PR_Swap'; 11='VCONN_Swap'
    12='Wait'; 13='Soft_Reset'; 14='Data_Reset'; 15='Data_Reset_Complete'; 16='Not_Supported'
    17='Get_Source_Cap_Extended'; 18='Get_Status'; 19='FR_Swap'; 20='Get_PPS_Status'
    21='Get_Country_Codes'; 22='Get_Sink_Cap_Extended'; 23='Get_Source_Info'; 24='Get_Revision'
}
$dataMessages = @{
    1='Source_Capabilities'; 2='Request'; 3='BIST'; 4='Sink_Capabilities'; 5='Battery_Status'
    6='Alert'; 7='Get_Country_Info'; 8='Enter_USB'; 9='EPR_Request'; 10='EPR_Mode'
    11='Source_Info'; 12='Revision'
}
$extendedMessages = @{
    1='Source_Capabilities_Extended'; 2='Status'; 3='Get_Battery_Cap'; 4='Get_Battery_Status'
    5='Battery_Capabilities'; 6='Get_Manufacturer_Info'; 7='Manufacturer_Info'
    8='Security_Request'; 9='Security_Response'; 10='Firmware_Update_Request'
    11='Firmware_Update_Response'; 12='PPS_Status'; 13='Country_Info'; 14='Country_Codes'
    15='Sink_Capabilities_Extended'; 16='Extended_Control'; 17='EPR_Source_Capabilities'
    18='EPR_Sink_Capabilities'
}

function Describe-Header([uint16]$Header) {
    if ($Header -eq [uint16]::MaxValue) { return 'header=n/a' }
    $type = $Header -band 0x1f
    $count = ($Header -shr 12) -band 7
    $extended = ($Header -band 0x8000) -ne 0
    $message = if ($extended) {
        Lookup $extendedMessages $type 'extended'
    } elseif ($count -eq 0) {
        Lookup $controlMessages $type 'control'
    } else {
        Lookup $dataMessages $type 'data'
    }
    return ('{0}, id={1}, objects={2}, header=0x{3:x4}' -f $message, (($Header -shr 9) -band 7), $count, $Header)
}

function Describe-Event([byte[]]$Event, [int]$Index) {
    $uptime = [BitConverter]::ToUInt32($Event, 0)
    $kind = [int]$Event[4]
    $code = [int]$Event[5]
    $messageId = [int]$Event[6]
    $counter = [int]$Event[7]
    $header = [BitConverter]::ToUInt16($Event, 8)
    $detail = [BitConverter]::ToUInt16($Event, 10)

    if (($kind -band 0x80) -ne 0) {
        $applicationKind = $kind -band 0x7f
        if ($applicationKind -eq 1) {
            $direction = if (($code -band 0x80) -ne 0) { 'sent' } else { 'received' }
            $reason = Lookup $hardResetReasons ($code -band 0x7f) 'reason'
            return ('{0,2}: t={1}ms APP hard-reset {2}; reason={3}; recovery={4}ms; VBUS-present={5}' -f `
                ($Index + 1), $uptime, $direction, $reason, $detail, [bool]($messageId -band 1))
        }
        if ($applicationKind -eq 8) {
            $context = [uint32]$header -bor ([uint32]$detail -shl 16)
            return ('{0,2}: t={1}ms APP VBUS-detector-low; path={2}; qualified-before={3}' -f `
                ($Index + 1), $uptime, (Lookup $vbusDetectorPaths $code 'path'), [bool]($context -band 1))
        }
        if ($applicationKind -eq 9) {
            $context = [uint32]$header -bor ([uint32]$detail -shl 16)
            return ('{0,2}: t={1}ms APP power-fail-sample; detector-low={2}; EXTI1-pending={3}; EXTI1-armed={4}; qualified-before={5}; GPIOB-IN=0x{6:x4}; EXTI-pending=0x{7:x4}' -f `
                ($Index + 1), $uptime, [bool]($code -band 1), [bool]($code -band 2), `
                [bool]($code -band 4), [bool]($code -band 8), ($context -band 0xffff), (($context -shr 16) -band 0xffff))
        }
        $name = Lookup $applicationEvents $applicationKind 'event'
        $context = [uint32]$header -bor ([uint32]$detail -shl 16)
        return ('{0,2}: t={1}ms APP {2}; code={3}; context={4}' -f ($Index + 1), $uptime, $name, $code, $context)
    }

    $name = Lookup $eventNames $kind 'event'
    $description = switch ($kind) {
        { $_ -in 2, 3, 4, 6, 9 } { 'path=' + (Lookup $paths $code 'path'); break }
        5 { 'reason=' + (Lookup $txReasons $code 'reason'); break }
        7 { 'reason=' + (Lookup $txReasons $code 'reason'); break }
        11 { 'error=' + (Lookup $protocolErrors $code 'error') + "; detail=$detail"; break }
        12 { 'phase=' + (Lookup $hardResetPhases $code 'phase') + '; reason=' + (Lookup $hardResetReasons $detail 'reason'); break }
        13 { 'phase=' + (Lookup $keepAlivePhases $code 'phase'); break }
        default { Describe-Header $header }
    }
    $wire = if ($kind -in 1, 2, 3, 4, 5, 6, 7, 8, 9, 10) { '; ' + (Describe-Header $header) } else { '' }
    $retry = if ($counter -ne 255) { "; counter=$counter" } else { '' }
    return ('{0,2}: t={1}ms {2}; {3}{4}{5}' -f ($Index + 1), $uptime, $name, $description, $retry, $wire)
}

$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
do {
    $portName = Find-ReferencePort
    if ($portName) { break }
    Start-Sleep -Milliseconds 200
} while ([DateTime]::UtcNow -lt $deadline)
if (-not $portName) { throw 'The reference USB serial port was not found.' }

$serial = [System.IO.Ports.SerialPort]::new($portName, 115200, 'None', 8, 'One')
$serial.DtrEnable = $false
$serial.RtsEnable = $false
$serial.ReadTimeout = 100
$serial.Open()
try {
    # Force a complete CDC session boundary even if the previous host left
    # DTR asserted or abandoned an IN transfer while closing.
    Start-Sleep -Milliseconds 50
    $serial.DtrEnable = $true
    Start-Sleep -Milliseconds 150
    $requestPage = if (-not $Arm) {
        [byte]0
    } elseif ($TraceLevel -eq 'Link') {
        [byte]0xff
    } else {
        [byte]0xfe
    }
    $summaryResponse = Read-BlackBoxPage $serial $requestPage
    if ($summaryResponse.Kind -ne 0xb0 -or $summaryResponse.Payload.Length -ne 14) {
        throw 'The device returned an invalid black-box summary.'
    }
    $summary = $summaryResponse.Payload
    $stateFlags = $summary[1]
    $logFlags = $summary[3]
    $storedTraceLevel = if ($summary[13] -ne 2) { 'n/a' } elseif ($logFlags -band 0x10) { 'link' } else { 'protocol' }
    $profile = if ($summary[13] -eq 2) { "deep numeric ($storedTraceLevel)" } else { 'high-level' }
    Write-Host "Connected to $portName; profile=$profile"
    Write-Host ('Generation {0}; valid={1}, restored={2}, dirty={3}, write-error={4}; active-page={5}, prepared-page={6}' -f `
        [BitConverter]::ToUInt32($summary, 4), [bool]($stateFlags -band 1), [bool]($stateFlags -band 2), `
        [bool]($stateFlags -band 4), [bool]($stateFlags -band 8), $summary[11], $summary[12])
    Write-Host ('Black box: {0} events; next sequence={1}; ABI={2}; trace ABI={3}; hard-reset={4}, frozen={5}, overwritten={6}' -f `
        $summary[2], [BitConverter]::ToUInt16($summary, 8), $summary[0], $summary[10], `
        [bool]($logFlags -band 1), [bool]($logFlags -band 2), [bool]($logFlags -band 4))

    if ($Arm) {
        if ($summary[12] -eq 255 -or ($stateFlags -band 8)) {
            throw 'The device could not prepare a black-box journal page.'
        }
        Write-Host "Black box cleared and armed for the next incident; trace level=$storedTraceLevel."
        return
    }

    for ($index = 0; $index -lt [int]$summary[2]; $index++) {
        $response = Read-BlackBoxPage $serial ([byte]($index + 1))
        if ($response.Kind -ne 0xb1 -or $response.Payload.Length -ne 16 -or $response.Payload[2] -ne 1) {
            throw "The device returned an invalid event page for index $index."
        }
        [byte[]]$event = $response.Payload[4..15]
        Write-Host (Describe-Event $event $index)
    }
}
finally {
    if ($serial.IsOpen) {
        $serial.DtrEnable = $false
        $serial.RtsEnable = $false
        $serial.Close()
    }
    $serial.Dispose()
}
