param(
    [string]$Root = (Split-Path $PSScriptRoot -Parent),
    [Parameter(Mandatory=$true)][ValidatePattern('^\d+\.\d+(?:\.\d+){0,2}(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$')][string]$Version,
    [string]$OutputDir,
    [string]$BuildRoot,
    [switch]$PrepareOnly,
    [switch]$Candidate
)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Utility')
$Root = (Get-Item -LiteralPath $Root).FullName
if (!$BuildRoot) { $BuildRoot = $Root }
if ($Candidate -and !$OutputDir) { throw 'Choose a separate -OutputDir for candidate packages.' }
if (!$OutputDir) { $OutputDir = Join-Path $Root "release\$Version" }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$commit = git -C $Root rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the source revision.' }
if (!$Candidate) {
    $changes = git -C $Root status --porcelain --untracked-files=normal
    if ($LASTEXITCODE -ne 0 -or $changes) { throw 'Commit the source before sealing a release. Use -Candidate for local verification.' }
}
$summary = Join-Path $Root "TEST_SUMMARY_v$Version.md"
if (!$PrepareOnly) {
    if (Test-Path -LiteralPath $OutputDir) { throw "Choose a new output directory; already exists: $OutputDir" }
    if (!$Candidate -and !(Test-Path -LiteralPath $summary -PathType Leaf)) { throw "Missing test summary: $summary" }
}
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$apps = [ordered]@{}
foreach ($flavor in 'full','portable') {
    $dist = if ($flavor -eq 'full') { 'dist-accelerated' } else { 'dist-portable' }
    $app = (Get-Item -LiteralPath (Join-Path $BuildRoot "$dist\jumpbot")).FullName
    $build = Get-Content -Raw -LiteralPath (Join-Path $app 'build-source.json') | ConvertFrom-Json
    if ($build.commit -ne $commit -or $build.flavor -ne $flavor) { throw "Build revision or flavor mismatch: $app" }
    foreach ($required in 'jumpbot.exe','jumpbot-desktop.exe','config.json','templates/piece.png','THIRD_PARTY_NOTICES.md') {
        if (!$build.files_sha256.PSObject.Properties[$required]) { throw "Missing from build manifest: $required" }
    }
    $sourceFiles = git -C $Root -c core.quotepath=false ls-files --cached --others --exclude-standard -- desktop templates config.json run.bat README.md '*NOTICE*' 'LICENSE*' tools/build_app.ps1 tools/collect_licenses.ps1
    if ($LASTEXITCODE -ne 0) { throw 'Cannot list build inputs.' }
    $sourceFiles = @($sourceFiles | Where-Object { Test-Path -LiteralPath (Join-Path $Root $_) -PathType Leaf })
    if (Compare-Object $sourceFiles @($build.source_sha256.PSObject.Properties.Name)) { throw "Source list changed: $flavor" }
    foreach ($entry in $build.source_sha256.PSObject.Properties) {
        if ((Get-FileHash -LiteralPath (Join-Path $Root $entry.Name)).Hash -ne $entry.Value) { throw "Source changed since build: $($entry.Name)" }
    }
    $payload = @(Get-ChildItem -LiteralPath $app -Recurse -File | ForEach-Object { $_.FullName.Substring($app.Length + 1).Replace('\','/') } |
        Where-Object { $_ -notmatch '^(logs/|debug/|build-source\.json$|source-version\.json$)' })
    if (Compare-Object $payload @($build.files_sha256.PSObject.Properties.Name)) { throw "Payload file list changed: $app" }
    foreach ($entry in $build.files_sha256.PSObject.Properties) {
        if ((Get-FileHash -LiteralPath (Join-Path $app $entry.Name)).Hash -ne $entry.Value) { throw "Payload changed since build: $($entry.Name)" }
    }
    $versionInfo = [ordered]@{ version=$Version; commit=$commit; flavor=$flavor; candidate=[bool]$Candidate; build_manifest_sha256=(Get-FileHash -LiteralPath (Join-Path $app 'build-source.json')).Hash }
    [IO.File]::WriteAllText((Join-Path $app 'source-version.json'), ($versionInfo | ConvertTo-Json))
    $apps[$flavor] = $app
}
if ($PrepareOnly) { return }
# Make all four artifacts from the verified folders and expose the directory only on success.
$parent = Split-Path $OutputDir -Parent
New-Item -ItemType Directory -Force -Path $parent | Out-Null
$stage = Join-Path $parent ('.jumpbot-package-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    foreach ($flavor in $apps.Keys) {
        $app = $apps[$flavor]
        & (Join-Path $Root 'tools\installer\build_installer.ps1') -Root $Root -Version $Version -Flavor $flavor -PayloadDir $app -OutputDir $stage -SkipAppBuild
        $archive = [IO.Compression.ZipFile]::Open((Join-Path $stage "jumpbot-win64-v$Version-$flavor.zip"), [IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($file in Get-ChildItem -LiteralPath $app -Recurse -File) {
                $relative = $file.FullName.Substring($app.Length + 1).Replace('\','/')
                if ($relative -match '^(logs|debug)/') { continue }
                [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName, "jumpbot/$relative") | Out-Null
            }
        } finally { $archive.Dispose() }
    }
    if (Test-Path -LiteralPath $summary -PathType Leaf) { Copy-Item -LiteralPath $summary -Destination (Join-Path $stage 'TEST_SUMMARY.md') }
    $sums = foreach ($file in (Get-ChildItem -LiteralPath $stage -File | Sort-Object Name)) {
        '{0}  {1}' -f (Get-FileHash -LiteralPath $file.FullName).Hash.ToLowerInvariant(), $file.Name
    }
    [IO.File]::WriteAllLines((Join-Path $stage 'SHA256SUMS.txt'), [string[]]$sums)
    [IO.Directory]::Move($stage, $OutputDir)
} finally {
    if (Test-Path -LiteralPath $stage) {
        $checked = (Get-Item -LiteralPath $stage).FullName
        if ((Split-Path $checked -Parent) -eq (Get-Item $parent).FullName -and (Split-Path $checked -Leaf) -match '^\.jumpbot-package-[0-9a-f]{32}$') {
            Remove-Item -LiteralPath $checked -Recurse -Force
        }
    }
}
Write-Host "Packages: $OutputDir"
