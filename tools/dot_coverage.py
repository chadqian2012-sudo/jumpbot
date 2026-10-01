#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""统计"中心标记（白色小圆点）"能覆盖多少目标，尤其是低命中率的"粘连块"。

背景：2026-10-01 第 4 轮统计显示 target.top_width/bbox_width < 0.45 的"粘连块"
真实命中率只有 91.9%（其它类 96.8~99.5%）。这些方块里有一部分是**抬高的台面**
（唱片机转盘、便利店屋顶、快递箱），游戏会在中心画一个亮白小点——只要检测到它，
瞄点就从"顶面估计"换成真中心，不需要靠"按比例缩短按压"这种盲补。

本工具把某一轮 CSV 里 未命中/命中 的样本帧找出来，用**放宽的门限**跑中心标记检测，
回答：如果我把门限放宽到 X，能覆盖多少比例的粘连块，且瞄点会移动多少像素。

用法：
    python tools\\dot_coverage.py logs\\run_xxx.csv --class low_ratio
    python tools\\dot_coverage.py logs\\run_xxx.csv --class all --limit 60
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import cv2  # noqa: E402

from _scorelog import TAG_RE, iter_labels, parse_csv  # noqa: E402

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402

MISS_DIR = PROJECT_DIR / "debug" / "miss"


def find_frame(index: int, timestamp: str, kind: str = "before") -> Path | None:
    """按 CSV 的时间戳（HH:MM:SS）匹配 debug/miss 里的现场帧（允许 ±2 秒）。"""
    if not timestamp or ":" not in timestamp:
        return None
    hh, mm, ss = (int(v) for v in timestamp.split(":"))
    total = hh * 3600 + mm * 60 + ss
    for path in MISS_DIR.glob(f"miss_*_{kind}.png"):
        stamp = path.name.split("_")[2]          # miss_20261001_034003_before.png
        if len(stamp) != 6 or not stamp.isdigit():
            continue
        file_total = int(stamp[0:2]) * 3600 + int(stamp[2:4]) * 60 + int(stamp[4:6])
        if abs(file_total - total) <= 2:
            return path
    return None


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="中心标记覆盖率统计")
    ap.add_argument("csv", help="run_*.csv 路径")
    ap.add_argument("--class", dest="cls",
                    choices=["low_ratio", "normal", "miss", "all"], default="low_ratio")
    ap.add_argument("--gate", type=float, default=0.45, help="放宽后的顶面/宽度门限")
    ap.add_argument("--min-width", type=int, default=120, help="放宽后的最小连通域宽度")
    ap.add_argument("--max-offset", type=float, default=60.0, help="中心标记离估计中心的最大距离")
    ap.add_argument("--limit", type=int, default=0, help="最多处理多少帧（0=全部）")
    args = ap.parse_args()

    cfg = J.Config.load(PROJECT_DIR / "config.json")
    cfg.center_dot_max_offset_px = args.max_offset
    cfg.center_dot_ratio_gate = args.gate
    cfg.center_dot_min_block_width = args.min_width

    rows = []
    # 用 _scorelog.parse_csv（with-open + csv 模块），不要自己 open + __import__("csv")：
    # 后者句柄不关，而且 note 里的逗号会被切错。
    for row, label, _streak in iter_labels(parse_csv(Path(args.csv))):
        match = TAG_RE.search(row.get("note") or "")
        if not match or label is None:
            continue
        _x, _y, width, height, top_width = (int(v) for v in match.groups())
        ratio = top_width / max(width, 1)
        rows.append((row, label, width, height, top_width, ratio))

    picked = []
    for row, label, width, height, top_width, ratio in rows:
        if args.cls == "low_ratio" and ratio >= args.gate:
            continue
        if args.cls == "normal" and ratio < args.gate:
            continue
        if args.cls == "miss" and label != 0:
            continue
        picked.append((row, label, width, height, top_width, ratio))
    if args.limit:
        picked = picked[: args.limit]
    print(f"选中样本 {len(picked)} 个（class={args.cls}，gate={args.gate}，"
          f"min_width={args.min_width}，max_offset={args.max_offset}）")

    found = 0
    shifts = []
    missing_frames = 0
    for row, label, width, height, top_width, ratio in picked:
        frame_path = find_frame(int(row["index"]), row.get("timestamp", ""), "before")
        if frame_path is None:
            missing_frames += 1
            continue
        frame = cv2.imread(str(frame_path))
        if frame is None:
            missing_frames += 1
            continue
        vision = J.Vision(cfg)
        piece, target, _pm, _bm = vision.detect(frame)
        if piece is None or target is None:
            continue
        dot = J.find_center_dot(frame, target, cfg)
        if dot is None:
            continue
        import math
        shift = math.hypot(dot[0] - target.x, dot[1] - target.y)
        found += 1
        shifts.append((row["index"], label, width, height, top_width, ratio, shift))

    n = len(picked) - missing_frames
    print(f"有现场帧的样本 {n} 个（缺帧 {missing_frames}）")
    if n:
        print(f"检测到中心标记：{found}/{n} = {found / n * 100:.0f}%")
    if shifts:
        shifts.sort(key=lambda s: -s[6])
        print(f"{'序号':>6}{'真值':>6}{'bbox':>12}{'比值':>7}{'瞄点位移':>10}")
        for idx, label, w, h, tw, ratio, shift in shifts[:20]:
            print(f"{idx:>6}{'命中' if label else '未中':>6}{f'{w}x{h}':>12}"
                  f"{ratio:>7.2f}{shift:>10.1f}")
        avg = sum(s[6] for s in shifts) / len(shifts)
        print(f"平均瞄点位移 {avg:.1f}px；位移>20px 的有 "
              f"{sum(1 for s in shifts if s[6] > 20)} 个")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
