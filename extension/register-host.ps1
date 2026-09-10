<#
.SYNOPSIS
  Register nova-connector as a native messaging host so the browser can start
  Nova on demand.

.DESCRIPTION
  This is the whole reason waking Nova needs no confirmation dialog. The trust is
  established HERE, once, by writing a registry key that names both the connector
  binary and the extension id allowed to call it - not by the user clicking
  through a prompt every time, which is what the `nova://` protocol-handler
  alternative would have required.

  Everything is written under HKCU, so no administrator rights are needed and
  nothing is changed for other users of the machine.

  Zen is a Firefox fork and reports Vendor=Mozilla in its application.ini, so it
  reads the standard Mozilla host path - the same one Firefox itself uses. One
  registration covers both.

.PARAMETER ConnectorPath
  Path to nova-connector.exe. Defaults to the debug build beside this repo.

.PARAMETER Unregister
  Remove the registration instead of adding it.
#>

[CmdletBinding()]
param(
    [string]$ConnectorPath,
    [switch]$Unregister
)

$ErrorActionPreference = 'Stop'

$HostName  = 'com.nova.connector'
$ExtensionId = 'nova@local'
$RegPath   = "HKCU:\Software\Mozilla\NativeMessagingHosts\$HostName"
$ManifestPath = Join-Path $PSScriptRoot "$HostName.json"

if ($Unregister) {
    if (Test-Path $RegPath) {
        Remove-Item $RegPath -Force
        Write-Host "Removed $RegPath"
    } else {
        Write-Host "Nothing registered at $RegPath"
    }
    if (Test-Path $ManifestPath) {
        Remove-Item $ManifestPath -Force
        Write-Host "Removed $ManifestPath"
    }
    return
}

# Prefer the RELEASE build. A debug Tauri binary loads its frontend from the Vite
# dev server (localhost:1420), so launching one without `bun run dev` running
# shows "localhost refused to connect" instead of the app. Release embeds the
# frontend and stands alone, which is what you want for actually using this.
if (-not $ConnectorPath) {
    $release = Join-Path $PSScriptRoot '..\src-tauri\target\release\nova-connector.exe'
    $debug   = Join-Path $PSScriptRoot '..\src-tauri\target\debug\nova-connector.exe'
    $ConnectorPath = if (Test-Path $release) { $release } else { $debug }
}
$ConnectorPath = [System.IO.Path]::GetFullPath($ConnectorPath)

if (-not (Test-Path $ConnectorPath)) {
    throw "nova-connector.exe not found at $ConnectorPath. Build it with: cargo build --bin nova-connector"
}

# The connector starts Nova by looking for it NEXT TO ITSELF, so a connector
# without nova.exe beside it can register successfully and then fail at the one
# moment it matters. Catch that here instead.
$novaExe = Join-Path (Split-Path $ConnectorPath -Parent) 'nova.exe'
if (-not (Test-Path $novaExe)) {
    Write-Warning "nova.exe is not beside the connector ($novaExe). Waking will fail until it is."
}

$manifest = [ordered]@{
    name        = $HostName
    description = 'Starts Nova so the browser extension can save into it'
    path        = $ConnectorPath
    type        = 'stdio'
    # Firefox uses `allowed_extensions` with the gecko id. (Chromium uses
    # `allowed_origins` with a chrome-extension:// URL - that variant belongs in
    # the Chromium registration, not here.)
    allowed_extensions = @($ExtensionId)
}

# Written WITHOUT a byte-order mark. `Set-Content -Encoding utf8` on Windows
# PowerShell 5.1 emits UTF-8 *with* a BOM, and a BOM makes the manifest fail to
# parse - the browser then reports the host as simply not existing, which sends
# you looking at the registry key instead of the file it points to.
$json = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText($ManifestPath, $json, (New-Object System.Text.UTF8Encoding $false))
Write-Host "Wrote  $ManifestPath"

New-Item -Path $RegPath -Force | Out-Null
# The DEFAULT value of the key is the manifest path; the manifest then points at
# the executable. Firefox will not look anywhere else.
Set-ItemProperty -Path $RegPath -Name '(Default)' -Value $ManifestPath
Write-Host "Wrote  $RegPath"

Write-Host ''
Write-Host 'Registered. Next:' -ForegroundColor Green
Write-Host '  1. Open about:debugging#/runtime/this-firefox in Zen'
Write-Host '  2. Load Temporary Add-on... and pick extension\manifest.json'
Write-Host '  3. Open Nova, then use the extension popup to Pair'
