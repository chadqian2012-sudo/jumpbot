#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""单帧诊断：把 Vision 找出的**全部**方块候选连同筛选条件一起打印出来。

用途：某次误检（例如把阴影当成目标方块、跳短摔死）时，用来定位这个候选
是从哪个连通域来的、面积/宽度/顶面中心各是多少、卡在哪一条筛选条件上。

用法：
    python tools\\inspect_frame.py debug\\dead_xxx_before.png
    python tools\\inspect_frame.py debug\\dead_xxx_before.png --mask debug\\mask.png
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

import cv2
import numpy as np

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="单帧方块候选诊断")
    ap.add_argument("image", help="帧图片路径")
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--mask", help="额外把方块掩码另存到该路径，便于看图")
    ap.add_argument("--top", type=int, default=25, help="最多打印多少个候选")
    ap.add_argument("--set", action="append", default=[], metavar="KEY=VALUE",
                    help="临时覆盖配置项，可重复，例如 --set block_bright_delta=6")
    args = ap.parse_args()

    frame = cv2.imread(args.image)
    if frame is None:
        print(f"读不到图片：{args.image}", file=sys.stderr)
        return 2
    cfg = J.Config.load(Path(args.config))
    for item in args.set:
        key, _, raw = item.partition("=")
        if not hasattr(cfg, key):
            print(f"配置里没有 {key}", file=sys.stderr)
            return 2
        cur = getattr(cfg, key)
        try:
            value = int(raw) if isinstance(cur, int) else float(raw)
        except ValueError:
            value = raw
        setattr(cfg, key, value)
        print(f"[覆盖] {key} = {value}")
    vis = J.Vision(cfg)
    piece_mask, block_mask = vis.masks(frame)
    piece = vis.find_piece(frame, piece_mask)
    if piece is None:
        print("未找到棋子")
        return 1
    h, w = frame.shape[:2]
    anchor_y = piece.y + cfg.piece_anchor_dy_ratio * h
    print(f"画面 {w}x{h}；棋子 bbox={piece.bbox} 中心=({piece.x:.1f},{piece.y:.1f}) "
          f"锚点_y={anchor_y:.1f} tpl={piece.tpl_score:.3f}")

    mask = block_mask.copy()
    grown = cv2.dilate(piece_mask, np.ones((9, 9), np.uint8), iterations=2)
    mask[grown > 0] = 0
    kernel = np.ones((5, 5), np.uint8)
    mask = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, kernel, iterations=2)
    mask = cv2.morphologyEx(mask, cv2.MORPH_OPEN, np.ones((3, 3), np.uint8), iterations=1)
    contours, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    filled = np.zeros_like(mask)
    cv2.drawContours(filled, contours, -1, 255, thickness=-1)
    if args.mask:
        cv2.imwrite(args.mask, filled)
    num, labels, stats, _cent = cv2.connectedComponentsWithStats(filled, 8)
    canvas_area = w * h
    min_dist = cfg.min_jump_dist_ratio * w
    max_dist = cfg.max_jump_dist_ratio * w

    rows = []
    for idx in range(1, num):
        bx, by, bw, bh, area = stats[idx]
        if area < cfg.block_min_area_ratio * 0.4 * canvas_area or \
                area > cfg.block_max_area_ratio * canvas_area:
            continue
        center = J.top_face_center(labels == idx, cfg.axis_slope)
        if center is None:
            continue
        cx, cy, top_width, top_y = center
        _axis, along, cross = J.axis_components(cx - piece.x, cy - piece.y, cfg.axis_slope)
        rows.append({
            "bbox": (int(bx), int(by), int(bw), int(bh)),
            "area": int(area),
            "center": (round(cx, 1), round(cy, 1)),
            "top_w": round(top_width, 1),
            "top_y": round(top_y, 1),
            "along": round(along, 1),
            "cross": round(cross, 1),
            "ratio": round(cross / max(abs(along), 1.0), 2),
            "dist": round(float(np.hypot(cx - piece.x, cy - anchor_y)), 1),
            "up": cy < piece.y - cfg.target_min_up_ratio * h,
            "in_range": min_dist <= along <= max_dist,
            "size_ok": (area >= cfg.block_min_area_ratio * canvas_area
                        and bw >= cfg.block_min_width_ratio * w
                        and bh >= cfg.block_min_height_ratio * h),
        })
    rows.sort(key=lambda r: r["dist"])
    print(f"共 {len(rows)} 个候选（按到棋子锚点距离排序）")
    for r in rows[: args.top]:
        flags = []
        flags.append("上方" if r["up"] else "—下方")
        flags.append("距离OK" if r["in_range"] else "距离✗")
        flags.append(f"横向比{r['ratio']:.2f}")
        flags.append("尺寸OK" if r["size_ok"] else "尺寸✗")
        print(f"  bbox={r['bbox']} area={r['area']:6d} 顶面中心={r['center']} "
              f"顶宽={r['top_w']:.0f} 沿轴={r['along']:7.1f} 横向={r['cross']:7.1f} "
              f"欧氏={r['dist']:6.1f} | " + " ".join(flags))
    target = vis.find_target(frame, block_mask, piece, piece_mask)
    if target is None:
        print("最终选择：无目标")
    else:
        _axis, along, cross = J.axis_components(
            target.x - piece.x, target.y - piece.y, cfg.axis_slope)
        print(f"最终选择：center=({target.x:.1f},{target.y:.1f}) bbox={target.bbox} "
              f"沿轴={along:.1f} 横向={cross:.1f} 横向比={cross / max(abs(along), 1.0):.2f} "
              f"欧氏={np.hypot(target.x - piece.x, target.y - anchor_y):.1f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
