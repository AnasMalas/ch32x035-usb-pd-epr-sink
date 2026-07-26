[CmdletBinding()]
param(
    [switch]$NoBrowser,
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$packageScript = Join-Path $PSScriptRoot 'package.ps1'
$page = & $packageScript -Output $Output

Write-Host "USB-PD Sink Control was packaged as a standalone offline page:"
Write-Host $page

if (-not $NoBrowser) {
    Start-Process -FilePath $page
}
