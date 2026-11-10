# Build signed installers + latest.json for a new DocAsk version.
#   .\scripts\release.ps1 -BaseUrl https://files.example.internal/docask -Notes "แก้บั๊ก PDF"
# Bump "version" in package.json first. The private key never enters the repo.
param(
  [Parameter(Mandatory = $true)][string]$BaseUrl,
  [string]$Notes = "",
  [string]$KeyPath = "$env:USERPROFILE\.tauri\docask-updater.key"
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not (Test-Path $KeyPath)) { throw "ไม่พบกุญแจเซ็นอัปเดตที่ $KeyPath" }
$env:TAURI_SIGNING_PRIVATE_KEY = $KeyPath
if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "" }
# clients built by this script check this URL by default
$env:DOCASK_UPDATE_URL = "$($BaseUrl.TrimEnd('/'))/latest.json"

npm run tauri build
if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }

# pass notes via env: PowerShell 5.1 mangles Thai command-line arguments to node
$env:DOCASK_RELEASE_NOTES = $Notes
node scripts/make-latest-json.mjs $BaseUrl
if ($LASTEXITCODE -ne 0) { throw "make-latest-json failed" }

Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY, Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD, Env:DOCASK_RELEASE_NOTES -ErrorAction SilentlyContinue
