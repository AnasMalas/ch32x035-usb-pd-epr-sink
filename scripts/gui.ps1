[CmdletBinding()]
param(
    # Retained for compatibility with older private-repository wrappers. The
    # serverless launcher no longer listens on a TCP port.
    [ValidateRange(1024, 65535)]
    [int]$Port = 8765,
    [switch]$NoBrowser,
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$packageScript = Join-Path $PSScriptRoot 'package-gui.ps1'
$page = & $packageScript -Output $Output

Write-Host "USB PD Control was packaged as a standalone offline page:"
Write-Host $page

if (-not $NoBrowser) {
    Start-Process -FilePath $page
}
