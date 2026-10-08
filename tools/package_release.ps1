param(
    [string]$Root = 'D:\Codex Workspace\jumpbot',
    [string]$Version = '1.2',
    [string]$OutputDir = ''
)
$ErrorActionPreference = 'Stop'
$packageRoot = (Resolve-Path -LiteralPath $Root).Path
$packageOutput = if($OutputDir) { [System.IO.Path]::GetFullPath($OutputDir) } else { Join-Path $packageRoot "release\$Version" }
New-Item -ItemType Directory -Force -Path $packageOutput | Out-Null
$packageCommit = (git -C $packageRoot rev-parse HEAD).Trim()
if($packageCommit -notmatch '^[0-9a-f]{40}$') { throw 'source commit unavailable' }
git -C $packageRoot diff --quiet HEAD -- jumpbot.py run_control.py config.json tools/build_app.ps1 tools/jumpbot.spec tools/installer tools/package_release.ps1
if($LASTEXITCODE -ne 0) { throw 'commit runtime/build source before sealing delivery' }
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$packageFiles = @()
foreach($packageFlavor in @('full','portable')) {
    $packageDist = if($packageFlavor -eq 'full') { 'dist-accelerated' } else { 'dist-portable' }
    $packageInstaller = if($packageFlavor -eq 'full') { 'installer-accelerated' } else { 'installer-portable' }
    $packageApp = Join-Path $packageRoot "$packageDist\jumpbot"
    if(-not (Test-Path -LiteralPath (Join-Path $packageApp 'jumpbot.exe'))) { throw 'missing app executable' }
    $packageSourceHashes = [ordered]@{}
    foreach($packageSource in @('jumpbot.py','run_control.py','config.json','tools/dashboard.py')) {
        $packageSourceHashes[$packageSource] = (Get-FileHash -LiteralPath (Join-Path $packageRoot $packageSource) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $packageManifest = [ordered]@{ version=$Version; commit=$packageCommit; flavor=$packageFlavor; source_sha256=$packageSourceHashes }
    [System.IO.File]::WriteAllText((Join-Path $packageApp 'source-version.json'), ($packageManifest | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))
    $packageZipPath = Join-Path $packageOutput "jumpbot-win64-v$Version-$packageFlavor.zip"
    if(Test-Path -LiteralPath $packageZipPath) { throw "will not overwrite existing ZIP: $packageZipPath" }
    $packageStream = [System.IO.File]::Open($packageZipPath, [System.IO.FileMode]::CreateNew)
    $packageArchive = [System.IO.Compression.ZipArchive]::new($packageStream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach($packageFile in Get-ChildItem -LiteralPath $packageApp -File -Recurse) {
            $packageRelative = $packageFile.FullName.Substring($packageApp.Length + 1).Replace('\','/')
            if($packageRelative -match '^(logs|debug)/') { continue }
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
$packageSummary = Join-Path $packageOutput 'TEST_SUMMARY.md'
Copy-Item -LiteralPath (Join-Path $packageRoot "TEST_SUMMARY_v$Version.md") -Destination $packageSummary
$packageFiles += $packageSummary
$packageHashLines = foreach($packageFile in $packageFiles) {
    '{0}  {1}' -f (Get-FileHash -LiteralPath $packageFile -Algorithm SHA256).Hash.ToLowerInvariant(), (Split-Path $packageFile -Leaf)
}
[System.IO.File]::WriteAllLines((Join-Path $packageOutput 'SHA256SUMS.txt'), $packageHashLines, [System.Text.UTF8Encoding]::new($false))
Get-ChildItem -LiteralPath $packageOutput -File | Select-Object Name,Length
