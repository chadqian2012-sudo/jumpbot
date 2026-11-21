# Run in an x64 Visual Studio developer shell with Rust (MSVC) on PATH.
param(
    [string]$Root = (Split-Path $PSScriptRoot -Parent),
    [string]$DistRoot,
    [string]$WorkRoot,
    [switch]$NoGpu,
    [switch]$Shortcut
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Utility')
$Root = (Get-Item -LiteralPath $Root).FullName
$flavor = if ($NoGpu) { 'portable' } else { 'full' }
if (!$DistRoot) { $DistRoot = Join-Path $Root $(if ($NoGpu) { 'dist-portable' } else { 'dist-accelerated' }) }
if (!$WorkRoot) {
    $WorkRoot = Join-Path $Root 'desktop\target'
    if ($NoGpu) { $WorkRoot = Join-Path $WorkRoot 'portable' }
}
$DistRoot = [IO.Path]::GetFullPath($DistRoot)
$appDir = Join-Path $DistRoot 'jumpbot'
if (Test-Path -LiteralPath $appDir) { throw "Choose a fresh -DistRoot; app folder already exists: $appDir" }
$target = 'x86_64-pc-windows-msvc'
$manifest = Join-Path $Root 'desktop\Cargo.toml'
$features = @()
if ($NoGpu) { $features += '--no-default-features' }
$sourceCommit = git -C $Root rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the source revision.' }

function Get-SourceHashes {
    $paths = git -C $Root -c core.quotepath=false ls-files --cached --others --exclude-standard -- desktop templates config.json run.bat README.md '*NOTICE*' 'LICENSE*' tools/build_app.ps1 tools/collect_licenses.ps1
    if ($LASTEXITCODE -ne 0) { throw 'Cannot list build inputs.' }
    $hashes = [ordered]@{}
    foreach ($path in $paths) {
        if (Test-Path -LiteralPath (Join-Path $Root $path) -PathType Leaf) {
            $hashes[$path] = (Get-FileHash -LiteralPath (Join-Path $Root $path)).Hash
        }
    }
    $hashes
}
$sourceHashes = Get-SourceHashes
& cargo build --manifest-path $manifest --release --locked --target $target --target-dir $WorkRoot --bin jumpbot --bin jumpbot-desktop @features
if ($LASTEXITCODE -ne 0) { throw "Cargo release build failed ($flavor)." }
$releaseDir = Join-Path $WorkRoot "$target\release"
$work = Join-Path $DistRoot ('.staging-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work -Force | Out-Null
try {
    foreach ($name in 'jumpbot.exe', 'jumpbot-desktop.exe') {
        Copy-Item -LiteralPath (Join-Path $releaseDir $name) -Destination $work
    }
    $world = @(Get-ChildItem -LiteralPath $releaseDir -Filter 'opencv_world*.dll' -File | Where-Object Name -Match '^opencv_world\d+\.dll$')
    if ($world.Count -ne 1) { throw "Expected one release OpenCV world DLL staged by Cargo in $releaseDir, found $($world.Count)." }
    Copy-Item -LiteralPath $world[0].FullName -Destination $work

    $vswhere = Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft Visual Studio\Installer\vswhere.exe'
    $crt = @(& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Redist\MSVC\**\x64\Microsoft.VC*.CRT\vcruntime140.dll') |
        Where-Object { $_ -notmatch '\\(debug_nonredist|onecore)\\' } | Sort-Object -Descending | Select-Object -First 1
    if (!$crt) { throw 'Cannot find the Visual C++ x64 redistributable runtime in Visual Studio.' }
    Get-ChildItem -LiteralPath (Split-Path $crt) -Filter '*.dll' -File | Copy-Item -Destination $work
    foreach ($name in 'config.json', 'templates', 'run.bat', 'README.md', 'THIRD_PARTY_NOTICES.md', 'LICENSE-OpenCV.txt') {
        Copy-Item -LiteralPath (Join-Path $Root $name) -Destination $work -Recurse
    }
    $launchers = [ordered]@{
        'run_long.bat' = '--max-jumps 0 --daemon'
        'run_dryrun.bat' = '--dry-run --samples 3'
        'selftest.bat' = '--selftest'
        'run_cpu.bat' = '--backend simd --no-cuda --no-opencl --max-jumps 0 --daemon'
    }
    foreach ($name in $launchers.Keys) {
        Set-Content -LiteralPath (Join-Path $work $name) -Encoding Ascii -Value @('@echo off', 'cd /d "%~dp0"', ('jumpbot.exe ' + $launchers[$name]), 'set "rc=%errorlevel%"', 'pause', 'exit /b %rc%')
    }
    & (Join-Path $Root 'tools\collect_licenses.ps1') -AppDir $work -Root $Root -NoGpu:$NoGpu
    Push-Location -LiteralPath $work
    $savedPath = $env:Path
    try {
        # Expose only Windows, so an SDK or developer PATH cannot conceal a missing DLL.
        $env:Path = "$env:SystemRoot\System32;$env:SystemRoot"
        & '.\jumpbot.exe' --selftest --no-cuda --no-opencl
        if ($LASTEXITCODE -ne 0) { throw "Packaged CLI self-test failed ($LASTEXITCODE)." }
    } finally {
        $env:Path = $savedPath
        Pop-Location
    }
    if ((Get-SourceHashes | ConvertTo-Json -Compress) -cne ($sourceHashes | ConvertTo-Json -Compress)) {
        throw 'Source changed while building. Run the build again.'
    }
    $files = [ordered]@{}
    $workPrefix = (Get-Item -LiteralPath $work).FullName.Length + 1
    foreach ($file in Get-ChildItem -LiteralPath $work -File -Recurse) {
        $relative = $file.FullName.Substring($workPrefix).Replace('\','/')
        if ($relative -notmatch '^(logs|debug)/') { $files[$relative] = (Get-FileHash -LiteralPath $file.FullName).Hash }
    }
    $build = [ordered]@{ commit = $sourceCommit; flavor = $flavor; source_sha256 = $sourceHashes; files_sha256 = $files }
    [IO.File]::WriteAllText((Join-Path $work 'build-source.json'), ($build | ConvertTo-Json -Depth 4))
    [IO.Directory]::Move($work, $appDir)
} finally {
    if (Test-Path -LiteralPath $work) {
        $checked = (Get-Item -LiteralPath $work).FullName
        if ((Split-Path $checked -Parent) -eq (Get-Item $DistRoot).FullName -and (Split-Path $checked -Leaf) -match '^\.staging-[0-9a-f]{32}$') {
            Remove-Item -LiteralPath $checked -Recurse -Force
        }
    }
}
if ($Shortcut) {
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'jumpbot (long run).lnk'))
    $link.TargetPath = Join-Path $appDir 'jumpbot.exe'
    $link.Arguments = '--max-jumps 0 --daemon'
    $link.WorkingDirectory = $appDir
    $link.Save()
}
Write-Host "$flavor app: $appDir"
