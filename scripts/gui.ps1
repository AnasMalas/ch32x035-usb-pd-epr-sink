[CmdletBinding()]
param(
    [ValidateRange(1024, 65535)]
    [int]$Port = 8765,
    [switch]$NoBrowser
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$root = [System.IO.Path]::GetFullPath((Join-Path $workspace 'tools\pd-control'))

if (-not (Test-Path -LiteralPath (Join-Path $root 'index.html'))) {
    throw "USB PD Control was not found at $root."
}

$mimeTypes = @{
    '.css'  = 'text/css; charset=utf-8'
    '.html' = 'text/html; charset=utf-8'
    '.js'   = 'text/javascript; charset=utf-8'
    '.json' = 'application/json; charset=utf-8'
    '.svg'  = 'image/svg+xml'
}

function Send-Response {
    param(
        [Parameter(Mandatory)]
        [System.Net.Sockets.NetworkStream]$Stream,
        [Parameter(Mandatory)]
        [int]$Status,
        [Parameter(Mandatory)]
        [string]$Reason,
        [Parameter(Mandatory)]
        [byte[]]$Body,
        [Parameter(Mandatory)]
        [string]$ContentType,
        [switch]$HeadersOnly
    )

    $header = "HTTP/1.1 $Status $Reason`r`n" +
        "Content-Type: $ContentType`r`n" +
        "Content-Length: $($Body.Length)`r`n" +
        "Cache-Control: no-store`r`n" +
        "X-Content-Type-Options: nosniff`r`n" +
        "Connection: close`r`n`r`n"
    $headerBytes = [System.Text.Encoding]::ASCII.GetBytes($header)
    $Stream.Write($headerBytes, 0, $headerBytes.Length)
    if (-not $HeadersOnly -and $Body.Length -gt 0) {
        $Stream.Write($Body, 0, $Body.Length)
    }
    $Stream.Flush()
}

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$url = "http://127.0.0.1:$Port/"

try {
    $listener.Start()
    Write-Host "USB PD Control is available at $url"
    Write-Host 'Press Ctrl+C to stop the local server.'
    if (-not $NoBrowser) {
        Start-Process $url
    }

    while ($true) {
        # AcceptTcpClient() blocks inside .NET and can prevent Windows PowerShell
        # from observing Ctrl+C. Poll first so control regularly returns to the
        # PowerShell pipeline even when no browser is connected.
        while (-not $listener.Pending()) {
            Start-Sleep -Milliseconds 100
        }

        $client = $listener.AcceptTcpClient()
        $stream = $null
        $reader = $null
        try {
            $client.ReceiveTimeout = 5000
            $stream = $client.GetStream()
            $reader = [System.IO.StreamReader]::new(
                $stream,
                [System.Text.Encoding]::ASCII,
                $false,
                1024,
                $true
            )

            $requestLine = $reader.ReadLine()
            if ([string]::IsNullOrWhiteSpace($requestLine)) {
                continue
            }

            while (-not [string]::IsNullOrEmpty($reader.ReadLine())) {}
            $parts = $requestLine.Split(' ')
            if ($parts.Count -lt 2 -or $parts[0] -notin @('GET', 'HEAD')) {
                Send-Response -Stream $stream -Status 405 -Reason 'Method Not Allowed' `
                    -Body ([System.Text.Encoding]::UTF8.GetBytes('Method not allowed.')) `
                    -ContentType 'text/plain; charset=utf-8'
                continue
            }

            $requestPath = $parts[1].Split('?')[0]
            $relativePath = [System.Uri]::UnescapeDataString($requestPath).TrimStart('/').Replace('/', '\')
            if ([string]::IsNullOrWhiteSpace($relativePath)) {
                $relativePath = 'index.html'
            }

            $fullPath = [System.IO.Path]::GetFullPath((Join-Path $root $relativePath))
            $rootPrefix = $root.TrimEnd('\') + '\'
            if (-not $fullPath.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                Send-Response -Stream $stream -Status 403 -Reason 'Forbidden' `
                    -Body ([System.Text.Encoding]::UTF8.GetBytes('Forbidden.')) `
                    -ContentType 'text/plain; charset=utf-8'
                continue
            }

            if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
                Send-Response -Stream $stream -Status 404 -Reason 'Not Found' `
                    -Body ([System.Text.Encoding]::UTF8.GetBytes('Not found.')) `
                    -ContentType 'text/plain; charset=utf-8'
                continue
            }

            $body = [System.IO.File]::ReadAllBytes($fullPath)
            $extension = [System.IO.Path]::GetExtension($fullPath).ToLowerInvariant()
            $contentType = if ($mimeTypes.ContainsKey($extension)) {
                $mimeTypes[$extension]
            }
            else {
                'application/octet-stream'
            }
            Send-Response -Stream $stream -Status 200 -Reason 'OK' -Body $body `
                -ContentType $contentType -HeadersOnly:($parts[0] -eq 'HEAD')
        }
        catch [System.Management.Automation.PipelineStoppedException] {
            throw
        }
        catch {
            Write-Warning $_.Exception.Message
        }
        finally {
            if ($reader) { $reader.Dispose() }
            if ($stream) { $stream.Dispose() }
            $client.Dispose()
        }
    }
}
finally {
    $listener.Stop()
}
