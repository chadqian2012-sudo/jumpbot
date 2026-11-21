"""Offline full detection parity and backend benchmark. Run from an x64 MSVC developer shell.

    python tools/bench_native.py --baseline-only
    python tools/bench_native.py --native desktop/target/release/jumpbot.exe

Requires both pinned Git objects, Python with OpenCV and NumPy for the references,
and rustc (or --rustc PATH) to rebuild the original desktop acceleration DLL.
The native CLI needs its release DLLs and templates. No capture or input.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import io
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time
import zipfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
REFS = {"8003412": "800341264bf3f326f206ab2e35178fb420874b7b",
        "v1.3": "c472b05ec3d732e3158185801b5659a0a476b48c"}
FIELDS = ("piece", "target", "piece_mask_crc32", "block_mask_crc32", "score")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def reference_worker(src, config, repeat, images):
    # Import each immutable reference only in its own subprocess.
    sys.path.insert(0, str(src))
    import cv2
    import jumpbot as bot

    cfg = bot.Config.load(config)
    cv2.setNumThreads(cfg.cv2_threads)
    cv2.ocl.setUseOpenCL(False)
    vision = bot.Vision(cfg)
    if src.name == "8003412" and (bot._EDGE_MEDIANS is None or bot._FIT_VERTEX is None):
        raise RuntimeError("8003412 must load both original Rust kernels")
    for image in images:
        frame = cv2.imread(str(image))
        if frame is None:
            raise ValueError(f"Cannot decode {image}")
        samples = []
        with contextlib.redirect_stdout(sys.stderr):
            for iteration in range(repeat + 1):
                vision.reset_prior()
                start = time.perf_counter_ns()
                piece, target, pm, bm = vision.detect(frame)
                elapsed = time.perf_counter_ns() - start
                if iteration:
                    samples.append(elapsed / 1000)
        row = {"image": str(image), "backend": vision._last_mask_backend,
               "detect_us": samples, "piece_mask_crc32": zlib.crc32(pm),
               "block_mask_crc32": zlib.crc32(bm), "score": bot.read_score(frame, cfg)}
        for key, obj in (("piece", piece), ("target", target)):
            row[key] = None if obj is None else {
                "x": float(obj.x), "y": float(obj.y), "bbox": [int(v) for v in obj.bbox]}
        if piece is not None:
            row["piece"]["source"] = piece.source
        if target is not None:
            row["target"]["top_width"] = float(target.top_width)
            row["target"]["dot"] = None if target.dot is None else list(map(float, target.dot))
        print(json.dumps(row, allow_nan=False))


def reference_sources(stage, rustc):
    files = ["jumpbot.py", "skin_vision.py", "run_control.py", "config.json", "templates"]
    for name, revision in REFS.items():
        extra = ["tests/fixtures", "desktop/vision/src/lib.rs"] if name == "8003412" else []
        archive = subprocess.check_output(
            ["git", "archive", "--format=zip", revision, "--", *files, *extra], cwd=ROOT)
        with zipfile.ZipFile(io.BytesIO(archive)) as z:
            z.extractall(stage / name)
    ref = stage / "8003412"
    subprocess.run([rustc, "--edition=2021", "--crate-type=cdylib", "--crate-name=jumpbot_vision",
                    "-Copt-level=3", "-Clto=thin", "--out-dir", ref,
                    ref / "desktop/vision/src/lib.rs"], check=True)
    return ref


def samples_for(command, env, images, repeat, cwd, backend):
    proc = subprocess.run(command, env=env, cwd=cwd, capture_output=True,
                          encoding="utf-8", errors="replace", timeout=300)
    if proc.returncode:
        raise RuntimeError(f"{command[0]} exited {proc.returncode}: {proc.stderr}\n{proc.stdout}")
    expected = {image.name for image in images}
    rows = {}
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        name = Path(row["image"]).name
        if name in rows or name not in expected or ("cuda" if row["backend"] == "gpu" else row["backend"]) != backend:
            raise ValueError(f"Unexpected/duplicate image or backend: {name}, {row['backend']}")
        times = row["detect_us"]
        if len(times) != repeat or not all(type(t) in (float, int) and t > 0 and math.isfinite(t)
                                           for t in times):
            raise ValueError(f"{name}: expected {repeat} positive finite full-detection samples")
        for key in ("piece_mask_crc32", "block_mask_crc32"):
            value = row[key]
            if isinstance(value, str):
                row[key] = int(value, 16)
            if type(row[key]) is not int or not 0 <= row[key] <= 0xffffffff:
                raise ValueError(f"{name}: invalid {key}")
        clean = {k: row[k] for k in FIELDS}
        if row["score"] is not None and type(row["score"]) is not int:
            raise ValueError(f"{name}: invalid score")
        for k in ("piece", "target"):
            if (obj := row[k]) is None:
                continue
            keys = ("x", "y", "bbox", "top_width", "dot") if k == "target" else ("x", "y", "bbox", "source")
            clean[k] = {field: obj[field] for field in keys}
            if len(obj["bbox"]) != 4 or any(type(v) is not int for v in obj["bbox"]):
                raise ValueError(f"{name}: expected integer xywh bbox")
            if k == "target" and obj["dot"] is not None:
                dot = obj["dot"]
                if len(dot) != 3 or any(type(v) not in (float, int) or not math.isfinite(v) for v in dot):
                    raise ValueError(f"{name}: invalid center dot")
            for key in set(keys) - {"bbox", "source", "dot"}:
                if type(obj[key]) not in (float, int) or not math.isfinite(obj[key]):
                    raise ValueError(f"{name}: invalid {k}.{key}")
        rows[name] = {**clean, "detect_us": times}
    if rows.keys() != expected:
        raise ValueError(f"Missing images: {sorted(expected - rows.keys())}")
    return rows


def differences(want, got):
    for key in FIELDS:
        a, b = want[key], got[key]
        if isinstance(a, dict) and isinstance(b, dict):
            for field, value in a.items():
                other = b[field]
                if field == "dot" and value is not None and other is not None:
                    equal = all(math.isclose(x, y, rel_tol=0, abs_tol=0.001) for x, y in zip(value, other))
                elif field in ("bbox", "source", "dot"):
                    equal = value == other
                else:
                    equal = math.isclose(value, other, rel_tol=0, abs_tol=0.001)
                if not equal:
                    yield f"{key}.{field}"
        elif a != b:
            yield key


def benchmark(args, stage):
    import cv2
    import numpy

    ref = reference_sources(stage, args.rustc)
    images = sorted((ref / "tests/fixtures").glob("*.png"))
    if len(images) != 17:
        raise ValueError("Incomplete corpus in the pinned commit")
    # The labelled positive counterpart to the two real cup-highlight rejection cases.
    dot = cv2.imread(str(ref / "tests/fixtures/purple-cylinder-stall.png"))
    cv2.circle(dot, (313, 555), 15, (160, 160, 160), -1)
    cv2.circle(dot, (313, 555), 5, (255, 255, 255), -1)
    corrected = stage / "purple-cylinder-valid-dot.png"
    if not cv2.imwrite(str(corrected), dot):
        raise OSError(f"Cannot write {corrected}")
    images.append(corrected)
    config = stage / "config.json"
    settings = json.loads((ref / "config.json").read_text(encoding="utf-8"))
    settings.update(cv2_threads=4, masks_backend=args.backend, cuda_enabled=args.backend == "cuda",
                    opencl_enabled=args.backend == "opencl")
    config.write_text(json.dumps(settings), encoding="utf-8")
    base_env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("JUMPBOT_", "PYTHON"))}
    base_env.update(JUMPBOT_NO_INPUT="1", JUMPBOT_DISABLE_CUDA=str(int(args.backend != "cuda")),
                    JUMPBOT_DISABLE_OPENCL=str(int(args.backend != "opencl")),
                    PYTHONIOENCODING="utf-8", PYTHONDONTWRITEBYTECODE="1")
    engines = list(REFS)
    if args.native:
        engines.append("native")
    report = {"commits": REFS, "config": settings, "sha256": {p.name: digest(p) for p in images},
              "host": platform.platform(), "cpu": platform.processor(), "python": sys.version,
              "opencv_reference": cv2.__version__, "numpy": numpy.__version__,
              "rustc": subprocess.check_output([args.rustc, "--version"], text=True).strip(),
              "baseline_dll_sha256": digest(ref / "jumpbot_vision.dll"),
              "native_sha256": digest(args.native) if args.native else None,
              "repeat": args.repeat, "orders": [], "rows": {name: {} for name in engines},
              "differences": []}
    # Rotate which engine runs first, and reverse the shared frame order on alternate passes.
    for i in range(len(engines)):
        order = engines[i:] + engines[:i]
        batch = images if i % 2 == 0 else images[::-1]
        report["orders"].append(order)
        for name in order:
            env = dict(base_env)
            if name == "native":
                cmd = [args.native, "--config", config, "--backend", args.backend, "--bench-repeat",
                       str(args.repeat), *(arg for p in batch for arg in ("--image", p))]
                cwd = args.native.parent
            else:
                cwd = stage / name
                cmd = [sys.executable, Path(__file__).resolve(), "--reference-worker", cwd,
                       config, str(args.repeat), *batch]
                if name == "8003412":
                    env["JUMPBOT_VISION_DLL"] = str(ref / "jumpbot_vision.dll")
            for image, row in samples_for(cmd, env, batch, args.repeat, cwd, args.backend).items():
                previous = report["rows"][name].get(image)
                if previous is None:
                    report["rows"][name][image] = row
                else:
                    fields = list(differences(previous, row))
                    if fields:
                        report["differences"].append([name, "own previous pass", image, fields])
                    previous["detect_us"].extend(row["detect_us"])
    for base, candidate in [("v1.3", "8003412")] + [(r, "native") for r in REFS if args.native]:
        for p in images:
            fields = list(differences(report["rows"][base][p.name], report["rows"][candidate][p.name]))
            if fields:
                report["differences"].append([base, candidate, p.name, fields])

    # Pinned, independently labelled cases from test_target_dots.py and test_skins.py.
    for engine, rows in report["rows"].items():
        for name in ("purple-horizontal-fall.png", "skin-gold-falling.png"):
            if rows[name]["piece"] is not None or rows[name]["target"] is not None:
                report["differences"].append([engine, "label", name, ["fall must have no piece or target"]])
        for name in ("purple-cup-before-death.png", "purple-cylinder-stall.png"):
            target = rows[name]["target"]
            if target is None or target["dot"] is not None:
                report["differences"].append([engine, "label", name, ["cup highlight is not a target dot"]])
        target = rows[corrected.name]["target"]
        if (not target or not target["dot"] or abs(target["x"] - 313) > 2 or abs(target["y"] - 555) > 2):
            report["differences"].append([engine, "label", corrected.name, ["valid white dot at 313,555"]])

    report["summary"] = {}
    for engine, rows in report["rows"].items():
        all_us = [t for row in rows.values() for t in row["detect_us"]]
        report["summary"][engine] = {
            "samples": len(all_us), "mean_frame_median_ms": statistics.mean(
                statistics.median(row["detect_us"]) for row in rows.values()) / 1000,
            "p50_ms": statistics.median(all_us) / 1000,
            "p95_ms": float(numpy.percentile(all_us, 95)) / 1000}
    if args.native:
        if digest(args.native) != report["native_sha256"]:
            raise RuntimeError("Native binary changed during the benchmark")
        current = report["summary"]["native"]["mean_frame_median_ms"]
        report["native_speedup"] = {r: report["summary"][r]["mean_frame_median_ms"] / current for r in REFS}
        report["native_opencv_dlls"] = {p.name: digest(p) for p in args.native.parent.glob("opencv_world*.dll")}
    return report


def main():
    if sys.argv[1:2] == ["--reference-worker"]:
        reference_worker(Path(sys.argv[2]), Path(sys.argv[3]), int(sys.argv[4]),
                         [Path(p) for p in sys.argv[5:]])
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--baseline-only", action="store_true")
    mode.add_argument("--native", type=Path, help="Release jumpbot.exe with its runtime DLLs")
    parser.add_argument("--backend", choices=("cpu", "simd", "cuda", "opencl"), default="simd",
                        help="Compare this exact backend. An unexpected fallback fails the run.")
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--repeat", type=int, default=5, help="Timed detections per frame and pass")
    parser.add_argument("--out", type=Path, help="Default: build/native-bench[-baseline].json")
    args = parser.parse_args()
    if args.repeat < 1:
        parser.error("--repeat must be positive")
    if args.native:
        args.native = args.native.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="jumpbot-bench-") as work:
        report = benchmark(args, Path(work))
    out = args.out or ROOT / "build" / ("native-bench.json" if args.native else "native-bench-baseline.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, allow_nan=False), encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))
    print(f'{len(report["differences"])} differences. Details: {out}')
    return int(bool(report["differences"]))


if __name__ == "__main__":
    sys.exit(main())
