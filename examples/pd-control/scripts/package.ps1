[CmdletBinding()]
param(
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$source = Split-Path -Parent $PSScriptRoot
$examples = Split-Path -Parent $source

if ([string]::IsNullOrWhiteSpace($Output)) {
    $Output = Join-Path (Join-Path $examples 'artifacts') 'usb-pd-control.html'
}

$index = [System.IO.File]::ReadAllText((Join-Path $source 'index.html'))
$styles = [System.IO.File]::ReadAllText((Join-Path $source 'styles.css'))
$protocol = [System.IO.File]::ReadAllText((Join-Path $source 'protocol.js'))
$application = [System.IO.File]::ReadAllText((Join-Path $source 'app.js'))

$externalPolicy = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"
$inlinePolicy = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"

if (-not $index.Contains($externalPolicy)) {
    throw 'The GUI Content Security Policy did not match the package script.'
}

$index = $index.Replace($externalPolicy, $inlinePolicy)
$index = $index.Replace(
    '    <link rel="stylesheet" href="styles.css">',
    "    <style>`n$styles`n    </style>"
)
$index = $index.Replace(
    '    <script src="protocol.js" defer></script>',
    ''
)
$index = $index.Replace(
    '    <script src="app.js" defer></script>',
    ''
)
$inlineScripts = "    <script>`n$protocol`n    </script>`n    <script>`n$application`n    </script>`n"
$index = $index.Replace('  </body>', "$inlineScripts  </body>")

if ($index.Contains('href="styles.css"') -or
    $index.Contains('src="protocol.js"') -or
    $index.Contains('src="app.js"')) {
    throw 'The GUI package still contains an external application asset.'
}

$fullOutput = [System.IO.Path]::GetFullPath($Output)
$outputDirectory = Split-Path -Parent $fullOutput
[System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
$utf8 = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($fullOutput, $index, $utf8)

Write-Output $fullOutput
