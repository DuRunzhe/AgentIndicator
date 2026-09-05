# Install agent-status-indicator on Windows from a GitHub Release.
#
# Usage (run in PowerShell):
#   irm https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.ps1 | iex
#   # or pin a version:
#   $env:VERSION = '0.2.10'
#   irm https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.ps1 | iex
param(
  [string]$Version = $env:VERSION,
  [string]$Destination = "$env:LOCALAPPDATA\Programs\AgentStatusIndicator"
)

$ErrorActionPreference = 'Stop'
$repo = 'DuRunzhe/AgentIndicator'
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -notin @('AMD64', 'x86_64', 'ARM64')) {
  throw "暂不支持的 CPU 架构: $arch"
}
$target = if ($arch -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }

if (-not $Version -or $Version -eq 'latest') {
  $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
  $Version = $release.tag_name.TrimStart('v')
}

$base = "https://github.com/$repo/releases/download/v$Version"
$asset = "agent-status-indicator-$target.zip"
$zip = Join-Path $env:TEMP $asset

Write-Host "Downloading $asset (v$Version) ..."
Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip

# Verify against the bare-hex .sha256 sidecar published next to the asset.
try {
  $expected = (Invoke-WebRequest -Uri "$base/$asset.sha256" -UseBasicParsing).Content.Trim()
  $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
  if ($actual -ne $expected) { throw "SHA256 verification failed" }
  Write-Host "SHA256 verified"
} catch {
  Write-Warning "No .sha256 sidecar found; skipping verification"
}

New-Item -ItemType Directory -Force -Path $Destination | Out-Null
Expand-Archive -Path $zip -DestinationPath $Destination -Force

$paths = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($paths -split ';' -contains $Destination)) {
  $joined = if ($paths) { "$paths;$Destination" } else { $Destination }
  [Environment]::SetEnvironmentVariable('Path', $joined, 'User')
}

Write-Host "Installed: $Destination\agent-status-indicator.exe (v$Version)"
Write-Host "Open a NEW terminal, then run: agent-status-indicator --diagnose"
