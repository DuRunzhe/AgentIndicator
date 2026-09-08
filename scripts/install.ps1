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

# Resolve the newest release automatically. VERSION is optional and only pins
# an older build; each source is tried in turn so a blocked or rate-limited
# endpoint never forces the caller to specify a version.
function Get-LatestVersion {
  # 1) GitHub REST API (authoritative, but anonymous calls are rate-limited)
  try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -TimeoutSec 15
    if ($release.tag_name) { return $release.tag_name.TrimStart('v') }
  } catch {}
  # 2) releases/latest Location redirect, which needs no API quota
  try {
    Invoke-WebRequest -Uri "https://github.com/$repo/releases/latest" -MaximumRedirection 0 -UseBasicParsing -ErrorAction Stop | Out-Null
  } catch {
    $response = $_.Exception.Response
    if ($response) {
      $location = [string]$response.Headers['Location']
      if ($location -match '/releases/tag/v?([^/]+)$') { return $Matches[1] }
    }
  }
  # 3) npm registry, which carries the same tagged release version
  try {
    return (Invoke-RestMethod -Uri 'https://registry.npmjs.org/agent-status-indicator/latest' -TimeoutSec 15).version
  } catch {}
  return $null
}
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -notin @('AMD64', 'x86_64', 'ARM64')) {
  throw "暂不支持的 CPU 架构: $arch"
}
$target = if ($arch -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }

if (-not $Version -or $Version -eq 'latest') {
  $Version = Get-LatestVersion
}
if (-not $Version) {
  throw "无法自动获取最新版本：GitHub API、releases 页面与 npm registry 均不可达。请检查网络后重试，或临时设置 `$env:VERSION = '0.2.14' 显式指定版本安装。"
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
