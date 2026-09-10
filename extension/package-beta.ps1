<#
    Assemble the handout for beta testers.

    One command, because a tester bundle assembled by hand is a bundle that
    eventually ships a stale XPI next to a fresh installer. Everything here is
    rebuilt from source every run.

    ASCII ONLY. Windows PowerShell 5.1 reads .ps1 as the system ANSI codepage,
    not UTF-8, so a smart quote or an em dash inside a string literal becomes
    mojibake and can break the parser outright.

    Usage:  .\package-beta.ps1
#>

[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path $PSScriptRoot 'dist\beta')
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent

function Step($text) { Write-Host "==> $text" -ForegroundColor Cyan }

# --- 1. Extension halves ------------------------------------------------------

Step 'Building the Chromium extension'
Push-Location $PSScriptRoot
try { node build-chromium.mjs | Out-Null } finally { Pop-Location }

Step 'Building the XPI'
& (Join-Path $PSScriptRoot 'build-xpi.ps1') | Out-Null

# --- 2. Chromium zip ----------------------------------------------------------

# Testers get a zip because "Load unpacked" needs a folder, and a folder is not
# something you can hand over. Entries are written by hand with forward slashes:
# under PS 5.1 both Compress-Archive and ZipFile.CreateFromDirectory write
# "icons\32.png", which some extractors turn into a single mangled filename.
Step 'Zipping the Chromium build'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Path $OutDir -Force | Out-Null }

$chromiumSrc = Join-Path $PSScriptRoot 'dist\chromium'
$chromiumZip = Join-Path $OutDir 'save-to-nova-chromium.zip'
if (Test-Path $chromiumZip) { Remove-Item $chromiumZip -Force }

$archive = [System.IO.Compression.ZipFile]::Open($chromiumZip, 'Create')
try {
    foreach ($file in Get-ChildItem $chromiumSrc -Recurse -File) {
        $relative = $file.FullName.Substring($chromiumSrc.Length + 1).Replace('\', '/')
        $entry = $archive.CreateEntry($relative, [System.IO.Compression.CompressionLevel]::Optimal)
        $writer = $entry.Open()
        $reader = [System.IO.File]::OpenRead($file.FullName)
        try { $reader.CopyTo($writer) } finally { $reader.Dispose(); $writer.Dispose() }
    }
} finally {
    $archive.Dispose()
}

# --- 3. Collect ---------------------------------------------------------------

Step 'Collecting'
Copy-Item (Join-Path $PSScriptRoot 'dist\save-to-nova.xpi') -Destination $OutDir -Force
Copy-Item (Join-Path $PSScriptRoot 'BETA-SETUP.md') -Destination $OutDir -Force

# The installer is not built here. It takes minutes, and a packaging script that
# silently triggers a full release build is a packaging script people stop
# running. Say plainly whether it is present and current.
$nsisDir = Join-Path $repo 'src-tauri\target\release\bundle\nsis'
$installer = Get-ChildItem $nsisDir -Filter '*-setup.exe' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1

if ($installer) {
    Copy-Item $installer.FullName -Destination $OutDir -Force
} else {
    Write-Host 'No installer found. Run: bun run tauri build' -ForegroundColor Yellow
}

# --- 4. Report ----------------------------------------------------------------

Write-Host ''
Write-Host "Bundle: $OutDir" -ForegroundColor Green
Get-ChildItem $OutDir -File | ForEach-Object {
    '{0,-40} {1,8:N0} KB' -f $_.Name, ($_.Length / 1KB)
}

if ($installer) {
    $age = (Get-Date) - $installer.LastWriteTime
    if ($age.TotalHours -gt 24) {
        Write-Host ''
        Write-Host ("Installer is {0:N0} h old. Rebuild if Nova changed since." -f $age.TotalHours) -ForegroundColor Yellow
    }
}
