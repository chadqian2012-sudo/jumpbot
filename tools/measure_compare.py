#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""对比两种落点误差测量的可靠性：掩码法（当前 CSV 用的）vs 相位相关法。

真值只有"命中/未命中"（来自 HUD 分数），而 debug/miss/ 下保存的都是**真实未命中**的
起跳帧+落地帧，所以：哪种测量在这些帧上更常给出 |err|>12px，哪种就更可信。
（掩码法已知噪声 ±28px，会让约 20% 的真实未命中"看起来像命中"。）

用法：
    python tools\\measure_compare.py            # 跑 debug/miss/ 下全部配对
    python tools\\measure_compare.py --tol 12
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="掩码法 vs 相位相关法 落点误差对比")
    ap.add_argument("--dir", default=str(PROJECT_DIR / "debug" / "miss"))
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--tol", type=float, default=12.0, help="中心命中容差（px）")
    args = ap.parse_args()

    cfg = J.Config.load(Path(args.config))
    pairs = []
    for before in sorted(Path(args.dir).glob("*_before.png")):
        after = before.with_name(before.name.replace("_before.png", "_after.png"))
        if after.exists():
            pairs.append((before, after))
    if not pairs:
        print(f"{args.dir} 下没有 *_before/_after 配对", file=sys.stderr)
        return 2

    print(f"配对 {len(pairs)} 组（全部为真实未命中）")
    print(f"{'帧':<34}{'掩码法':>10}{'相位法':>10}")
    mask_hits = flow_hits = 0
    mask_vals, flow_vals = [], []
    for before, after in pairs:
        f1 = cv2.imread(str(before))
        f2 = cv2.imread(str(after))
        if f1 is None or f2 is None:
            continue
        vision = J.Vision(cfg)
        piece, target, _pm, _bm = vision.detect(f1)
        if piece is None or target is None:
            print(f"{before.name[:34]:<34}{'起跳帧识别失败':>20}")
            continue
        piece2, _t2, _pm2, bm2 = vision.detect(f2)
        if piece2 is None:
            print(f"{before.name[:34]:<34}{'落地帧无棋子':>20}")
            continue
        axis, _along, _cross = J.axis_components(target.x - piece.x, target.y - piece.y,
                                                 cfg.axis_slope)
        detail = J.measure_landing_error(f2, bm2, piece2, cfg, axis,
                                         expected_width=target.top_width)
        mask_err = detail[0] if detail else float("nan")
        flow = J.measure_landing_error_flow(f1, f2, target, piece2, cfg, axis)
        flow_err = flow[0] if flow else float("nan")
        mask_vals.append(mask_err)
        flow_vals.append(flow_err)
        if abs(mask_err) < args.tol:
            mask_hits += 1
        if abs(flow_err) < args.tol:
            flow_hits += 1
        print(f"{before.name[:34]:<34}{mask_err:>10.1f}{flow_err:>10.1f}")
    n = len(mask_vals)
    if n:
        print()
        print(f"真实未命中 {n} 组中，被判成「命中」( |err| < {args.tol:.0f}px ) 的比例：")
        print(f"  掩码法（当前 CSV 用）：{mask_hits}/{n} = {mask_hits / n * 100:.0f}%"
              f"   ← 越低越可信")
        print(f"  相位相关法：          {flow_hits}/{n} = {flow_hits / n * 100:.0f}%")
        good = sum(1 for v in flow_vals if v == v)
        print(f"  相位法可用帧 {good}/{n}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
