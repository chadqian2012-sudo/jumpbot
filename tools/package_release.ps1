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
$stage = $null
try {
    if (!$PrepareOnly) {
        $parent = Split-Path $OutputDir -Parent
        $stage = Join-Path $parent ('.jumpbot-package-' + [guid]::NewGuid().ToString('N'))
        $delivery = Join-Path $stage 'delivery'
        [void][IO.Directory]::CreateDirectory($delivery)
    }
    $sourceFiles = git -C $Root -c core.quotepath=false ls-files --cached --others --exclude-standard -- desktop templates config.json run.bat README.md '*NOTICE*' 'LICENSE*' tools/build_app.ps1 tools/collect_licenses.ps1
    if ($LASTEXITCODE -ne 0) { throw 'Cannot list build inputs.' }
    $sourceFiles = @($sourceFiles | Where-Object { Test-Path -LiteralPath (Join-Path $Root $_) -PathType Leaf })
    foreach ($flavor in 'full','portable') {
        $dist = if ($flavor -eq 'full') { 'dist-accelerated' } else { 'dist-portable' }
        $app = (Get-Item -LiteralPath (Join-Path $BuildRoot "$dist\jumpbot")).FullName
        $copy = if ($stage) { Join-Path $stage $flavor }
        $build = & (Join-Path $PSScriptRoot 'installer\read_payload.ps1') -AppDir $app -Flavor $flavor -CopyTo $copy
        if ($build.commit -ne $commit) { throw "Build revision mismatch: $app" }
        if (Compare-Object $sourceFiles @($build.source_sha256.PSObject.Properties.Name)) { throw "Source list changed: $flavor" }
        foreach ($entry in $build.source_sha256.PSObject.Properties) {
            if ((Get-FileHash -LiteralPath (Join-Path $Root $entry.Name)).Hash -ne $entry.Value) { throw "Source changed since build: $($entry.Name)" }
        }
        $manifestDir = if ($copy) { $copy } else { $app }
        $versionInfo = [ordered]@{ version=$Version; commit=$commit; flavor=$flavor; candidate=[bool]$Candidate; build_manifest_sha256=(Get-FileHash -LiteralPath (Join-Path $manifestDir 'build-source.json')).Hash }
        $json = $versionInfo | ConvertTo-Json
        [IO.File]::WriteAllText((Join-Path $app 'source-version.json'), $json)
        if ($copy) {
            [IO.File]::WriteAllText((Join-Path $copy 'source-version.json'), $json)
            $apps[$flavor] = @($build.files_sha256.PSObject.Properties.Name) + @('build-source.json','source-version.json')
        }
    }
    if ($PrepareOnly) { return }
    foreach ($flavor in $apps.Keys) {
        $app = Join-Path $stage $flavor
        & (Join-Path $Root 'tools\installer\build_installer.ps1') -Root $Root -Version $Version -Flavor $flavor -PayloadDir $app -OutputDir $delivery -SkipAppBuild
        $archive = [IO.Compression.ZipFile]::Open((Join-Path $delivery "jumpbot-win64-v$Version-$flavor.zip"), [IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($relative in $apps[$flavor]) {
                [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, (Join-Path $app $relative), "jumpbot/$relative") | Out-Null
            }
        } finally { $archive.Dispose() }
    }
    if (Test-Path -LiteralPath $summary -PathType Leaf) { Copy-Item -LiteralPath $summary -Destination (Join-Path $delivery 'TEST_SUMMARY.md') }
    $sums = foreach ($file in (Get-ChildItem -LiteralPath $delivery -File | Sort-Object Name)) {
        '{0}  {1}' -f (Get-FileHash -LiteralPath $file.FullName).Hash.ToLowerInvariant(), $file.Name
    }
    [IO.File]::WriteAllLines((Join-Path $delivery 'SHA256SUMS.txt'), [string[]]$sums)
    [IO.Directory]::Move($delivery, $OutputDir)
} finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) {
        $checked = (Get-Item -LiteralPath $stage).FullName
        if ((Split-Path $checked -Parent) -eq (Get-Item -LiteralPath $parent).FullName -and (Split-Path $checked -Leaf) -match '^\.jumpbot-package-[0-9a-f]{32}$') {
            Remove-Item -LiteralPath $checked -Recurse -Force
        }
    }
}
Write-Host "Packages: $OutputDir"
