<#
.SYNOPSIS
  Register nova-connector as a native messaging host for Chromium browsers.

.DESCRIPTION
  The Firefox counterpart of this script (register-host.ps1) writes one key,
  because every Gecko browser reads the Mozilla path. Chromium browsers do not
  share: each vendor looks under its OWN key, so Brave, Helium, Chrome and
  Chromium each need their own registration. All are written; a key for a browser
  that is not installed is harmless and means it works the day it is.

  The manifest format differs from Firefox's too. Chromium matches on
  `allowed_origins` with a chrome-extension:// URL, where Firefox matches on
  `allowed_extensions` with a gecko id - which is why the two cannot share one
  file even though they name the same executable.

  Everything is under HKCU, so no administrator rights are required.

.PARAMETER ConnectorPath
  Path to nova-connector.exe. Defaults to the debug build.

.PARAMETER Unregister
  Remove the registrations instead of adding them.
#>

[CmdletBinding()]
param(
    [string]$ConnectorPath,
    [switch]$Unregister
)

$ErrorActionPreference = 'Stop'

$HostName = 'com.nova.connector'
$ManifestPath = Join-Path $PSScriptRoot "$HostName.chromium.json"

# Vendor\Product differs per browser; there is no shared Chromium path.
$Browsers = [ordered]@{
    'Brave'    = 'HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts'
    'Helium'   = 'HKCU:\Software\Helium\Helium\NativeMessagingHosts'
    'Chrome'   = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts'
    'Chromium' = 'HKCU:\Software\Chromium\NativeMessagingHosts'
}

if ($Unregister) {
    foreach ($name in $Browsers.Keys) {
        $key = Join-Path $Browsers[$name] $HostName
        if (Test-Path $key) {
            Remove-Item $key -Force
            Write-Host "Removed  $name"
        }
    }
    if (Test-Path $ManifestPath) { Remove-Item $ManifestPath -Force }
    Write-Host 'Unregistered.'
    return
}

# Prefer the RELEASE build - see the note in register-host.ps1. A debug Tauri
# binary needs the Vite dev server running or its window shows nothing.
if (-not $ConnectorPath) {
    $release = Join-Path $PSScriptRoot '..\src-tauri\target\release\nova-connector.exe'
    $debug   = Join-Path $PSScriptRoot '..\src-tauri\target\debug\nova-connector.exe'
    $ConnectorPath = if (Test-Path $release) { $release } else { $debug }
}
$ConnectorPath = [System.IO.Path]::GetFullPath($ConnectorPath)

if (-not (Test-Path $ConnectorPath)) {
    throw "nova-connector.exe not found at $ConnectorPath. Build it with: cargo build --bin nova-connector"
}

$novaExe = Join-Path (Split-Path $ConnectorPath -Parent) 'nova.exe'
if (-not (Test-Path $novaExe)) {
    Write-Warning "nova.exe is not beside the connector ($novaExe). Waking will fail until it is."
}

# The id is pinned by the `key` field in manifest.chromium.json, so it is stable
# across machines and reinstalls - unlike Firefox, where the origin is random per
# installation and can only be learned at pair time.
$chromiumManifest = Join-Path $PSScriptRoot 'manifest.chromium.json'
if (-not (Test-Path $chromiumManifest)) {
    throw "manifest.chromium.json not found. Run: node build-chromium.mjs"
}
$idFile = Join-Path $PSScriptRoot '.chromium-id.txt'
if (-not (Test-Path $idFile)) {
    throw ".chromium-id.txt not found - it carries the id derived from the manifest key."
}
$ExtensionId = (Get-Content $idFile -TotalCount 1).Trim()

$manifest = [ordered]@{
    name        = $HostName
    description = 'Starts Nova so the browser extension can save into it'
    path        = $ConnectorPath
    type        = 'stdio'
    allowed_origins = @("chrome-extension://$ExtensionId/")
}

# WITHOUT a BOM. Windows PowerShell's `Set-Content -Encoding utf8` writes one,
# and a BOM makes the manifest fail to parse - which the browser then reports as
# the host simply not existing, sending you to look at the registry instead of
# the file it points at.
$json = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText($ManifestPath, $json, (New-Object System.Text.UTF8Encoding $false))
Write-Host "Wrote  $ManifestPath"
Write-Host "Extension id: $ExtensionId"
Write-Host ''

foreach ($name in $Browsers.Keys) {
    $key = Join-Path $Browsers[$name] $HostName
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name '(Default)' -Value $ManifestPath
    Write-Host "Registered  $name"
}

Write-Host ''
Write-Host 'Next:' -ForegroundColor Green
Write-Host '  1. node build-chromium.mjs'
Write-Host '  2. Open chrome://extensions (or brave://extensions) -> enable Developer mode'
Write-Host '  3. Load unpacked -> pick extension\dist\chromium'
Write-Host '  4. Open Nova, then use the extension popup to Pair'
