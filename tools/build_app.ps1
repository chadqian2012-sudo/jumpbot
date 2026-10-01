# Package jumpbot as a standalone app folder (PyInstaller onedir).
# ASCII-only (Windows PowerShell reads .ps1 as ANSI).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools\build_app.ps1            # with GPU (CuPy+CUDA runtime)
#   powershell -ExecutionPolicy Bypass -File tools\build_app.ps1 -NoGpu     # CPU-only (smaller)
param(
    [string]$Root = "D:\Codex Workspace\jumpbot",
    [string]$Python = "D:\Codex Workspace\.venv\Scripts\python.exe",
    [switch]$NoGpu,
    [switch]$Shortcut
)
$ErrorActionPreference = "Stop"
$env:JUMPBOT_WITH_GPU = if ($NoGpu) { "0" } else { "1" }

# Packaging is CPU heavy; drop to BelowNormal so a running game keeps priority.
try {
    [System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'BelowNormal'
    Write-Host "build priority: BelowNormal"
} catch {
    Write-Warning "could not lower priority: $_"
}

Push-Location $Root
try {
    Write-Host "== PyInstaller build (GPU=$($env:JUMPBOT_WITH_GPU)) =="
    & $Python -m PyInstaller --noconfirm --clean (Join-Path $Root "tools\jumpbot.spec")
    if ($LASTEXITCODE -ne 0) { Write-Error "PyInstaller failed"; exit 3 }

    $exe = Join-Path $Root "dist\jumpbot\jumpbot.exe"
    if (-not (Test-Path $exe)) { Write-Error "exe not found: $exe"; exit 4 }

    Write-Host "== selftest (no game window needed) =="
    & $exe --selftest

    $appDir = Join-Path $Root "dist\jumpbot"

    # Trim files verified as unnecessary (keeps the frozen GPU stack working):
    #  * opencv ffmpeg decoder (~29MB): we only decode PNG templates
    #  * nvrtc "alt" variant (~86MB): CuPy uses nvrtc64_120_0.dll
    #  NOT trimmed: cupy thrust/cub (~108MB) - CuPy imports them, removing kills the GPU path.
    $trim = @(
        "cv2\opencv_videoio_ffmpeg*_64.dll",
        "nvidia\cuda_nvrtc\bin\nvrtc64_*.alt.dll"
    )
    $saved = 0
    foreach ($pattern in $trim) {
        Get-ChildItem (Join-Path $appDir "_internal") -Recurse -Filter (Split-Path $pattern -Leaf) -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -like ("*" + $pattern.Replace('*', '*')) } |
            ForEach-Object {
                $saved += $_.Length
                Remove-Item -LiteralPath $_.FullName -Force
                Write-Host ("trimmed: {0} ({1:N1} MB)" -f $_.Name, ($_.Length / 1MB))
            }
    }
    if ($saved -gt 0) { Write-Host ("trim total: {0:N0} MB" -f ($saved / 1MB)) }
    $size = (Get-ChildItem $appDir -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
    Write-Host ("app folder size after trim: {0:N0} MB" -f $size)
    $longBat = @"
@echo off
rem Launch jumpbot in long-run mode (auto-resume when the game window comes back).
cd /d "%~dp0"
jumpbot.exe --max-jumps 5000 --daemon
pause
"@
    $dryBat = @"
@echo off
rem Recognition only - never sends mouse input (safe check).
cd /d "%~dp0"
jumpbot.exe --dry-run --samples 3
pause
"@
    $selfBat = @"
@echo off
rem Verify dependencies / templates / GPU backend without touching the game.
cd /d "%~dp0"
jumpbot.exe --selftest
pause
"@
    Set-Content -Path (Join-Path $appDir "run_long.bat")   -Value $longBat -Encoding ASCII
    Set-Content -Path (Join-Path $appDir "run_dryrun.bat") -Value $dryBat  -Encoding ASCII
    Set-Content -Path (Join-Path $appDir "selftest.bat")   -Value $selfBat -Encoding ASCII
    Write-Host "launchers written: run_long.bat / run_dryrun.bat / selftest.bat"

    if ($Shortcut) {
        $desktop = [Environment]::GetFolderPath("Desktop")
        $lnk = Join-Path $desktop "jumpbot (long run).lnk"
        $shell = New-Object -ComObject WScript.Shell
        $sc = $shell.CreateShortcut($lnk)
        $sc.TargetPath = $exe
        $sc.Arguments = "--max-jumps 5000 --daemon"
        $sc.WorkingDirectory = $appDir
        $sc.Description = "jumpbot - long run mode"
        $sc.Save()
        Write-Host "desktop shortcut: $lnk"
    }
    Write-Host "== DONE: $exe =="
} finally {
    Pop-Location
}
