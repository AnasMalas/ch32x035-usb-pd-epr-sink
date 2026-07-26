[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$broken = [System.Collections.Generic.List[string]]::new()

Get-ChildItem -LiteralPath $workspace -Recurse -File -Filter '*.md' |
    Where-Object {
        $_.FullName -notmatch '[\\/](target|artifacts)[\\/]'
    } |
    ForEach-Object {
        $document = $_
        $text = [System.IO.File]::ReadAllText($document.FullName)
        foreach ($match in [regex]::Matches($text, '\[[^\]]*\]\(([^)]+)\)')) {
            $link = $match.Groups[1].Value.Trim()
            if ($link -match '^(https?://|mailto:|#)') {
                continue
            }

            $path = ($link -split '#', 2)[0]
            if ([string]::IsNullOrWhiteSpace($path)) {
                continue
            }

            $resolved = [System.IO.Path]::GetFullPath((Join-Path $document.DirectoryName $path))
            if (-not (Test-Path -LiteralPath $resolved)) {
                $relativeDocument = $document.FullName.Substring($workspace.Length).TrimStart('\', '/')
                $broken.Add("${relativeDocument}: $link")
            }
        }
    }

if ($broken.Count -ne 0) {
    throw "Broken local Markdown links:`n$($broken -join "`n")"
}

Write-Host 'Local Markdown links are valid.'
