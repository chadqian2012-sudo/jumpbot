#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""瞄点精度数据集：把"顶面估计中心"和"游戏自带中心白点(真值)"成对导出。

为什么需要它：主循环的按压时长只取决于"棋子→目标"的沿轴距离，所以**瞄点误差会
直接变成落点误差**。但游戏只在部分方块上画中心白点，白点可用率实测仅 28.7%。
这个工具负责量化"没有白点时我们到底偏多少"，并给改进后的估计器提供验收集。

安全：纯离线，只读 PNG，不做任何窗口/输入操作（JUMPBOT_NO_INPUT=1）。

用法：
    python tools\\aim_error.py                        # 采集 + 汇总
    python tools\\aim_error.py --out debug/aim/ds.csv  # 另存数据集
    python tools\\aim_error.py --max 200               # 只取前 200 帧

注意：只喂**原始游戏帧**（debug/run_* / debug/miss / debug/samples）。
debug/annotated、debug/masks 里是被画过框/掩码的图，喂进去结果没有意义。

**踩过的坑**：`debug/run_*/jump_NNN_before.png` / `_after.png` 是 `annotate()` 画过
红/绿框和十字的**标注图**（实测含 3013 个纯绿、673 个纯红像素），只有
`jump_NNN_raw.png` / `jump_NNN_raw_after.png` 才是原始帧。本工具默认跳过标注图，
并按"纯绿/纯红像素数量"再兜底校验一次（`annotate()` 用的是 (0,255,0) 和 (0,0,255)）。
"""
from __future__ import annotations

import argparse
import csv
import dataclasses
import os
import statistics
import sys
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402

DEFAULT_ROOTS = ("run_*", "miss", "samples")


def looks_annotated(img) -> bool:
    """检测 annotate() 画的红/绿标记（纯色像素数量），用于剔除标注帧。"""
    import numpy as np
    b = img[:, :, 0].astype(np.int16)
    g = img[:, :, 1].astype(np.int16)
    r = img[:, :, 2].astype(np.int16)
    pure_green = int(((g > 200) & (r < 80) & (b < 80)).sum())
    pure_red = int(((r > 200) & (g < 80) & (b < 80)).sum())
    return pure_green > 200 or pure_red > 200


def iter_frames(debug_dir: Path, roots) -> list[Path]:
    out: list[Path] = []
    for pat in roots:
        for entry in sorted(debug_dir.glob(pat)):
            if entry.is_dir():
                for png in sorted(entry.glob("*.png")):
                    # run_* 目录里只有 *_raw*.png 是原始帧，*_before/_after 是标注图
                    if entry.name.startswith("run_") and png.name.endswith(
                            ("_before.png", "_after.png")):
                        continue
                    out.append(png)
            elif entry.suffix.lower() == ".png":
                out.append(entry)
    return out


def section(values, label):
    """打印一组误差的分位统计。"""
    if not values:
        print(f"  {label}: 无样本")
        return
    v = sorted(values)
    n = len(v)
    print("  %-22s n=%4d  均值 %+7.1f  中位 %+7.1f  |绝对值|中位 %6.1f  p90 %6.1f  最大 %6.1f"
          % (label, n, statistics.mean(v), statistics.median(v),
             statistics.median([abs(x) for x in v]),
             v[min(n - 1, int(0.9 * n))], v[-1]))


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    os.environ["JUMPBOT_NO_INPUT"] = "1"

    ap = argparse.ArgumentParser(description="瞄点精度数据集（顶面估计 vs 白点真值）")
    ap.add_argument("--debug", default=str(PROJECT_DIR / "debug"))
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--roots", default=",".join(DEFAULT_ROOTS),
                    help="逗号分隔的目录/通配符，相对 --debug")
    ap.add_argument("--out", default=str(PROJECT_DIR / "debug" / "aim" / "dataset.csv"))
    ap.add_argument("--max", type=int, default=0, help="最多处理多少帧（0=全部）")
    ap.add_argument("--mode", default="", choices=("", "estimate", "geometric"),
                    help="覆盖 config 的 aim_center_mode，用于 A/B")
    args = ap.parse_args()

    cfg = J.Config.load(Path(args.config))
    if args.mode:
        cfg = dataclasses.replace(cfg, aim_center_mode=args.mode)
    if cfg.center_dot_max_offset_px <= 0:
        print("config 的 center_dot_max_offset_px<=0，取不到真值，无法评估", file=sys.stderr)
        return 2

    debug_dir = Path(args.debug)
    frames = iter_frames(debug_dir, [x for x in args.roots.split(",") if x.strip()])
    if args.max > 0:
        frames = frames[: args.max]
    print(f"候选帧 {len(frames)}（来自 {args.roots}）")

    # 真值路径：按配置门限走 find_target，命中白点时 Block.dot 非空。
    # 估计路径：把 center_dot_max_offset_px 置 0，find_target 退化成纯顶面估计。
    v_dot = J.Vision(cfg)
    v_plain = J.Vision(dataclasses.replace(cfg, center_dot_max_offset_px=0.0))

    rows = []
    skipped_annotated = 0
    for i, path in enumerate(frames, 1):
        img = cv2.imread(str(path))
        if img is None:
            continue
        if looks_annotated(img):
            skipped_annotated += 1
            continue
        v_plain.reset_prior()
        piece, plain, _pm, _bm = v_plain.detect(img)
        if piece is None or plain is None:
            continue
        v_dot.reset_prior()
        _p2, dotted, _pm2, _bm2 = v_dot.detect(img)
        truth = dotted.dot if (dotted is not None and dotted.dot is not None) else None

        bbox_w = max(plain.bbox[2], 1)
        gate = (plain.top_width < cfg.center_dot_ratio_gate * bbox_w
                and plain.bbox[2] >= cfg.center_dot_min_block_width)
        # 放宽宽度门限后的白点搜索：用于回答"<120px 的方块上到底有没有白点"
        relaxed = J.find_dot_near(img, plain.x, plain.y,
                                  cfg.center_dot_max_offset_px, cfg)

        along = cross = err_along = err_cross = None
        if truth is not None:
            _u, along, cross = J.axis_components(truth[0] - piece.x, truth[1] - piece.y,
                                                 cfg.axis_slope)
            _u2, e_along, e_cross = J.axis_components(plain.x - truth[0], plain.y - truth[1],
                                                      cfg.axis_slope)
            err_along, err_cross = e_along, e_cross
        rows.append(dict(
            src=path.parent.name, file=path.name,
            bbox_x=plain.bbox[0], bbox_y=plain.bbox[1], bbox_w=plain.bbox[2],
            bbox_h=plain.bbox[3], top_y=round(plain.top_y, 2),
            top_width=round(plain.top_width, 2), ratio=round(plain.top_width / bbox_w, 4),
            area=plain.area, gate=int(gate),
            plain_x=round(plain.x, 2), plain_y=round(plain.y, 2),
            truth_x="" if truth is None else round(truth[0], 2),
            truth_y="" if truth is None else round(truth[1], 2),
            truth_area="" if truth is None else truth[2],
            dot_dist="" if truth is None else round(
                ((plain.x - truth[0]) ** 2 + (plain.y - truth[1]) ** 2) ** 0.5, 2),
            err_along="" if err_along is None else round(err_along, 2),
            err_cross="" if err_cross is None else round(err_cross, 2),
            piece_x=round(piece.x, 2), piece_y=round(piece.y, 2),
            along_dist="" if along is None else round(along, 2),
            cross_ratio="" if (along in (None, 0)) else round(abs(cross) / max(abs(along), 1.0), 4),
            relaxed_ok=int(relaxed is not None),
            relaxed_dx="" if relaxed is None else round(relaxed[0] - plain.x, 2),
            relaxed_dy="" if relaxed is None else round(relaxed[1] - plain.y, 2),
        ))
        if i % 200 == 0:
            print(f"  ...{i}/{len(frames)}")

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", newline="", encoding="utf-8") as fh:
        writer = csv.DictWriter(fh, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)
    print(f"数据集：{out_path}（{len(rows)} 行；另跳过标注图 {skipped_annotated} 张）")

    truth_rows = [r for r in rows if r["truth_x"] != ""]
    print(f"\n有目标帧 {len(rows)}，其中拿到白点真值 {len(truth_rows)} 帧"
          f"（{100 * len(truth_rows) / max(1, len(rows)):.1f}%）")

    print("\n[1] 顶面估计 vs 白点真值（有真值的帧）")
    section([r["err_along"] for r in truth_rows], "沿轴误差")
    section([r["err_cross"] for r in truth_rows], "横向误差")
    section([r["dot_dist"] for r in truth_rows], "欧氏距离")

    print("\n[2] 白点可用率：按方块宽度 / 是否过现有门限")
    for lo, hi in ((0, 80), (80, 120), (120, 200), (200, 4000)):
        band = [r for r in rows if lo <= r["bbox_w"] < hi]
        if not band:
            continue
        hit = sum(1 for r in band if r["truth_x"] != "")
        rel = sum(1 for r in band if r["relaxed_ok"])
        print("  宽度 %4d~%-4d n=%4d  过门限白点 %.1f%%   放宽门限后 %.1f%%"
              % (lo, hi, len(band), 100 * hit / len(band), 100 * rel / len(band)))

    print("\n[3] 放宽宽度门限后新多出来的白点（原先完全不搜的 <120px 方块）")
    extra = [r for r in rows if r["bbox_w"] < 120 and r["relaxed_ok"] and r["truth_x"] == ""]
    print(f"  新增候选 {len(extra)} 个")
    if extra:
        dx = [r["relaxed_dx"] for r in extra]
        dy = [r["relaxed_dy"] for r in extra]
        d = [(x * x + y * y) ** 0.5 for x, y in zip(dx, dy)]
        print("  相对顶面估计的偏移: 中位 %.1f  p90 %.1f  最大 %.1f px"
              % (statistics.median(d), sorted(d)[int(0.9 * len(d))], max(d)))
        far = sorted(zip(d, [r["file"] for r in extra]), reverse=True)[:5]
        print("  离估计最远的 5 例:", [(round(x), f) for x, f in far])

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
