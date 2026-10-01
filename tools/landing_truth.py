#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""用"游戏自带的中心白点"当尺子，量出**真实落点误差**（而不是噪声 ±28px 的估计值）。

原理：`debug/miss/` 下每对 before/after 帧里，after 帧能看到棋子落在方块上，
而唱片机转盘、便利店屋顶、快递箱等方块中心画着一个亮白小点——那是游戏的中心判定点。
于是：
    真实误差 = 棋子锚点相对白点的偏移，在跳跃轴向上的投影
对照 CSV 里的估计误差，就能知道：
  1) 估计值到底偏多少、偏哪边；
  2) 未命中样本的真实误差符号（过冲还是欠冲）——这是调"按压距离"唯一可靠的依据。

用法：
    python tools\\landing_truth.py                    # 跑 debug/miss 下全部配对
    python tools\\landing_truth.py --list             # 逐帧明细
"""
from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402

MISS_DIR = PROJECT_DIR / "debug" / "miss"


# 白点检测直接用 jumpbot 里的实现（主循环同样用它），避免"工具与主程序两套实现漂移"
find_dot_near = J.find_dot_near


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="用中心白点量真实落点误差")
    ap.add_argument("--dir", default=str(MISS_DIR))
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--tol", type=float, default=12.0, help="中心命中容差 px")
    ap.add_argument("--list", action="store_true", help="打印逐帧明细")
    args = ap.parse_args()

    cfg = J.Config.load(Path(args.config))
    pairs = []
    for before in sorted(Path(args.dir).glob("*_before.png")):
        after = before.with_name(before.name.replace("_before.png", "_after.png"))
        if after.exists():
            pairs.append((before, after))
    print(f"配对 {len(pairs)} 组")
    rows = []
    for before, after in pairs:
        f1 = cv2.imread(str(before)); f2 = cv2.imread(str(after))
        if f1 is None or f2 is None:
            continue
        vision = J.Vision(cfg)
        piece1, target1, _pm1, _bm1 = vision.detect(f1)
        piece2, _t2, _pm2, bm2 = vision.detect(f2)
        if piece1 is None or target1 is None or piece2 is None or bm2 is None:
            continue
        axis, _along, _cross = J.axis_components(target1.x - piece1.x, target1.y - piece1.y,
                                                 cfg.axis_slope)
        detail = J.measure_landing_error(f2, bm2, piece2, cfg, axis,
                                         expected_width=target1.top_width)
        if detail is None:
            continue
        est_err, _width, _center_x = detail
        ux, uy = axis
        cx_est = piece2.x - est_err * ux
        cy_est = piece2.y + cfg.piece_anchor_dy_ratio * f2.shape[0] + 18
        dot = find_dot_near(f2, cx_est, cy_est, 70.0, cfg)
        if dot is None:
            rows.append((before.name, est_err, None, None))
            continue
        dx, dy, _area = dot
        py = piece2.y + cfg.piece_anchor_dy_ratio * f2.shape[0]
        true_err = (piece2.x - dx) * ux + (py - dy) * uy
        rows.append((before.name, est_err, true_err, math.hypot(dx - cx_est, dy - cy_est)))

    with_dot = [r for r in rows if r[2] is not None]
    print(f"有估计误差的帧 {len(rows)}；其中能找到中心白点 {len(with_dot)}")
    if not with_dot:
        return 0
    print()
    print(f"{'帧':<34}{'估计误差':>9}{'真实误差':>9}{'白点离估计':>11}")
    for name, est, true, dist in with_dot:
        print(f"{name[:34]:<34}{est:>9.1f}{true:>9.1f}{dist:>11.1f}")
    import statistics
    est_vals = [r[1] for r in with_dot]
    true_vals = [r[2] for r in with_dot]
    print()
    print(f"估计误差: 均值 {sum(est_vals)/len(est_vals):+.2f}  中位 {statistics.median(est_vals):+.2f}")
    print(f"真实误差: 均值 {sum(true_vals)/len(true_vals):+.2f}  中位 {statistics.median(true_vals):+.2f}"
          f"  |err|中位 {statistics.median([abs(v) for v in true_vals]):.1f}")
    print(f"真实 |误差| > 容差 {args.tol:.0f}px 的比例: "
          f"{sum(1 for v in true_vals if abs(v) > args.tol)}/{len(true_vals)} = "
          f"{100*sum(1 for v in true_vals if abs(v) > args.tol)/len(true_vals):.0f}%（这些帧都是真实未命中）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
