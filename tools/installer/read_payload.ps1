# Validate the build manifest before opening an installer or ZIP. When sealing
# a release, copy only its declared files and verify the bytes in that copy.
param(
    [Parameter(Mandatory=$true)][string]$AppDir,
    [Parameter(Mandatory=$true)][string]$Flavor,
    [string]$CopyTo
)
$ErrorActionPreference = 'Stop'
$AppDir = (Get-Item -LiteralPath $AppDir).FullName
$manifest = Join-Path $AppDir 'build-source.json'
$bytes = [IO.File]::ReadAllBytes($manifest)
$build = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff) | ConvertFrom-Json
if ($build.flavor -ne $Flavor) { throw "Wrong payload flavor: $AppDir" }
$declared = @($build.files_sha256.PSObject.Properties.Name)
foreach ($name in 'jumpbot.exe','jumpbot-desktop.exe','config.json','templates/piece.png','THIRD_PARTY_NOTICES.md') {
    if ($name -notin $declared) { throw "Missing from build manifest: $name" }
}
$actual = @(Get-ChildItem -LiteralPath $AppDir -Recurse -Force -File | ForEach-Object {
    $_.FullName.Substring($AppDir.Length + 1).Replace('\','/')
} | Where-Object { $_ -notmatch '^((?:_internal/)?(?:logs|debug)/|build-source\.json$|source-version\.json$)' })
$difference = Compare-Object -ReferenceObject $declared -DifferenceObject $actual
if ($difference) { throw "Payload file list changed: $($difference.InputObject -join ', ')" }

if ($CopyTo) { New-Item -ItemType Directory -Path $CopyTo | Out-Null }
foreach ($entry in $build.files_sha256.PSObject.Properties) {
    $file = Join-Path $AppDir $entry.Name
    if ($CopyTo) {
        $copy = Join-Path $CopyTo $entry.Name
        [void][IO.Directory]::CreateDirectory((Split-Path $copy))
        [IO.File]::Copy($file, $copy)
        $file = $copy
    }
    if ((Get-FileHash -LiteralPath $file).Hash -ne $entry.Value) { throw "Payload changed since build: $($entry.Name)" }
}
if ($CopyTo) { [IO.File]::WriteAllBytes((Join-Path $CopyTo 'build-source.json'), $bytes) }
$build
