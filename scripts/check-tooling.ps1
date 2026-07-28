[CmdletBinding()]
param(
    [string]$WorkflowPath
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$workflowPath = if ([string]::IsNullOrWhiteSpace($WorkflowPath)) {
    Join-Path $workspace '.github/workflows/ci.yml'
}
elseif ([System.IO.Path]::IsPathRooted($WorkflowPath)) {
    $WorkflowPath
}
else {
    Join-Path $workspace $WorkflowPath
}
$workflow = [System.IO.File]::ReadAllText($workflowPath)

$delegatesToCheckScript = [regex]::IsMatch(
    $workflow,
    '(?m)^\s*run:\s+\./scripts/check\.ps1(?:\s+.*?)?-RequireNode\s*$'
)
if (-not $delegatesToCheckScript) {
    throw 'Repository CI must delegate to scripts/check.ps1 with -RequireNode.'
}

$duplicatesCargoChecks = [regex]::IsMatch(
    $workflow,
    '(?im)^\s*(?:run:\s*)?cargo\s+(?:fmt|clippy|test|build)\b'
)
if ($duplicatesCargoChecks) {
    throw 'Repository CI must not duplicate Cargo checks from scripts/check.ps1.'
}

Write-Host 'CI tooling contract is valid.'
