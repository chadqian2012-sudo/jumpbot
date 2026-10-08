#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""跨后端离线验证/端到端基准：CPU、SIMD、CUDA、各 OpenCL GPU。

python tools/bench_backends.py --require-opencl NVIDIA Intel --verify-hsv
参考快照默认 build/reference_portable/jumpbot.py；全程禁止真实输入。
"""
from __future__ import annotations

import argparse
import ast
import builtins
import dataclasses
import hashlib
import json
import os
import sys
import types
from pathlib import Path
from unittest.mock import patch

os.environ["JUMPBOT_NO_INPUT"] = "1"
PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import cv2
import numpy as np
import jumpbot as J
from aim_error import looks_annotated
from bench_cuda import assert_masks, paired_times
from regress import is_game_frame


def reference_vision(cfg, path):
    source = path.read_text(encoding="utf-8")
    cls = next(n for n in ast.parse(source).body
               if isinstance(n, ast.ClassDef) and n.name == "Vision")
    method = next(n for n in cls.body
                  if isinstance(n, ast.FunctionDef) and n.name == "_masks_simd")
    scope = dict(vars(J))
    exec(compile(ast.Module(body=[method], type_ignores=[]), str(path), "exec"), scope)
    vision = J.Vision(dataclasses.replace(cfg, masks_backend="simd"))
    vision._masks_simd = types.MethodType(scope["_masks_simd"], vision)
    return vision, hashlib.sha256(source.encode()).hexdigest()


def compute(name, vision, frame):
    if name == "cuda":
        result = vision._masks_gpu(frame)
    elif name.startswith("opencl:"):
        result = vision._masks_opencl(frame)
    else:
        result = vision.masks(frame)
    if result is None:
        raise RuntimeError(f"{name} 实际不可用，不能把回退结果算作加速结果")
    return result


def check_opencl_hsv(vision):
    cl, rt = vision._cl, vision._cl_runtime
    # 从二进制缓存构建的程序在部分驱动上不保留 CL_PROGRAM_SOURCE。
    source = rt["source"] + r'''
        __kernel void verify_hsv(__global const uchar* bgr, __constant const int* tables,
                                 __global uchar* out, int n) {
            int i = get_global_id(0); if (i >= n) return;
            Hsv c = jumpbot_bgr_hsv(bgr + 3*i, tables);
            out[3*i] = c.h; out[3*i+1] = c.s; out[3*i+2] = c.v;
        }
    '''
    program = cl.Program(rt["context"], source).build(
        options=["-cl-std=CL1.2"], cache_dir=str(J.DEBUG_DIR / "opencl_cache"))
    kernel = cl.Kernel(program, "verify_hsv")
    batch = 1 << 20
    gpu_in = cl.Buffer(rt["context"], cl.mem_flags.READ_ONLY, size=batch*3)
    gpu_out = cl.Buffer(rt["context"], cl.mem_flags.WRITE_ONLY, size=batch*3)
    for start in range(0, 1 << 24, batch):
        colors = np.arange(start, start+batch, dtype=np.uint32)
        frame = np.stack((colors & 255, (colors >> 8) & 255, colors >> 16), axis=1)
        frame = frame.astype(np.uint8).reshape(1024, 1024, 3)
        got = np.empty_like(frame)
        cl.enqueue_copy(rt["queue"], gpu_in, frame, is_blocking=False)
        kernel(rt["queue"], (batch,), None, gpu_in, rt["tables"], gpu_out, np.int32(batch))
        cl.enqueue_copy(rt["queue"], got, gpu_out, is_blocking=True)
        if not np.array_equal(got, cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)):
            raise AssertionError(f"{rt['name']} HSV 穷举失败，颜色起点 {start}")
    print(f"{rt['name']}：16,777,216 种 BGR 的 HSV 逐通道一致", flush=True)


def fallback_checks(cfg, frame):
    expected = J.Vision(dataclasses.replace(cfg, masks_backend="simd")).masks(frame)
    real_import = builtins.__import__

    def no_accelerators(name, *args, **kwargs):
        if name.split(".")[0] in ("cupy", "pyopencl"):
            raise ImportError("测试模拟：未安装可选 GPU 依赖")
        return real_import(name, *args, **kwargs)

    with patch("builtins.__import__", side_effect=no_accelerators):
        v = J.Vision(dataclasses.replace(cfg, masks_backend="auto"))
        assert_masks(expected, v.masks(frame), "缺少 GPU 库时 CPU 回退")
        if v._auto_backend != "simd":
            raise AssertionError("缺少可选库却报告 GPU 后端")
    for backend in ("gpu", "opencl"):
        v = J.Vision(dataclasses.replace(cfg, masks_backend=backend))
        if backend == "gpu":
            v._masks_gpu = lambda _f: None
        else:
            v._masks_opencl = lambda _f: None
        assert_masks(expected, v.masks(frame), backend + " 失败回退")
        if v._last_mask_backend != "simd":
            raise AssertionError("回退后的实际后端标记不正确")
    v = J.Vision(dataclasses.replace(cfg, masks_backend="auto", cuda_enabled=False,
                                     opencl_enabled=False, masks_simd=False))
    assert_masks(expected, v.masks(frame), "禁用所有加速时纯 NumPy CPU")
    if v._auto_backend != "cpu":
        raise AssertionError("纯 CPU 选择未生效")
    v = J.Vision(dataclasses.replace(cfg, masks_backend="opencl", opencl_device="不存在的设备XYZ"))
    assert_masks(expected, v.masks(frame), "设备选择器不匹配时回退")
    print("缺依赖/禁用加速/显式 GPU 失败/无匹配设备回退：通过", flush=True)


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--reference", type=Path, default=PROJECT_DIR / "build/reference_portable/jumpbot.py")
    ap.add_argument("--out", type=Path, default=PROJECT_DIR / "build/backends_benchmark.json")
    ap.add_argument("--require-opencl", nargs="*", default=[])
    ap.add_argument("--verify-hsv", action="store_true")
    ap.add_argument("--repeat", type=int, default=15)
    args = ap.parse_args()
    if args.repeat < 1:
        ap.error("repeat 必须 >=1")
    cfg = J.Config.load(PROJECT_DIR / "config.json")
    cv2.setNumThreads(J.effective_cv2_threads(cfg.cv2_threads))
    reference, source_sha = reference_vision(cfg, args.reference)
    visions = {name: J.Vision(dataclasses.replace(cfg, masks_backend=backend))
               for name, backend in (("cpu", "cpu"), ("simd", "simd"), ("cuda", "gpu"))}
    selectors = list(args.require_opencl) or [""]
    for selector in selectors:
        visions["opencl:" + (selector or "auto")] = J.Vision(
            dataclasses.replace(cfg, masks_backend="opencl", opencl_device=selector))
    paths = sorted(p for p in (PROJECT_DIR / "debug").rglob("*.png") if is_game_frame(p))
    samples, verified, fingerprints = [], 0, []
    unavailable = []
    for path in paths:
        frame = cv2.imread(str(path))
        if frame is None or min(frame.shape[:2]) < 250 or looks_annotated(frame):
            continue
        want = reference.masks(frame)
        reference.reset_prior()
        want_detect = reference.detect(frame)[:2]
        for name, v in list(visions.items()):
            try:
                got = compute(name, v, frame)
            except RuntimeError:
                if name.startswith("opencl:") and args.require_opencl:
                    raise
                if verified:
                    raise
                unavailable.append(name)
                del visions[name]
                continue
            assert_masks(want, got, f"{path.name} {name}")
            v.reset_prior()
            if v.detect(frame)[:2] != want_detect:
                raise AssertionError(f"{path.name} {name} 棋子/目标/瞄点不同")
        verified += 1
        fingerprints.append((str(path.relative_to(PROJECT_DIR)), hashlib.sha256(frame.tobytes()).hexdigest()))
        if len(samples) < 8 and path.parent not in [p for p, _f in samples]:
            samples.append((path.parent, frame))
    if not samples:
        raise RuntimeError("没有干净历史帧")
    print(f"全部 {verified} 张历史帧：{list(visions)} 掩码/完整识别与原 SIMD 相同", flush=True)

    rng = np.random.default_rng(7)
    cases = [rng.integers(0, 256, (h,w,3), dtype=np.uint8)
             for h,w in ((37,23), (127,257), (1050,553), (950,500), (1050,553))]
    cases += [cases[2][:,::2], cases[2][::-1,::-1]]
    retained = {name: compute(name,v,cases[0]) for name,v in visions.items()}
    copies = {name: tuple(m.copy() for m in result) for name,result in retained.items()}
    synthetic = 0
    for overrides in ({}, {"block_bright_delta":12}, {"block_color_delta":765},
                      {"top_bar_ratio":-0.1, "bottom_bar_ratio":0.2},
                      {"top_bar_ratio":0.8, "bottom_bar_ratio":0.4}):
        reference.cfg = dataclasses.replace(reference.cfg, **overrides)
        for v in visions.values():
            v.cfg = dataclasses.replace(v.cfg, **overrides)
        for frame in cases:
            want = reference.masks(frame)
            for name,v in visions.items():
                assert_masks(want, compute(name,v,frame), "边界 " + name)
            synthetic += 1
        # 每组恢复全部配置，防止上一组覆盖流入下一组。
        reference.cfg = dataclasses.replace(cfg, masks_backend="simd")
        for name,v in visions.items():
            v.cfg = dataclasses.replace(cfg, masks_backend=("gpu" if name == "cuda" else
                                       "opencl" if name.startswith("opencl:") else name),
                                       opencl_device=name.split(":",1)[1] if name.startswith("opencl:") else "")
    for name in retained:
        assert_masks(copies[name], retained[name], "结果生命周期 " + name)
    print(f"{synthetic} 组合边界/非连续输入/尺寸往返/结果生命周期：通过", flush=True)
    if args.verify_hsv:
        for name,v in visions.items():
            if name.startswith("opencl:"):
                check_opencl_hsv(v)
    frames = [f for _p,f in samples]
    results = {}
    for name,v in visions.items():
        masks = paired_times(reference.masks, lambda f, n=name, vv=v: compute(n,vv,f), frames, args.repeat)

        def detect(vv, f):
            vv.reset_prior()
            return vv.detect(f)

        detection = paired_times(lambda f: detect(reference,f), lambda f: detect(v,f), frames, args.repeat)
        info = {"masks":masks, "detect":detection}
        if name.startswith("opencl:"):
            info["device"] = v._cl_runtime["name"]
            info["hsv_colors_verified"] = (1 << 24) if args.verify_hsv else 0
        results[name] = info
        print(f"{name}: masks {masks['before']['median_ms']:.3f} → {masks['after']['median_ms']:.3f}ms"
              f" ({masks['speedup']:.2f}x)，detect {detection['before']['median_ms']:.3f} →"
              f" {detection['after']['median_ms']:.3f}ms", flush=True)
    fallback_checks(cfg, frames[0])
    result = {"verified_frames":verified, "synthetic_cases":synthetic, "unavailable":unavailable,
              "reference_sha256":source_sha, "config":dataclasses.asdict(cfg),
              "frame_fingerprints":fingerprints, "backends":results}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding="utf-8")
    print(f"详细结果：{args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
