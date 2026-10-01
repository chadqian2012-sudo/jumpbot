# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller 打包配置（onedir）。

产物：dist/jumpbot/jumpbot.exe + templates/ + tools/dashboard.py + 可选 CUDA 运行时。
环境变量 JUMPBOT_WITH_GPU=0 可打"轻量版"（不带 CuPy/CUDA，自动回退 CPU SIMD）。
"""
import os
import shutil
from pathlib import Path

ROOT = Path(SPECPATH).resolve().parent          # tools/ 的父目录
WITH_GPU = os.environ.get("JUMPBOT_WITH_GPU", "1") == "1"

datas = [
    (str(ROOT / "templates"), "templates"),
    (str(ROOT / "config.json"), "."),
    (str(ROOT / "tools" / "dashboard.py"), "tools"),
    (str(ROOT / "tools" / "_scorelog.py"), "tools"),
]
binaries = []
hiddenimports = ["mss", "mss.windows"]

if WITH_GPU:
    try:
        import cupy
        from PyInstaller.utils.hooks import collect_submodules, collect_data_files

        hiddenimports += collect_submodules("cupy")
        # CuPy 用 DeferredImport 做延迟加载，有些子包（cupy_backends / cupyx）
        # 是独立顶层包，collect_submodules('cupy') 抓不到 → 打包后报
        # ModuleNotFoundError: cupy_backends.cuda._softlink。
        # cupy_backends / cupyx 是独立顶层包；cuda.* 来自 nvidia-cuda-runtime-cu12
        # （cupy 会派生 `python -m cuda.pathfinder...` 子进程去探测动态库）
        for pkg in ("cupy_backends", "cupyx", "cuda"):
            try:
                hiddenimports += collect_submodules(pkg)
            except Exception as exc:
                print(f"[spec] 跳过 {pkg}: {exc}")
        # collect_submodules 对 cuda.pathfinder._dynamic_libs.* 这类动态加载的模块枚举不到，
        # 直接按目录把 cuda 包下所有 .py 都显式列出来（打包后子进程要能 -m 运行它们）。
        try:
            import cuda as _cuda
            cuda_root = Path(_cuda.__file__).resolve().parent
            for py in cuda_root.rglob("*.py"):
                parts = list(py.relative_to(cuda_root).with_suffix("").parts)
                if parts and parts[-1] == "__init__":
                    parts = parts[:-1]
                if parts:
                    hiddenimports.append("cuda." + ".".join(parts))
            print(f"[spec] cuda 包显式收集模块 {len(hiddenimports)} 项（累计）")
        except Exception as exc:
            print(f"[spec] cuda 包枚举失败: {exc}")
        # CuPy 的动态导入还会用到这些"静态分析看不见"的标准库
        hiddenimports += ["graphlib", "plistlib", "typing_extensions", "importlib.metadata",
                          "importlib.resources", "ctypes.util"]
        datas += collect_data_files("cupy")
        # 只带 CUDA 运行时需要的两个组件（cudart + nvrtc），
        # cublas/cusolver/cufft/npp 共 2.6GB，掩码计算完全用不到。
        nv_root = Path(cupy.__file__).resolve().parent.parent / "nvidia"
        for comp in ("cuda_runtime", "cuda_nvrtc"):
            for dll in (nv_root / comp / "bin").glob("*.dll"):
                binaries.append((str(dll), f"nvidia/{comp}/bin"))
    except Exception as exc:                     # pragma: no cover
        print(f"[spec] 未启用 GPU 打包：{exc}")

a = Analysis(
    [str(ROOT / "jumpbot.py")],
    pathex=[str(ROOT)],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    excludes=["matplotlib", "pandas", "scipy", "tkinter", "PyQt5", "PySide2", "IPython"],
    noarchive=False,
)
pyz = PYZ(a.pure)
exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="jumpbot",
    debug=False,
    strip=False,
    upx=False,
    console=True,
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="jumpbot",
)
