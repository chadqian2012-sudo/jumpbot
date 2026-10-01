#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""两组实机日志的命中率 A/B 对比（权威口径 = 读分判连击，见 tools/_scorelog.py）。

为什么需要它：单轮 500 跳的命中率标准误约 0.76%，肉眼看"97.0 vs 96.8"分不出真假。
这个工具给出 Wilson 置信区间与两比例 z 检验，并把差异按距离分段拆开——
很多改动是"某一距离段变好/变差"，聚合数字会把它平均掉。

用法：
    python tools\\ab_hitrate.py --a logs\\run_A1.csv,logs\\run_A2.csv --b logs\\run_B1.csv,logs\\run_B2.csv
    python tools\\ab_hitrate.py --a ... --b ... --label-a geometric --label-b estimate
"""
from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _scorelog as SL  # noqa: E402


def wilson(hits: int, n: int, z: float = 1.96) -> tuple:
    """Wilson 区间（小样本/接近 100% 时比正态近似稳）。"""
    if n == 0:
        return (0.0, 0.0)
    p = hits / n
    d = 1 + z * z / n
    center = (p + z * z / (2 * n)) / d
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, center - half), min(1.0, center + half))


def z_test(h1: int, n1: int, h2: int, n2: int) -> tuple:
    """两比例 z 检验（合并比例），返回 (z, 双尾 p)。"""
    if n1 == 0 or n2 == 0:
        return (0.0, 1.0)
    p1, p2 = h1 / n1, h2 / n2
    p = (h1 + h2) / (n1 + n2)
    se = math.sqrt(p * (1 - p) * (1 / n1 + 1 / n2))
    if se <= 0:
        return (0.0, 1.0)
    z = (p1 - p2) / se
    pv = math.erfc(abs(z) / math.sqrt(2))
    return (z, pv)


def band_of(dist: float, edges, names) -> str:
    for lo, hi, nm in zip(edges[:-1], edges[1:], names):
        if lo <= dist < hi:
            return nm
    return names[-1]


def collect(paths):
    rows = list(SL.iter_rows(paths))
    out = []
    for row, label, _streak in SL.iter_labels(rows):
        if label is None:
            continue
        out.append((row, label))
    return out


def agg(pairs):
    hits = sum(1 for _r, l in pairs if l == 1)
    miss = sum(1 for _r, l in pairs if l == 0)
    unk = sum(1 for _r, l in pairs if l == -1)
    n = hits + miss
    return hits, miss, unk, n


def show(name, pairs):
    h, m, u, n = agg(pairs)
    lo, hi = wilson(h, n)
    print("  %-12s 命中 %4d / 判定 %4d = %6.2f%%  (95%%CI %.2f~%.2f)  普通 %3d  无法判定 %d"
          % (name, h, n, 100 * h / max(1, n), 100 * lo, 100 * hi, m, u))
    return h, n


DIST_EDGES = [0, 120, 180, 240, 300, 360, 10000]
DIST_NAMES = ["<120", "120-180", "180-240", "240-300", "300-360", "360+"]


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="两组日志的命中率 A/B 对比")
    ap.add_argument("--a", required=True, help="A 组 CSV，逗号分隔")
    ap.add_argument("--b", required=True, help="B 组 CSV，逗号分隔")
    ap.add_argument("--label-a", default="A")
    ap.add_argument("--label-b", default="B")
    args = ap.parse_args()

    pa = [Path(x.strip()) for x in args.a.split(",") if x.strip()]
    pb = [Path(x.strip()) for x in args.b.split(",") if x.strip()]
    for p in pa + pb:
        if not p.exists():
            print(f"找不到 {p}", file=sys.stderr)
            return 2
    A, B = collect(pa), collect(pb)
    if not A or not B:
        print("样本为空", file=sys.stderr)
        return 2

    print("A 组文件：" + ", ".join(p.name for p in pa))
    print("B 组文件：" + ", ".join(p.name for p in pb))
    print("\n[总体]")
    h1, n1 = show(args.label_a, A)
    h2, n2 = show(args.label_b, B)
    z, pv = z_test(h1, n1, h2, n2)
    d = 100 * (h1 / max(1, n1) - h2 / max(1, n2))
    print("\n  差（A−B）= %+.2f 个百分点   z=%.2f   p=%.3f   %s"
          % (d, z, pv, "显著(p<0.05)" if pv < 0.05 else "不显著"))

    print("\n[按距离分段]")
    print("  %-10s %-22s %-22s %s" % ("距离", args.label_a, args.label_b, "差"))
    for lo, hi, nm in zip(DIST_EDGES[:-1], DIST_EDGES[1:], DIST_NAMES):
        sa = [(r, l) for r, l in A if lo <= SL.as_float(r.get("dist_px"), 0) < hi]
        sb = [(r, l) for r, l in B if lo <= SL.as_float(r.get("dist_px"), 0) < hi]
        ha, ma, _ua, na = agg(sa)
        hb, mb, _ub, nb = agg(sb)
        if na + nb == 0:
            continue
        ta = "%5d/%5d %6.2f%%" % (ha, na, 100 * ha / max(1, na))
        tb = "%5d/%5d %6.2f%%" % (hb, nb, 100 * hb / max(1, nb))
        dd = (100 * (ha / max(1, na) - hb / max(1, nb))) if na and nb else float("nan")
        zz, pp = z_test(ha, na, hb, nb)
        star = "*" if pp < 0.05 else " "
        print("  %-10s %-22s %-22s %+6.2f pp  p=%.3f%s" % (nm, ta, tb, dd, pp, star))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
