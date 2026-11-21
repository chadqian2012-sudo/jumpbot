param(
    [Parameter(Mandatory=$true)][string]$AppDir,
    [string]$Root = (Split-Path $PSScriptRoot -Parent),
    [switch]$NoGpu
)
$ErrorActionPreference = 'Stop'
$dest = Join-Path $AppDir 'licenses'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$features = @()
if ($NoGpu) { $features += '--no-default-features' }
$graph = & cargo metadata --manifest-path (Join-Path $Root 'desktop\Cargo.toml') --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc @features
if ($LASTEXITCODE -ne 0) { throw 'Cannot read the locked Cargo dependency graph.' }
$metadata = $graph | ConvertFrom-Json
$index = foreach ($pkg in ($metadata.packages | Where-Object source | Sort-Object name, version)) {
    $crateDir = Split-Path $pkg.manifest_path
    $texts = @(Get-ChildItem -LiteralPath $crateDir -File | Where-Object Name -Match '^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE|COPYRIGHT)([._-]|$)')
    if ($pkg.license_file) { $texts += Get-Item -LiteralPath (Join-Path $crateDir $pkg.license_file) }
    $subdir = Join-Path $dest "$($pkg.name)-$($pkg.version)"
    foreach ($file in ($texts | Sort-Object FullName -Unique)) {
        New-Item -ItemType Directory -Path $subdir -Force | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $subdir -Force
    }
    $nested = Join-Path $crateDir 'licenses'
    if (Test-Path -LiteralPath $nested -PathType Container) {
        New-Item -ItemType Directory -Path $subdir -Force | Out-Null
        Copy-Item -LiteralPath $nested -Destination $subdir -Recurse
    }
    "$($pkg.name) $($pkg.version) | $($pkg.license) | $($pkg.repository)"
}
[IO.File]::WriteAllLines((Join-Path $dest 'cargo-dependencies.txt'), [string[]]$index)
$sysroot = & rustc --print sysroot
if ($LASTEXITCODE -ne 0) { throw 'Cannot locate the Rust standard library notices.' }
$doc = Join-Path $sysroot 'share\doc\rust'
$copyright = @('COPYRIGHT-library.html','COPYRIGHT.html') | ForEach-Object { Join-Path $doc $_ } | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (!$copyright) { throw "Missing Rust toolchain copyright notices: $doc" }
Copy-Item -LiteralPath $copyright -Destination (Join-Path $dest 'rust-standard-library.html')
Copy-Item -LiteralPath (Join-Path $Root 'desktop\ui\Saira-OFL.txt') -Destination $dest
$sdk = if ($env:JUMPBOT_OPENCV_DIR) { $env:JUMPBOT_OPENCV_DIR } else { Join-Path $Root 'build\cv-sdk' }
$cvLicenses = Join-Path $sdk 'etc\licenses'
if (!(Test-Path -LiteralPath $cvLicenses -PathType Container)) { throw "Missing OpenCV SDK notices: $cvLicenses" }
Copy-Item -LiteralPath $cvLicenses -Destination (Join-Path $dest 'OpenCV') -Recurse
