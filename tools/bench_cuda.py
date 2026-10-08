#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""CUDA 离线 A/B：旧提交 vs 当前实现，包含传输/下载的端到端计时。

python tools/bench_cuda.py --reference fdfcd9b --out build/cuda_benchmark.json
只从本地可信 Git 提交提取 Vision._masks_gpu，不执行该提交的其他代码。
全程不接触游戏窗口、不发送输入；GPU 不可用即失败，禁止把 CPU 回退算成 GPU。
"""
from __future__ import annotations

import argparse
import ast
import dataclasses
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time
import types
from pathlib import Path

os.environ["JUMPBOT_NO_INPUT"] = "1"
PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import cv2
import numpy as np
import jumpbot as J
from aim_error import looks_annotated
from regress import is_game_frame


def reference_method(ref):
    source = subprocess.run(["git", "show", f"{ref}:jumpbot.py"], cwd=PROJECT_DIR,
                            check=True, capture_output=True, encoding="utf-8").stdout
    cls = next(n for n in ast.parse(source).body
               if isinstance(n, ast.ClassDef) and n.name == "Vision")
    method = next(n for n in cls.body
                  if isinstance(n, ast.FunctionDef) and n.name == "_masks_gpu")
    scope = dict(vars(J))
    exec(compile(ast.Module(body=[method], type_ignores=[]), f"git:{ref}", "exec"), scope)
    return scope["_masks_gpu"], hashlib.sha256(source.encode()).hexdigest()


def require_gpu(vision, frame):
    out = vision._masks_gpu(frame)
    if out is None or not vision._gpu_state:
        raise RuntimeError("真实 CUDA 路径不可用；停止测试，不计入 CPU 回退耗时")
    return out


def assert_masks(want, got, name):
    for label, a, b in zip(("piece", "block"), want, got):
        if a.dtype != b.dtype or a.shape != b.shape or not np.array_equal(a, b):
            raise AssertionError(f"{name} {label} 掩码不一致")


def paired_times(old_fn, new_fn, frames, repeat):
    samples = {"before": [], "after": []}
    for frame in frames:
        for _ in range(3):
            old_fn(frame)
            new_fn(frame)
    # 交错且每轮反转顺序，减少 GPU 负载/温度随时间变化引入的偏差。
    for i in range(repeat):
        funcs = [("before", old_fn), ("after", new_fn)]
        if i % 2:
            funcs.reverse()
        for frame in frames:
            for key, fn in funcs:
                t0 = time.perf_counter()
                fn(frame)  # _masks_gpu 的下载为阻塞式，返回时 GPU 结果已完成。
                samples[key].append((time.perf_counter() - t0) * 1000)
    result = {key: {"median_ms": statistics.median(values),
                    "p90_ms": float(np.percentile(values, 90)),
                    "samples": len(values)} for key, values in samples.items()}
    result["speedup"] = result["before"]["median_ms"] / result["after"]["median_ms"]
    return result


def verify_hsv(vision):
    """穷举全部 256^3 种 uint8 BGR，逐通道核对 CUDA HSV 与本机 OpenCV。"""
    cp = vision._cp
    kernel = cp.RawKernel(vision._gpu_kernel.code + r'''
        extern "C" __global__ void verify_hsv(
            const unsigned char* bgr, const int* tables, unsigned char* out, int n)
        {
            int i = blockIdx.x * blockDim.x + threadIdx.x;
            if (i >= n) return;
            int h, s, v;
            jumpbot_bgr_hsv(bgr + 3 * i, tables, h, s, v);
            out[3 * i] = h; out[3 * i + 1] = s; out[3 * i + 2] = v;
        }
    ''', "verify_hsv", options=("--std=c++11",))
    batch = 1 << 20
    for start in range(0, 1 << 24, batch):
        colors = np.arange(start, start + batch, dtype=np.uint32)
        frame = np.stack((colors & 255, (colors >> 8) & 255, colors >> 16), axis=1)
        frame = frame.astype(np.uint8).reshape(1024, 1024, 3)
        gpu_in = cp.asarray(frame)
        gpu_out = cp.empty_like(gpu_in)
        kernel(((batch + 255) // 256,), (256,),
               (gpu_in, vision._gpu_hsv_tables, gpu_out, np.int32(batch)))
        if not np.array_equal(gpu_out.get(), cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)):
            raise AssertionError(f"HSV 穷举不一致，颜色区间 {start}..{start + batch}")
    print("CUDA HSV vs OpenCV：全部 16,777,216 种 BGR 颜色逐通道一致", flush=True)


def verify_runtime(vision, frame):
    cp = vision._cp
    # 自有非默认流不能被默认流同步遗漏。
    with cp.cuda.Stream(non_blocking=True):
        assert_masks(vision._masks_simd(frame), require_gpu(vision, frame), "非默认流")
    require_gpu(vision, frame)
    pool = cp.get_default_memory_pool()
    before = pool.used_bytes()
    for _ in range(1000):
        require_gpu(vision, frame)
    after = pool.used_bytes()
    if before != after:
        raise AssertionError(f"设备缓冲区持续增长：{before} → {after}")
    # 内核执行异常必须即刻回退，状态面板也要显示真实后端。
    saved_kernel, saved_cfg = vision._gpu_kernel, vision.cfg
    vision.cfg = dataclasses.replace(saved_cfg, masks_backend="auto")
    vision._auto_backend = "gpu"

    def broken_kernel(*_args):
        raise RuntimeError("测试注入的 CUDA 内核异常")

    vision._gpu_kernel = broken_kernel
    assert_masks(vision._masks_simd(frame), vision.masks(frame), "CUDA 异常回退")
    if vision._auto_backend != "simd" or vision._gpu_state is not False:
        raise AssertionError("CUDA 异常后的后端状态不正确")
    vision._gpu_kernel, vision.cfg = saved_kernel, saved_cfg
    print(f"非默认流/异常回退/1000 帧缓冲复用通过；设备使用 {before} → {after} bytes", flush=True)
    return {"iterations": 1000, "used_bytes_before": before, "used_bytes_after": after}


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--reference", default="fdfcd9b")
    ap.add_argument("--repeat", type=int, default=15)
    ap.add_argument("--max-frames", type=int, default=0, help="0=全部干净历史帧")
    ap.add_argument("--out", type=Path, default=PROJECT_DIR / "build/cuda_benchmark.json")
    ap.add_argument("--verify-hsv", action="store_true", help="穷举 256^3 种 BGR，验证融合 HSV 转换")
    args = ap.parse_args()
    if args.repeat < 1 or args.max_frames < 0:
        ap.error("repeat 必须 >=1，max-frames 必须 >=0")
    cfg = dataclasses.replace(J.Config.load(PROJECT_DIR / "config.json"),
                              masks_backend="gpu", masks_simd=True)
    cv2.setNumThreads(J.effective_cv2_threads(cfg.cv2_threads))
    old_method, source_sha = reference_method(args.reference)
    old, new = J.Vision(cfg), J.Vision(cfg)
    old._masks_gpu = types.MethodType(old_method, old)
    paths = sorted(p for p in (PROJECT_DIR / "debug").rglob("*.png") if is_game_frame(p))
    checked, skipped, perf_frames, fingerprints = 0, 0, [], []
    for path in paths:
        frame = cv2.imread(str(path))
        if frame is None or min(frame.shape[:2]) < 250 or looks_annotated(frame):
            skipped += 1
            continue
        want, got = require_gpu(old, frame), require_gpu(new, frame)
        assert_masks(want, got, path.name)
        assert_masks(new._masks_simd(frame), got, path.name + " SIMD")
        old.reset_prior()
        new.reset_prior()
        if old.detect(frame)[:2] != new.detect(frame)[:2]:
            raise AssertionError(f"{path.name} 棋子/目标/瞄点识别结果不一致")
        checked += 1
        fingerprints.append((str(path.relative_to(PROJECT_DIR)), hashlib.sha256(frame.tobytes()).hexdigest()))
        # 按目录挑代表帧，避免计时样本全部来自同一轮录制。
        if len(perf_frames) < 8 and path.parent not in [p for p, _f in perf_frames]:
            perf_frames.append((path.parent, frame))
        if args.max_frames and checked >= args.max_frames:
            break
    if not checked:
        raise RuntimeError("没有可用的干净原始历史帧")
    print(f"历史帧逐像素/完整识别 A/B：{checked} 张一致；剔除 {skipped} 张派生/标注图", flush=True)

    rng = np.random.default_rng(20261007)
    cases = [rng.integers(0, 256, (h, w, 3), dtype=np.uint8)
             for h, w in ((37, 23), (127, 257), (1050, 553), (950, 500), (1050, 553))]
    cases.extend([np.full((71, 93, 3), c, np.uint8) for c in (0, 25, 55, 110, 160, 238, 245, 255)])
    # 非连续的裁剪/翻转帧、尺寸往返、画布截断和亮块开关。
    cases += [cases[2][:, ::2], cases[2][::-1, ::-1]]
    retained = require_gpu(new, cases[0])
    saved = tuple(m.copy() for m in retained)
    synthetic = 0
    for overrides in ({}, {"block_bright_delta": 12},
                      {"piece_hue_min": 0, "piece_hue_max": 179,
                       "block_color_delta": 765, "block_pale_delta": 255},
                      {"top_bar_ratio": -0.1, "bottom_bar_ratio": 0.2},
                      {"top_bar_ratio": 0.8, "bottom_bar_ratio": 0.4}):
        old.cfg = new.cfg = dataclasses.replace(cfg, **overrides)
        for frame in cases:
            assert_masks(require_gpu(old, frame), require_gpu(new, frame), "合成边界")
            synthetic += 1
    assert_masks(saved, retained, "返回结果不能被下一帧复用缓冲区覆盖")
    old.cfg = new.cfg = cfg
    print(f"合成边界/尺寸切换/非连续输入：{synthetic} 例通过", flush=True)
    if args.verify_hsv:
        verify_hsv(new)

    frames = [f for _p, f in perf_frames]
    masks = paired_times(lambda f: require_gpu(old, f), lambda f: require_gpu(new, f),
                         frames, args.repeat)

    def detect(v, f):
        v.reset_prior()
        return v.detect(f)

    detect_result = paired_times(lambda f: detect(old, f), lambda f: detect(new, f),
                                frames, args.repeat)
    result = {"reference": args.reference, "reference_source_sha256": source_sha,
              "config": dataclasses.asdict(cfg), "device": new._cp.cuda.runtime.getDeviceProperties(0)["name"].decode(),
              "cupy": new._cp.__version__, "opencv": cv2.__version__,
              "verified_frames": checked, "synthetic_cases": synthetic,
              "frame_fingerprints": fingerprints, "masks": masks, "detect": detect_result,
              "hsv_colors_verified": (1 << 24) if args.verify_hsv else 0,
              "runtime": verify_runtime(new, frames[0])}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    for name, data in (("masks", masks), ("detect", detect_result)):
        print(f"{name}: {data['before']['median_ms']:.3f} → {data['after']['median_ms']:.3f} ms"
              f"，{data['speedup']:.2f}x；p90 {data['before']['p90_ms']:.3f} → {data['after']['p90_ms']:.3f} ms")
    print(f"详细结果：{args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
