#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""识别热点基准测试：量化 detect() 各步骤耗时，并对比 CPU / OpenCL(UMat) / SIMD 写法。

用途：每次想"加速识别"之前先跑这个，用数据选路径；改完再跑一遍确认收益，
并核对"结果是否完全一致"（加速不能改变识别结果）。

用法：
    python tools\\bench_vision.py                 # 默认取几张真实帧
    python tools\\bench_vision.py --frames A.png B.png
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

import cv2
import numpy as np

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402

DEFAULT_FRAMES = [
    PROJECT_DIR / "debug" / "dryrun" / "dry_000.png",
    PROJECT_DIR / "debug" / "dead_20261001_030020_before.png",
    PROJECT_DIR / "debug" / "miss" / "miss_20261001_034003_before.png",
]


def bench(label: str, fn, n: int = 30) -> float:
    fn()                                   # 预热
    t0 = time.perf_counter()
    for _ in range(n):
        fn()
    ms = (time.perf_counter() - t0) / n * 1000
    print(f"  {label:<34} {ms:7.2f} ms")
    return ms


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="识别热点基准")
    ap.add_argument("--frames", nargs="*", default=[str(p) for p in DEFAULT_FRAMES])
    ap.add_argument("--repeat", type=int, default=30)
    args = ap.parse_args()

    cfg = J.Config.load(PROJECT_DIR / "config.json")
    frames = [cv2.imread(str(p)) for p in args.frames]
    frames = [f for f in frames if f is not None]
    if not frames:
        print("没有可用帧", file=sys.stderr)
        return 2
    frame = frames[0]
    print(f"帧: {frame.shape[1]}x{frame.shape[0]}，样本 {len(frames)} 张，重复 {args.repeat} 次")
    print(f"OpenCV {cv2.__version__} | 线程 {cv2.getNumThreads()} | OpenCL "
          f"{cv2.ocl.haveOpenCL()}/{cv2.ocl.useOpenCL()}")

    vision = J.Vision(cfg)
    vision.detect(frame)
    pm, bm = vision.masks(frame)
    piece, target, _pm, _bm = vision.detect(frame)

    print("\n[整链路]")
    bench("detect()", lambda: vision.detect(frame), args.repeat)
    bench("masks()", lambda: vision.masks(frame), args.repeat)
    bench("find_piece()", lambda: vision.find_piece(frame, pm), args.repeat)
    bench("find_target()", lambda: vision.find_target(frame, bm, piece, pm), args.repeat)
    bench("read_score()", lambda: J.read_score(frame, cfg), args.repeat)
    bench("_small_gray()", lambda: J._small_gray(frame, cfg), args.repeat)

    print("\n[OpenCL / UMat 对比]")
    hsv_u8 = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
    u_frame = cv2.UMat(frame)
    bench("cvtColor CPU", lambda: cv2.cvtColor(frame, cv2.COLOR_BGR2HSV), args.repeat)
    bench("cvtColor UMat", lambda: cv2.cvtColor(u_frame, cv2.COLOR_BGR2HSV).get(), args.repeat)
    mask_u8 = (hsv_u8[:, :, 2] > 128).astype(np.uint8) * 255
    u_mask = cv2.UMat(mask_u8)
    kern = np.ones((5, 5), np.uint8)
    bench("morphologyEx CPU",
          lambda: cv2.morphologyEx(mask_u8, cv2.MORPH_CLOSE, kern, iterations=2), args.repeat)
    bench("morphologyEx UMat",
          lambda: cv2.morphologyEx(u_mask, cv2.MORPH_CLOSE, kern, iterations=2).get(),
          args.repeat)
    bench("connectedComponentsWithStats CPU",
          lambda: cv2.connectedComponentsWithStats(mask_u8, 8), args.repeat)
    tpl = cv2.imread(str(PROJECT_DIR / "templates" / "piece.png"))
    if tpl is not None:
        u_tpl = cv2.UMat(tpl)
        bench("matchTemplate CPU",
              lambda: cv2.matchTemplate(frame, tpl, cv2.TM_CCOEFF_NORMED), args.repeat)
        bench("matchTemplate UMat",
              lambda: cv2.matchTemplate(u_frame, u_tpl, cv2.TM_CCOEFF_NORMED).get(),
              args.repeat)

    print("\n[numpy 掩码写法对比]")
    bg = J.estimate_background_rows(frame)
    val = hsv_u8[:, :, 2].astype(np.int16)
    bgv = bg[:, 2].astype(np.int16).reshape(-1, 1)

    def old_darker():
        return val < (bgv - 30)

    def new_darker():
        thr = np.clip(bg[:, 2].astype(np.int16) - 30, 0, 255).astype(np.uint8).reshape(-1, 1)
        thr_full = np.ascontiguousarray(np.broadcast_to(thr, frame.shape[:2]))
        return cv2.compare(hsv_u8[:, :, 2], thr_full, cv2.CMP_LT) > 0

    same = int((old_darker() ^ new_darker()).sum())
    print(f"  darker 两种写法差异像素 = {same}（必须为 0）")
    bench("darker (int16 numpy)", old_darker, args.repeat)
    bench("darker (uint8 cv2.compare)", new_darker, args.repeat)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
