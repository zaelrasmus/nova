<#
.SYNOPSIS
  Package the Firefox/Zen extension as an .xpi.

.DESCRIPTION
  An .xpi is just a zip with manifest.json at its ROOT - not inside a folder, which
  is the single most common way a hand-rolled package fails to install.

  What you do with the result depends on whether your browser enforces extension
  signing:

    * If it does NOT (many Firefox forks ship with enforcement off, and
      Developer Edition / Nightly / ESR honour the about:config pref), you can
      install this file directly and permanently.

    * If it DOES - stock Firefox release ignores the pref entirely - the file
      has to be signed by Mozilla first. Use the UNLISTED flow, which signs it
      without publishing it anywhere:

          npx web-ext sign --channel=unlisted --api-key=... --api-secret=...

      That returns a signed .xpi you install and keep. "Unlisted" means exactly
      what it says: not in the store, not searchable, not public. Signing does
      not make the extension any less local - it still only ever talks to
      127.0.0.1.

  Either way nothing about how the extension WORKS changes. This is purely about
  the browser being willing to keep it installed across restarts.
#>

[CmdletBinding()]
param(
    [string]$OutFile
)

$ErrorActionPreference = 'Stop'

if (-not $OutFile) {
    $OutFile = Join-Path $PSScriptRoot 'dist\save-to-nova.xpi'
}

$manifestPath = Join-Path $PSScriptRoot 'manifest.json'
if (-not (Test-Path $manifestPath)) { throw "manifest.json not found next to this script." }
$manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json

# Everything the extension needs at runtime. Listed explicitly rather than
# excluding: a package that accidentally ships the signing key or a stale dist\
# is worse than one that is missing a file you will notice immediately.
$include = @('manifest.json', 'background.js', 'popup.html', 'popup.js', 'popup.css', 'icons')

$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("nova-xpi-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $staging -Force | Out-Null
try {
    foreach ($item in $include) {
        $source = Join-Path $PSScriptRoot $item
        if (-not (Test-Path $source)) { throw "Missing $item" }
        Copy-Item $source -Destination $staging -Recurse -Force
    }

    $outDir = Split-Path $OutFile -Parent
    if ($outDir -and -not (Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
    if (Test-Path $OutFile) { Remove-Item $OutFile -Force }

    # Entries are written BY HAND with forward slashes, and that is not fussiness.
    # The ZIP format requires '/' as the separator, but under Windows PowerShell
    # both Compress-Archive and ZipFile.CreateFromDirectory (which is .NET
    # Framework here, not .NET Core) write "icons\32.png" with a backslash. An
    # archive built that way installs as a corrupt add-on, and the error says
    # nothing about separators.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem

    $archive = [System.IO.Compression.ZipFile]::Open($OutFile, 'Create')
    try {
        foreach ($file in Get-ChildItem $staging -Recurse -File) {
            $relative = $file.FullName.Substring($staging.Length + 1).Replace('\', '/')
            $entry = $archive.CreateEntry($relative, [System.IO.Compression.CompressionLevel]::Optimal)
            $writer = $entry.Open()
            $reader = [System.IO.File]::OpenRead($file.FullName)
            try { $reader.CopyTo($writer) } finally { $reader.Dispose(); $writer.Dispose() }
        }
    }
    finally {
        $archive.Dispose()
    }
}
finally {
    Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
}

$size = [math]::Round((Get-Item $OutFile).Length / 1KB, 1)
Write-Host "Built  $OutFile  ($size KB)"
Write-Host "Extension id: $($manifest.browser_specific_settings.gecko.id)"
Write-Host ''
Write-Host 'To install WITHOUT signing, the browser must allow unsigned add-ons:' -ForegroundColor Green
Write-Host '  1. about:config -> xpinstall.signatures.required -> false'
Write-Host '  2. about:addons -> gear icon -> Install Add-on From File... -> pick the .xpi'
Write-Host ''
Write-Host 'If step 2 is refused, the build enforces signing and the pref is ignored.'
Write-Host 'Sign it unlisted instead (it stays private):'
Write-Host '  npx web-ext sign --channel=unlisted --api-key=<key> --api-secret=<secret>'
