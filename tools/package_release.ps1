param(
    [string]$Root = 'D:\Codex Workspace\jumpbot',
    [ValidatePattern('^\d+\.\d+(?:\.\d+){0,2}(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$')][string]$Version = '1.2',
    [string]$OutputDir = '',
    [switch]$PrepareOnly
)
$ErrorActionPreference = 'Stop'
# A PowerShell 7 parent can put incompatible modules ahead of Windows PS 5.1.
Import-Module (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Utility') -ErrorAction Stop
$packageRoot = (Resolve-Path -LiteralPath $Root).Path
$packageOutput = if($OutputDir) { [System.IO.Path]::GetFullPath($OutputDir) } else { Join-Path $packageRoot "release\$Version" }
New-Item -ItemType Directory -Force -Path $packageOutput | Out-Null
$packageCommit = (git -C $packageRoot rev-parse HEAD).Trim()
if($packageCommit -notmatch '^[0-9a-f]{40}$') { throw 'source commit unavailable' }
git -C $packageRoot diff --quiet HEAD
if($LASTEXITCODE -ne 0) { throw 'commit tracked source before sealing delivery' }
if(-not $PrepareOnly) {
    foreach($packageFlavor in @('full','portable')) {
        $packageInstaller = if($packageFlavor -eq 'full') { 'installer-accelerated' } else { 'installer-portable' }
        $packageSetupName = "jumpbot-setup-$Version-$packageFlavor.exe"
        if(-not (Test-Path -LiteralPath (Join-Path $packageRoot "$packageInstaller\$packageSetupName"))) { throw "missing installer: $packageSetupName" }
        foreach($packageName in @("jumpbot-win64-v$Version-$packageFlavor.zip",$packageSetupName)) {
            if(Test-Path -LiteralPath (Join-Path $packageOutput $packageName)) { throw "will not overwrite existing delivery: $packageName" }
        }
    }
    if(-not (Test-Path -LiteralPath (Join-Path $packageRoot "TEST_SUMMARY_v$Version.md"))) { throw 'missing test summary' }
    foreach($packageName in @('TEST_SUMMARY.md','SHA256SUMS.txt')) {
        if(Test-Path -LiteralPath (Join-Path $packageOutput $packageName)) { throw "will not overwrite existing delivery: $packageName" }
    }
}
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$packageFiles = @()
foreach($packageFlavor in @('full','portable')) {
    $packageDist = if($packageFlavor -eq 'full') { 'dist-accelerated' } else { 'dist-portable' }
    $packageInstaller = if($packageFlavor -eq 'full') { 'installer-accelerated' } else { 'installer-portable' }
    # The caller/temp directory may use an 8.3 name (RUNNER~1), while the
    # provider expands Get-ChildItem.FullName. Use that same canonical base.
    $packageApp = (Get-Item -LiteralPath (Join-Path $packageRoot "$packageDist\jumpbot")).FullName.TrimEnd('\')
    if(-not (Test-Path -LiteralPath (Join-Path $packageApp 'jumpbot.exe'))) { throw 'missing app executable' }
    $packageBuild = Get-Content -LiteralPath (Join-Path $packageApp 'build-source.json') -Raw | ConvertFrom-Json
    $packageExeHash = (Get-FileHash -LiteralPath (Join-Path $packageApp 'jumpbot.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    if($packageBuild.commit -ne $packageCommit -or $packageBuild.flavor -ne $packageFlavor -or $packageBuild.exe_sha256 -ne $packageExeHash) {
        throw "build identity does not match current source/flavor/executable: $packageFlavor"
    }
    $packageSourceHashes = [ordered]@{}
    foreach($packageSource in @('jumpbot.py','run_control.py','skin_vision.py','config.json','tools/dashboard.py','tools/_scorelog.py','tools/live_acceptance.py','tools/run_frozen_acceptance.py','tools/run_skin_acceptance.py')) {
        $packageSourceHashes[$packageSource] = (Get-FileHash -LiteralPath (Join-Path $packageRoot $packageSource) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    foreach($packageProperty in $packageBuild.source_sha256.PSObject.Properties) {
        if($packageSourceHashes[$packageProperty.Name] -ne $packageProperty.Value) { throw "build source hash mismatch: $($packageProperty.Name)" }
    }
    foreach($packageSource in @('jumpbot.py','run_control.py','skin_vision.py','config.json','tools/dashboard.py','tools/_scorelog.py')) {
        if(-not $packageBuild.source_sha256.PSObject.Properties[$packageSource]) { throw "missing build source hash: $packageSource" }
    }
    $packageManifest = [ordered]@{ version=$Version; commit=$packageCommit; flavor=$packageFlavor; exe_sha256=$packageExeHash; source_sha256=$packageSourceHashes }
    $packageManifestText = $packageManifest | ConvertTo-Json -Depth 5
    $packageManifestPath = Join-Path $packageApp 'source-version.json'
    if($PrepareOnly) {
        [System.IO.File]::WriteAllText($packageManifestPath, $packageManifestText, [System.Text.UTF8Encoding]::new($false))
        Write-Host "prepared manifest before installer compilation: $packageFlavor"
        continue
    }
    if((Get-Content -LiteralPath $packageManifestPath -Raw).Trim() -ne $packageManifestText.Trim()) {
        throw 'prepare matching manifests before compiling installers'
    }
    $packageZipPath = Join-Path $packageOutput "jumpbot-win64-v$Version-$packageFlavor.zip"
    if(Test-Path -LiteralPath $packageZipPath) { throw "will not overwrite existing ZIP: $packageZipPath" }
    $packageStream = [System.IO.File]::Open($packageZipPath, [System.IO.FileMode]::CreateNew)
    $packageArchive = [System.IO.Compression.ZipArchive]::new($packageStream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach($packageFile in Get-ChildItem -LiteralPath $packageApp -File -Recurse) {
            if(-not $packageFile.FullName.StartsWith($packageApp + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
                throw 'payload file escaped canonical app directory'
            }
            $packageRelative = $packageFile.FullName.Substring($packageApp.Length + 1).Replace('\','/')
            if($packageRelative -match '^(_internal/)?(logs|debug)/') { continue }
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($packageArchive, $packageFile.FullName, "jumpbot/$packageRelative", [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    } finally {
        $packageArchive.Dispose()
        $packageStream.Dispose()
    }
    $packageSetupName = "jumpbot-setup-$Version-$packageFlavor.exe"
    Copy-Item -LiteralPath (Join-Path $packageRoot "$packageInstaller\$packageSetupName") -Destination (Join-Path $packageOutput $packageSetupName)
    $packageFiles += @($packageZipPath, (Join-Path $packageOutput $packageSetupName))
}
if($PrepareOnly) { return }
$packageSummary = Join-Path $packageOutput 'TEST_SUMMARY.md'
Copy-Item -LiteralPath (Join-Path $packageRoot "TEST_SUMMARY_v$Version.md") -Destination $packageSummary
$packageFiles += $packageSummary
$packageHashLines = foreach($packageFile in $packageFiles) {
    '{0}  {1}' -f (Get-FileHash -LiteralPath $packageFile -Algorithm SHA256).Hash.ToLowerInvariant(), (Split-Path $packageFile -Leaf)
}
[System.IO.File]::WriteAllLines((Join-Path $packageOutput 'SHA256SUMS.txt'), $packageHashLines, [System.Text.UTF8Encoding]::new($false))
Get-ChildItem -LiteralPath $packageOutput -File | Select-Object Name,Length
