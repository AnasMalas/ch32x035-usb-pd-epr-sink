[CmdletBinding()]
param(
    [string]$Port,
    [int]$BaudRate = 115200,
    [string[]]$Send,
    [ValidateRange(1, 60)]
    [int]$ListenSeconds = 3,
    [switch]$List
)

$ErrorActionPreference = 'Stop'
$ports = @([System.IO.Ports.SerialPort]::GetPortNames() | Sort-Object)

if ($List) {
    if ($ports.Count -eq 0) {
        Write-Host 'No serial ports found.'
    }
    else {
        $ports | ForEach-Object { Write-Host $_ }
    }
    return
}

if (-not $Port) {
    if ($ports.Count -eq 1) {
        $Port = $ports[0]
    }
    elseif ($ports.Count -eq 0) {
        throw 'No serial port found. Connect the running USB-console firmware and try again.'
    }
    else {
        throw "More than one serial port was found ($($ports -join ', ')). Select one with -Port COMx."
    }
}

$serial = [System.IO.Ports.SerialPort]::new($Port, $BaudRate)
$serial.NewLine = "`n"
$serial.DtrEnable = $true
$serial.RtsEnable = $false
$serial.ReadTimeout = 100
$serial.WriteTimeout = 1000

try {
    $serial.Open()
    Write-Host "Connected to $Port."

    if ($Send.Count -gt 0) {
        foreach ($line in $Send) {
            $serial.Write("$line`n")
        }

        $deadline = [DateTime]::UtcNow.AddSeconds($ListenSeconds)
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($serial.BytesToRead -gt 0) {
                Write-Host -NoNewline $serial.ReadExisting()
            }
            Start-Sleep -Milliseconds 20
        }
        return
    }

    $subscription = Register-ObjectEvent -InputObject $serial -EventName DataReceived -Action {
        Write-Host -NoNewline $Event.Sender.ReadExisting()
    }
    try {
        Write-Host 'Type a firmware command and press Enter. Type :quit to close the console.'
        while ($true) {
            $line = Read-Host
            if ($line -eq ':quit') {
                break
            }
            $serial.Write("$line`n")
        }
    }
    finally {
        Unregister-Event -SubscriptionId $subscription.SubscriptionId -ErrorAction SilentlyContinue
        Remove-Job -Id $subscription.Id -Force -ErrorAction SilentlyContinue
    }
}
finally {
    if ($serial.IsOpen) {
        $serial.Close()
    }
    $serial.Dispose()
}
