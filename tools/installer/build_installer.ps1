# Build a single-file Windows installer (setup .exe) for jumpbot using Inno Setup.
# ASCII-only on purpose: Windows PowerShell reads .ps1 as ANSI, non-ASCII text breaks parsing.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools\installer\build_installer.ps1
#   powershell -ExecutionPolicy Bypass -File tools\installer\build_installer.ps1 -SkipAppBuild
#
# Requires: Inno Setup 6 (ISCC.exe). Install with:  winget install JRSoftware.InnoSetup
param(
    [string]$Root = "D:\Codex Workspace\jumpbot",
    [string]$Version = "1.0",
    [switch]$SkipAppBuild
)
$ErrorActionPreference = "Stop"

$payload = Join-Path $Root "dist\jumpbot"
$outDir = Join-Path $Root "installer"
$iss = Join-Path $Root "tools\installer\jumpbot.iss"

if (-not (Test-Path -LiteralPath $payload)) {
    throw "payload not found: $payload  (run tools\build_app.ps1 first)"
}
if (-not $SkipAppBuild) {
    Write-Host "== step 1/2: rebuild app folder (dist\jumpbot) =="
    & powershell -ExecutionPolicy Bypass -File (Join-Path $Root "tools\build_app.ps1")
    if ($LASTEXITCODE -ne 0) { throw "app build failed" }
} else {
    Write-Host "== step 1/2: skipped app build (-SkipAppBuild) =="
}

$iscc = @(
    (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
    (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe"),
    (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe")
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
if (-not $iscc) {
    throw "ISCC.exe not found. Install Inno Setup 6:  winget install JRSoftware.InnoSetup"
}
Write-Host "ISCC: $iscc"

New-Item -ItemType Directory -Force -Path $outDir | Out-Null
# Keep compiler priority low so a running game is not disturbed by a long LZMA2 pass.
try { [System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'BelowNormal' } catch {}

Write-Host "== step 2/2: compile installer (LZMA2/max on ~400MB payload, takes a few minutes) =="
& $iscc "/DPayloadDir=$payload" "/DOutputDir=$outDir" "/DAppVersion=$Version" $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }

$setup = Join-Path $outDir "jumpbot-setup-$Version.exe"
if (-not (Test-Path -LiteralPath $setup)) { throw "installer not produced: $setup" }
$mb = [math]::Round((Get-Item -LiteralPath $setup).Length / 1MB, 1)
Write-Host ("== DONE: {0}  ({1} MB) ==" -f $setup, $mb)
