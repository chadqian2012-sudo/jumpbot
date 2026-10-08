param(
    [Parameter(Mandatory=$true)][string]$AppDir,
    [string]$Python = 'D:\Codex Workspace\.venv\Scripts\python.exe',
    [switch]$Full
)
$ErrorActionPreference = 'Stop'
$licenseAppDir = (Resolve-Path -LiteralPath $AppDir).Path
$licenseOutputDir = Join-Path $licenseAppDir 'licenses'
New-Item -ItemType Directory -Force -Path $licenseOutputDir | Out-Null
$licenseMetadataCode = @'
import importlib.metadata as m
import json, re, sys
from pathlib import Path
names = ['numpy', 'opencv-python', 'opencv-python-headless', 'mss', 'pyopencl', 'pytools', 'PyInstaller']
if sys.argv[1] == 'full':
    names += ['cupy-cuda12x', 'cuda-pathfinder', 'nvidia-cuda-runtime-cu12', 'nvidia-cuda-nvrtc-cu12', 'nvidia-nvjitlink-cu12']
result = []
for name in names:
    try:
        d = m.distribution(name)
    except m.PackageNotFoundError:
        continue
    for f in d.files or []:
        text = str(f).replace('\\', '/')
        if '.dist-info/' not in text:
            continue
        if '/licenses/' not in text and not re.search(r'/(license|copying|notice|eula)(\.[^/]*)?$', text, re.I):
            continue
        relative = text.split('/licenses/', 1)[-1] if '/licenses/' in text else f.name
        result.append(dict(source=str(d.locate_file(f)), relative=name + '/' + relative))
python_license = Path(sys.base_prefix) / 'LICENSE.txt'
if python_license.exists():
    result.append(dict(source=str(python_license), relative='Python/LICENSE.txt'))
print(json.dumps(result))
'@
$licenseFlavor = if($Full) { 'full' } else { 'portable' }
$licenseJson = & $Python -c $licenseMetadataCode $licenseFlavor
if($LASTEXITCODE -ne 0) { throw 'license inventory failed' }
$licenseInventory = $licenseJson | ConvertFrom-Json
foreach($licenseEntry in $licenseInventory) {
    $licenseDestination = [System.IO.Path]::GetFullPath((Join-Path $licenseOutputDir $licenseEntry.relative))
    if(-not $licenseDestination.StartsWith($licenseOutputDir + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'license path escaped the app license directory'
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $licenseDestination -Parent) | Out-Null
    Copy-Item -LiteralPath $licenseEntry.source -Destination $licenseDestination -Force
}
Write-Host "copied $($licenseInventory.Count) runtime license files to $licenseOutputDir"
