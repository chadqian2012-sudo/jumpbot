# Build OpenCV Python bindings WITH CUDA (limited module list) into a separate venv.
# ASCII-only on purpose: Windows PowerShell 5.1 reads .ps1 as ANSI and mangles non-ASCII.
#
# Usage: powershell -ExecutionPolicy Bypass -File tools\build_opencv_cuda.ps1
param(
    [string]$CudaRoot = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6",
    [string]$SrcDir   = "D:\Codex Workspace\opencv-src",
    [string]$ContribDir = "D:\Codex Workspace\opencv_contrib-src",
    [string]$BuildDir = "D:\Codex Workspace\opencv-build-cuda2",
    [string]$VenvDir  = "D:\Codex Workspace\.venv-cuda",
    [string]$NasmDir  = "D:\Codex Workspace\nasm",
    [int]$Jobs = 8
)
$ErrorActionPreference = "Stop"

if (-not (Test-Path (Join-Path $CudaRoot "bin\nvcc.exe"))) {
    Write-Error "nvcc not found: $CudaRoot\bin\nvcc.exe"
    exit 2
}
if (-not (Test-Path (Join-Path $SrcDir "CMakeLists.txt"))) {
    Write-Error "OpenCV source not found: $SrcDir"
    exit 2
}
$py = Join-Path $VenvDir "Scripts\python.exe"
if (-not (Test-Path $py)) {
    Write-Error "venv python not found: $py"
    exit 2
}
& $py -m pip install --quiet numpy mss cmake ninja

# NASM: without it CMake disables OpenCV's SIMD extensions ("Performance will suffer")
if (Test-Path (Join-Path $NasmDir "nasm.exe")) {
    $env:PATH = "$NasmDir;" + $env:PATH
    Write-Host "NASM: $NasmDir"
} else {
    Write-Warning "NASM not found at $NasmDir; OpenCV SIMD extensions may be disabled"
}

# Python dev files: the bundled runtime has include/ + libs/python312.lib
$pyBase = & $py -c "import sys; print(sys.base_prefix)"
$pyInc  = Join-Path $pyBase "include"
$pyLib  = Join-Path $pyBase "libs\python312.lib"
if (-not (Test-Path $pyLib)) { Write-Error "python312.lib not found: $pyLib"; exit 2 }
$cudaFwd = $CudaRoot -replace '\\', '/'
$contribFwd = (Join-Path $ContribDir "modules") -replace '\\', '/'
$nasmFwd = (Join-Path $NasmDir "nasm.exe") -replace '\\', '/'
if (-not (Test-Path (Join-Path $ContribDir "modules\cudev"))) {
    Write-Error "opencv_contrib (cudev) not found: $ContribDir\modules\cudev"
    exit 2
}

Write-Host "== configure (CUDA + python3 bindings) =="
New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
Push-Location $BuildDir
try {
    & $py -m cmake -G "Visual Studio 18 2026" -A x64 `
        "-DBUILD_LIST=core,imgproc,imgcodecs,python3,cudev,cudaarithm,cudaimgproc,cudafilters" `
        "-DWITH_CUDA=ON" "-DCUDA_ARCH_BIN=8.6" "-DCUDA_FAST_MATH=OFF" `
        "-DCMAKE_POLICY_DEFAULT_CMP0219=NEW" `
        "-DCUDA_TOOLKIT_ROOT_DIR=$cudaFwd" `
        "-DCUDA_NVCC_FLAGS=--allow-unsupported-compiler" `
        "-DOPENCV_EXTRA_MODULES_PATH=$contribFwd" `
        "-DNASM=$nasmFwd" `
        "-DBUILD_opencv_python3=ON" "-DPYTHON3_EXECUTABLE=$py" `
        "-DPYTHON3_INCLUDE_DIR=$pyInc" "-DPYTHON3_LIBRARY=$pyLib" `
        "-DBUILD_TESTS=OFF" "-DBUILD_PERF_TESTS=OFF" "-DBUILD_EXAMPLES=OFF" "-DBUILD_DOCS=OFF" `
        "-DCMAKE_INSTALL_PREFIX=$BuildDir\install" `
        $SrcDir
    if ($LASTEXITCODE -ne 0) { Write-Error "cmake configure failed"; exit 3 }

    Write-Host "== build opencv_python3 (Release, $Jobs jobs) =="
    & $py -m cmake --build . --config Release --target opencv_python3 --parallel $Jobs
    if ($LASTEXITCODE -ne 0) { Write-Error "build failed"; exit 4 }

    Write-Host "== install =="
    & $py -m cmake --install . --config Release

    $pyd = Get-ChildItem -Recurse -Filter "cv2*.pyd" $BuildDir | Select-Object -First 1
    Write-Host "artifact: $($pyd.FullName)"
    $verify = @"
import sys
sys.path.insert(0, r"$($pyd.DirectoryName)")
import cv2
print("cv2", cv2.__version__)
print("CUDA devices:", cv2.cuda.getCudaEnabledDeviceCount())
"@
    $verify | & $py -
    Write-Host "== DONE =="
} finally {
    Pop-Location
}
