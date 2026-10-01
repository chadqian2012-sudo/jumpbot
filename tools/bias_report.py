#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""偏置扫描结果分析：按 bias_pct 分组统计真实命中率，找中心判定区间的中心。

背景：像素级的落点误差测量已全部被证伪（掩码法噪声 ±28px 且符号会反；"落地帧中心白点"
约一半与游戏判分矛盾）。所以"当前瞄点/按压有没有系统偏置"只能用**分数**这个权威二值信号
测——主动给沿轴距离加受控偏置，看命中率曲线在哪里达峰。峰值不在 0 就说明存在可修的系统偏置。

用法：
    python tools\\bias_report.py logs\\run_xxx.csv [logs\\run_yyy.csv ...]
    python tools\\bias_report.py --glob            # 自动取所有含 bias_pct 的 run_*.csv
"""
from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _scorelog as SL  # noqa: E402

PROJECT_DIR = Path(__file__).resolve().parent.parent


def wilson(hits: int, n: int, z: float = 1.96) -> tuple:
    if n == 0:
        return (0.0, 0.0)
    p = hits / n
    d = 1 + z * z / n
    center = (p + z * z / (2 * n)) / d
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, center - half), min(1.0, center + half))


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="偏置扫描结果分析")
    ap.add_argument("files", nargs="*", help="CSV 路径")
    ap.add_argument("--glob", action="store_true", help="自动扫描 logs/ 下所有含 bias_pct 的 CSV")
    ap.add_argument("--by-distance", action="store_true", help="再按距离分段拆开看")
    args = ap.parse_args()

    paths = [Path(x) for x in args.files]
    if args.glob or not paths:
        for p in sorted((PROJECT_DIR / "logs").glob("run_*.csv")):
            try:
                head = p.open(encoding="utf-8-sig").readline()
            except OSError:
                continue
            if "bias_pct" in head:
                paths.append(p)
    paths = [p for p in paths if p.exists()]
    if not paths:
        print("没有带 bias_pct 列的日志；先用 --bias-sweep 跑一轮", file=sys.stderr)
        return 2

    rows = list(SL.iter_rows(paths))
    groups: dict = {}
    for row, label, _s in SL.iter_labels(rows):
        if label is None or label == -1:
            continue
        try:
            b = float(row.get("bias_pct") or 0.0)
        except ValueError:
            continue
        groups.setdefault(b, []).append((row, label))

    print("参与统计的文件： " + ", ".join(p.name for p in paths))
    print(f"\n{'偏置%':>7} {'命中':>5} {'普通':>5} {'样本':>5} {'命中率':>8}   95%CI")
    best = None
    for b in sorted(groups):
        v = groups[b]
        hits = sum(1 for _r, l in v if l == 1)
        miss = sum(1 for _r, l in v if l == 0)
        n = hits + miss
        if n == 0:
            continue
        lo, hi = wilson(hits, n)
        rate = hits / n
        flag = ""
        if best is None or rate > best[1]:
            best = (b, rate, n)
            flag = "  <-- 当前最佳"
        print(f"{b:+7.1f} {hits:5d} {miss:5d} {n:5d} {100 * rate:7.2f}%   "
              f"{100 * lo:5.2f}~{100 * hi:5.2f}{flag}")

    if best:
        b0 = groups.get(0.0)
        print(f"\n峰值档位：{best[0]:+.1f}%（命中率 {100 * best[1]:.2f}%，n={best[2]}）")
        if b0:
            h = sum(1 for _r, l in b0 if l == 1)
            m = sum(1 for _r, l in b0 if l == 0)
            if h + m:
                base = h / (h + m)
                print(f"零偏置档：{100 * base:.2f}%（n={h + m}）  "
                      f"→ 峰值相对零偏置 {100 * (best[1] - base):+.2f}pp")
                if abs(best[0]) < 0.01:
                    print("结论：峰值就在零偏置 —— 当前不存在可修的系统偏置，"
                          "精度只能靠降方差（换更好的瞄点/更稳的按压），不要再加常数补偿。")
                else:
                    print(f"结论：峰值偏离零偏置 {best[0]:+.1f}% —— "
                          f"存在可修的系统偏置，值得做 A/B 验证后再改默认。")

    if args.by_distance:
        print("\n分距离（只列样本足够的组合）")
        bands = [(0, 120, "<120"), (120, 180, "120-180"), (180, 240, "180-240"),
                 (240, 300, "240-300"), (300, 360, "300-360"), (360, 9999, "360+")]
        print(f"{'距离':>9} " + " ".join(f"{b:+7.1f}" for b in sorted(groups)))
        for lo, hi, nm in bands:
            cells = []
            for b in sorted(groups):
                v = [(r, l) for r, l in groups[b]
                     if lo <= SL.as_float(r.get("dist_px"), 0) < hi]
                hits = sum(1 for _r, l in v if l == 1)
                miss = sum(1 for _r, l in v if l == 0)
                n = hits + miss
                cells.append(f"{100 * hits / n:6.1f}" if n >= 10 else "     -")
            print(f"{nm:>9} " + " ".join(f"{c:>7}" for c in cells))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
