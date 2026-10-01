#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""按目标方块类型统计真实命中率，并列出未命中样本（配合 debug/miss/ 现场帧）。

数据来源：run_*.csv 的 note 列（新版本会带 `tb=x,y,w,h tw=顶宽` 目标几何）。
真值判据与主程序一致（HUD 分数增量），与 tools/report.py 同一套逻辑。

用法：
    python tools\\miss_report.py                     # 最新一轮
    python tools\\miss_report.py logs\\run_xxx.csv    # 指定轮次
    python tools\\miss_report.py --all               # 汇总 logs/ 下所有轮次
"""
from __future__ import annotations

import argparse
import glob
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _scorelog import TAG_RE, as_float, iter_labels, iter_rows  # noqa: E402

PROJECT_DIR = Path(__file__).resolve().parent.parent
LOGS_DIR = PROJECT_DIR / "logs"


def classify(width: int, height: int, top_width: int) -> str:
    aspect = height / max(width, 1)
    if aspect > 1.8:
        return "细长高物件(旗杆等)"
    if aspect < 0.85:
        return "扁宽块(便利店/台阶)"
    if width < 90:
        return "小块(药瓶/圆台等)"
    if top_width / max(width, 1) < 0.45:
        return "粘连块(顶面占比低)"
    return "常规方块"


def analyze(rows) -> None:
    stats: dict = {}
    misses: list = []
    total = hits = 0
    for row, label, _streak in iter_labels(rows):
        if label not in (0, 1):
            continue
        dist = as_float(row.get("dist_px"))
        if dist is None:
            continue
        match = TAG_RE.search(row.get("note") or "")
        if not match:
            cls = "（旧日志无几何信息）"
            width = height = top_width = 0
        else:
            _x, _y, width, height, top_width = (int(v) for v in match.groups())
            cls = classify(width, height, top_width)
        bucket = stats.setdefault(cls, [0, 0])
        bucket[0] += 1
        bucket[1] += label
        total += 1
        hits += label
        if label == 0:
            misses.append((row["index"], dist, cls, width, height, top_width,
                           row.get("error_px"), row["_file"]))
    print(f"样本 {total}，真实命中 {hits} = {hits / max(total, 1) * 100:.2f}%")
    print()
    print(f"{'目标类型':<22}{'样本':>6}{'命中率':>9}")
    for cls, (n, h) in sorted(stats.items(), key=lambda kv: -kv[1][0]):
        print(f"{cls:<22}{n:>6}{h / n * 100:>8.1f}%")
    if misses:
        print()
        print(f"未命中样本（{len(misses)} 条，取前 40）：")
        print(f"{'#':>5}{'距离':>8}{'类型':<22}{'bbox宽高':>10}{'顶宽':>6}{'|测量误差|':>11}")
        for idx, dist, cls, w, h, tw, err, _f in misses[:40]:
            err_text = err if err else "n/a"
            size_text = f"{w}x{h}"
            print(f"{idx:>5}{dist:>8.1f}{cls:<22}{size_text:>10}{tw:>6}{err_text:>11}")
        print("\n现场帧在 debug/miss/（文件名带时间戳，与上面序号可用日志时间对照）")


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="按目标类型统计真实命中率")
    ap.add_argument("csv", nargs="?", help="CSV 路径，默认取最新一轮")
    ap.add_argument("--all", action="store_true", help="汇总 logs/ 下全部轮次")
    args = ap.parse_args()
    if args.all:
        paths = sorted(glob.glob(str(LOGS_DIR / "run_*.csv")))
    elif args.csv:
        paths = [args.csv]
    else:
        candidates = sorted(LOGS_DIR.glob("run_*.csv"), key=lambda p: p.stat().st_mtime)
        if not candidates:
            print("logs/ 下没有 run_*.csv", file=sys.stderr)
            return 2
        paths = [str(candidates[-1])]
    print(f"分析 {len(paths)} 个文件")
    analyze(iter_rows(paths))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
