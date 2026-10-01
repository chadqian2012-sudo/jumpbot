#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""识别 A/B 对比：同一批帧在“当前 config.json”与“叠加覆盖项”两种设置下的目标选择差异。

改阈值/改算法前后来一遍，能立刻看出这次改动影响了哪些帧、影响方向是否合理，
避免“改好了这一帧、悄悄弄坏了另外十帧”。

用法：
    python tools\\ab_vision.py --set block_pale_delta=15            # 只比一项
    python tools\\ab_vision.py --set block_pale_delta=15 --set block_degenerate_top_ratio=0
    python tools\\ab_vision.py --limit 200 --show 30
"""
from __future__ import annotations

import argparse
import importlib.util
import math
import sys
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402


def load_regress_module():
    spec = importlib.util.spec_from_file_location("rgr", PROJECT_DIR / "tools" / "regress.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def detect_target(cfg, frame):
    try:
        piece, target, _pm, _bm = J.Vision(cfg).detect(frame)
    except Exception as exc:  # 单帧异常不该打断对比
        return None, None, f"异常 {exc}"
    return piece, target, None


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="识别 A/B 对比")
    ap.add_argument("--set", action="append", default=[], metavar="KEY=VALUE",
                    help="B 组相对 config.json 的覆盖项，可重复")
    ap.add_argument("--dir", default=str(PROJECT_DIR / "debug"))
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--limit", type=int, default=0, help="只跑前 N 帧（0=全部）")
    ap.add_argument("--show", type=int, default=20, help="最多列出多少条差异")
    ap.add_argument("--tol", type=float, default=15.0, help="目标中心差异超过该值算“变了”")
    args = ap.parse_args()

    rgr = load_regress_module()
    cfg_a = J.Config.load(Path(args.config))
    cfg_b = J.Config.load(Path(args.config))
    for item in args.set:
        key, _, raw = item.partition("=")
        if not hasattr(cfg_b, key):
            print(f"配置里没有 {key}", file=sys.stderr)
            return 2
        cur = getattr(cfg_b, key)
        try:
            value = int(raw) if isinstance(cur, int) else float(raw)
        except ValueError:
            value = raw
        setattr(cfg_b, key, value)
        print(f"[B 组] {key} = {value}（当前值 {getattr(cfg_a, key)}）")

    frames = sorted(p for p in Path(args.dir).rglob("*.png") if rgr.is_game_frame(p))
    if args.limit:
        frames = frames[: args.limit]
    diff_missing_a = []     # A 无目标、B 有
    diff_missing_b = []     # A 有目标、B 无
    diff_moved = []         # 两边都有但中心差异过大
    same = errors = 0
    for path in frames:
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        _pa, ta, ea = detect_target(cfg_a, frame)
        _pb, tb, eb = detect_target(cfg_b, frame)
        if ea or eb:
            errors += 1
            continue
        if ta is None and tb is None:
            same += 1
            continue
        if ta is None:
            diff_missing_a.append(path.name)
            continue
        if tb is None:
            diff_missing_b.append(path.name)
            continue
        delta = math.hypot(ta.x - tb.x, ta.y - tb.y)
        if delta > args.tol:
            diff_moved.append((path.name, ta, tb, delta))
        else:
            same += 1

    print(f"\n对比帧数 {len(frames)}：一致 {same}，A 无/B 有 {len(diff_missing_a)}，"
          f"A 有/B 无 {len(diff_missing_b)}，中心位移 >{args.tol:.0f}px {len(diff_moved)}，"
          f"异常 {errors}")
    if diff_missing_a:
        print(f"\nA 无目标 → B 有目标（{len(diff_missing_a)}）：{diff_missing_a[:args.show]}")
    if diff_missing_b:
        print(f"\nA 有目标 → B 无目标（{len(diff_missing_b)}）：{diff_missing_b[:args.show]}")
    if diff_moved:
        diff_moved.sort(key=lambda item: -item[3])
        print(f"\n目标中心位移最大 {min(len(diff_moved), args.show)} 条：")
        for name, ta, tb, delta in diff_moved[: args.show]:
            print(f"  {name}: A=({ta.x:.0f},{ta.y:.0f}) → B=({tb.x:.0f},{tb.y:.0f}) "
                  f"位移 {delta:.0f}px")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
