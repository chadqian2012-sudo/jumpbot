param(
    [string]$Root = (Join-Path $PSScriptRoot '..\..'),
    [Parameter(Mandatory=$true)][ValidatePattern('^\d+\.\d+(?:\.\d+){0,2}(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$')][string]$Version,
    [ValidateSet('full','portable')][string]$Flavor = 'full',
    [string]$PayloadDir,
    [string]$OutputDir,
    [switch]$SkipAppBuild
)
$ErrorActionPreference = 'Stop'
$Root = (Get-Item -LiteralPath $Root).FullName
$parsedVersion = [version]($Version.Split('-')[0])
$numericVersion = '{0}.{1}.{2}.{3}' -f $parsedVersion.Major, $parsedVersion.Minor, ([math]::Max(0,$parsedVersion.Build)), ([math]::Max(0,$parsedVersion.Revision))
$suffix = if ($Flavor -eq 'full') { 'accelerated' } else { 'portable' }
if (!$PayloadDir) { $PayloadDir = Join-Path $Root "dist-$suffix\jumpbot" }
if (!$OutputDir) { $OutputDir = Join-Path $Root "installer-$suffix" }
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
$setup = Join-Path $OutputDir "jumpbot-setup-$Version-$Flavor.exe"
if (Test-Path -LiteralPath $setup) { throw "Installer already exists: $setup" }
if (!$SkipAppBuild) {
    if ((Split-Path $PayloadDir -Leaf) -ne 'jumpbot') { throw 'Payload directory must be named jumpbot.' }
    & (Join-Path $Root 'tools\build_app.ps1') -Root $Root -DistRoot (Split-Path ([IO.Path]::GetFullPath($PayloadDir))) -NoGpu:($Flavor -eq 'portable')
}
$PayloadDir = (Get-Item -LiteralPath $PayloadDir).FullName
$build = Get-Content -Raw -LiteralPath (Join-Path $PayloadDir 'build-source.json') | ConvertFrom-Json
if ($build.flavor -ne $Flavor) { throw "Wrong payload flavor for $setup" }
foreach ($exe in 'jumpbot.exe','jumpbot-desktop.exe') {
    if ((Get-FileHash -LiteralPath (Join-Path $PayloadDir $exe)).Hash -ne $build.files_sha256.$exe) { throw "Wrong or missing executable: $exe" }
}
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (!$iscc) {
    $iscc = @(
        (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Inno Setup 6\ISCC.exe'),
        (Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Inno Setup 6\ISCC.exe')
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
}
if (!$iscc) { throw 'Install Inno Setup 6 or put ISCC.exe on PATH.' }
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
& $iscc /Qp "/DPayloadDir=$PayloadDir" "/DOutputDir=$OutputDir" "/DAppVersion=$Version" "/DAppVersionInfo=$numericVersion" "/DAppFlavor=$Flavor" (Join-Path $PSScriptRoot 'jumpbot.iss')
if ($LASTEXITCODE -ne 0 -or !(Test-Path -LiteralPath $setup -PathType Leaf)) { throw "Installer compilation failed: $setup" }
Write-Host "Installer: $setup"
