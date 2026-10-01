#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""节奏复盘：量化"每跳总间隔 / 按压时长 / 非按压开销"，用于验证轮询节奏优化的效果。

用法：
    python tools\\cadence_report.py                       # 最新一轮
    python tools\\cadence_report.py A.csv B.csv           # 多轮对比（左=旧，右=新）
"""
from __future__ import annotations

import argparse
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _scorelog import parse_csv  # noqa: E402

PROJECT_DIR = Path(__file__).resolve().parent.parent


def stats(path: Path) -> dict | None:
    rows = []
    phases = {"t_acquire_ms": [], "t_landing_ms": [], "t_score_ms": [], "t_loop_ms": []}
    for r in parse_csv(path):
        if (r.get("result") or "") != "landed":
            continue
        try:
            hh, mm, ss = (int(v) for v in (r.get("timestamp") or "").split(":"))
            rows.append((hh * 3600 + mm * 60 + ss, float(r.get("hold_ms") or 0)))
        except Exception:
            continue
        for key in phases:
            val = r.get(key)
            if val not in (None, ""):
                try:
                    phases[key].append(float(val))
                except ValueError:
                    pass
    if len(rows) < 20:
        return None
    gaps, holds = [], []
    for (t0, h0), (t1, _h1) in zip(rows, rows[1:]):
        dt = t1 - t0
        if 0 < dt < 20:
            gaps.append(dt * 1000.0)
            holds.append(h0)
    if len(gaps) < 20:
        return None
    over = [g - h for g, h in zip(gaps, holds)]
    return {
        "file": path.name, "n": len(gaps),
        "gap_med": statistics.median(gaps), "gap_mean": sum(gaps) / len(gaps),
        "hold_med": statistics.median(holds),
        "over_med": statistics.median(over), "over_mean": sum(over) / len(over),
        "over_p90": sorted(over)[int(len(over) * 0.9)],
        "over_tail": 100.0 * sum(1 for o in over if o > 1200) / len(over),
        "phases": {k: (statistics.median(v) if len(v) >= 20 else None) for k, v in phases.items()},
    }


def show(s: dict) -> None:
    print(f"{s['file']}（{s['n']} 跳）")
    print(f"  每跳总间隔  中位 {s['gap_med']:6.0f} ms   均值 {s['gap_mean']:6.0f} ms")
    print(f"  按压时长    中位 {s['hold_med']:6.0f} ms")
    print(f"  非按压开销  中位 {s['over_med']:6.0f} ms   均值 {s['over_mean']:6.0f} ms"
          f"   P90 {s['over_p90']:6.0f} ms   >1.2s 占比 {s['over_tail']:.1f}%")
    ph = s.get("phases") or {}
    if ph.get("t_loop_ms") is not None:
        print(f"  分阶段（中位）：抓帧识别(acquire) {ph['t_acquire_ms']:.0f} ms | "
              f"落地等待(landing) {ph['t_landing_ms']:.0f} ms | "
              f"读分(score) {ph['t_score_ms']:.0f} ms | 整轮循环 {ph['t_loop_ms']:.0f} ms")


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="每跳节奏复盘")
    ap.add_argument("csv", nargs="*", help="一轮或多轮 CSV；默认取最新")
    args = ap.parse_args()
    paths = [Path(p) for p in args.csv]
    if not paths:
        cands = sorted((PROJECT_DIR / "logs").glob("run_*.csv"), key=lambda p: p.stat().st_mtime)
        if not cands:
            print("logs/ 下没有 run_*.csv", file=sys.stderr)
            return 2
        paths = [cands[-1]]
    results = [s for s in (stats(p) for p in paths) if s]
    if not results:
        print("可用样本不足（每轮至少 20 跳）")
        return 1
    for s in results:
        show(s)
    if len(results) >= 2:
        a, b = results[0], results[-1]
        print()
        print(f"对比 {a['file']} → {b['file']}：")
        print(f"  每跳总间隔  {a['gap_med']:.0f} → {b['gap_med']:.0f} ms"
              f"（{b['gap_med'] - a['gap_med']:+.0f} ms）")
        print(f"  非按压开销  {a['over_med']:.0f} → {b['over_med']:.0f} ms"
              f"（{b['over_med'] - a['over_med']:+.0f} ms）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
