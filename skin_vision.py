"""Conservative, hue-independent fallback for round-head/tapered-body skins.

This module never captures a window or emits input. It returns a match only
when head/body geometry, material colour consistency and platform support
agree. Multiple plausible people are deliberately treated as ambiguous.
"""
from dataclasses import dataclass
import math

import cv2
import numpy as np


@dataclass
class SkinMatch:
    x: float
    y: float
    bbox: tuple[int, int, int, int]
    score: float
    mask: np.ndarray
    support_points: tuple = ()


def has_horizontal_character_mask(mask, min_area, max_area, min_foot_ratio):
    """Reject a substantial lower-screen coloured blob rotated onto its side.

    Only used when no upright HSV candidate or complete skin shape exists;
    an upright, validated character is never vetoed by an unrelated platform.
    """
    height = mask.shape[0]
    _n, _labels, stats, _centres = cv2.connectedComponentsWithStats(mask, 8)
    return any(min_area <= area <= max_area and by + bh >= height * min_foot_ratio
               and bw > bh * 1.25 for _bx, by, bw, bh, area in stats[1:])


def find_skin_piece(frame, support_mask=None, min_foot_ratio=0.5):
    if frame is None or frame.ndim != 3 or frame.shape[2] != 3:
        return None
    height, width = frame.shape[:2]
    if min(height, width) < 32:
        return None
    if support_mask is not None:
        if (support_mask.shape != (height, width) or not support_mask.any()
                or (support_mask > 0).mean() > .90):
            return None  # broken/occluded captures cannot prove platform support
    top = int(height * 0.35)
    crop = frame[top:]
    lab = cv2.cvtColor(crop, cv2.COLOR_BGR2LAB)
    edges = cv2.Canny(cv2.cvtColor(crop, cv2.COLOR_BGR2GRAY), 25, 70)
    # Lab chroma edges preserve equal-luminance but different-colour skins.
    # They are not a hue allowlist and do not change CUDA/OpenCL mask kernels.
    for channel in cv2.split(lab):
        edges |= cv2.Canny(channel, 20, 60)
    contours, _ = cv2.findContours(edges, cv2.RETR_LIST, cv2.CHAIN_APPROX_SIMPLE)
    shapes = []
    heads = []
    platforms = []
    for contour in contours:
        x, y, bw, bh = cv2.boundingRect(contour)
        if x <= 1 or x + bw >= width - 1 or bw < 5 or bh < 5:
            continue
        hull = cv2.convexHull(contour)
        area = cv2.contourArea(hull)
        if area < 20:
            continue
        solidity = cv2.contourArea(contour) / area
        shapes.append((x, y, bw, bh, contour, solidity))
        if bw >= width * .08 and area >= width * height * .003:
            platforms.append((bw, bh, area, hull))
        perimeter = cv2.arcLength(hull, True)
        circularity = 4 * math.pi * area / max(perimeter * perimeter, 1)
        if (max(8, width * .018) <= bw <= width * .10
                and .80 <= bw / bh <= 1.25 and circularity >= .84
                and solidity >= .72):
            heads.append((x, y, bw, bh, hull, circularity))

    matches = []
    for hx, hy, hw, hh, head, circularity in heads:
        diameter = (hw + hh) * .5
        for bx, by, bw, bh, body, solidity in shapes:
            if not (.55 <= bw / diameter <= 1.65 and 1.15 <= bh / diameter <= 3.8
                    and solidity >= .86 and bh / bw >= 1.15):
                continue
            gap = by - (hy + hh)
            if not (-2 <= gap <= diameter * .35):
                continue
            if abs((bx + bw * .5) - (hx + hw * .5)) > diameter * .25:
                continue
            foot_y = by + bh + top
            foot_x = bx + bw * .5
            if foot_y < height * min_foot_ratio or foot_y >= height - 2:
                continue
            x0, x1 = min(hx, bx), max(hx + hw, bx + bw)
            y0, y1 = hy, by + bh
            if not (1.5 <= (y1 - y0) / (x1 - x0) <= 4.8):
                continue
            body_mask = np.zeros((bh, bw), dtype=np.uint8)
            shifted_body = body - np.array([[[bx, by]]], dtype=np.int32)
            cv2.drawContours(body_mask, [shifted_body], -1, 255, cv2.FILLED)
            row_widths = (body_mask > 0).sum(axis=1)
            neck = float(np.median(row_widths[max(1, bh // 8):max(2, bh // 3)]))
            base = float(np.median(row_widths[bh // 2:max(bh // 2 + 1, bh * 7 // 8)]))
            if neck < 3 or base < neck * 1.15:
                continue  # reject rods, cylinders and rectangular bottle caps
            head_mask = np.zeros((hh, hw), dtype=np.uint8)
            cv2.drawContours(head_mask, [head - np.array([[[hx, hy]]], dtype=np.int32)],
                             -1, 255, cv2.FILLED)
            kernel = np.ones((3, 3), np.uint8)
            head_core = cv2.erode(head_mask, kernel) > 0
            body_core = cv2.erode(body_mask, kernel) > 0
            if not head_core.any() or not body_core.any():
                continue
            head_colour = np.median(lab[hy:hy + hh, hx:hx + hw][head_core], axis=0)
            body_colour = np.median(lab[by:by + bh, bx:bx + bw][body_core], axis=0)
            if np.linalg.norm(head_colour - body_colour) > 35:
                continue
            support_points = ()
            if support_mask is not None:
                hits = 0
                for delta in (6, 10, 14, 18, 24):
                    sy = int(round(foot_y)) + delta
                    sx = int(round(foot_x))
                    if sy < height and support_mask[sy, max(0, sx - 4):min(width, sx + 5)].any():
                        hits += 1
                if hits < 2:
                    # White tops can have no HSV mask although their platform
                    # outline is complete. Confirm a broad supporting surface
                    # geometrically; do not expand the global white threshold.
                    witnesses = []
                    for pw, ph, area, hull in platforms:
                        if pw < diameter * 2 or ph < diameter or area < diameter * diameter * 2:
                            continue
                        points = []
                        supported_rows = 0
                        for delta in (6, 10, 14, 18, 24):
                            sy = int(round(foot_y)) + delta
                            sx = int(round(foot_x))
                            if sy >= height:
                                continue
                            row = [(px, sy) for px in range(max(0, sx - 4), min(width, sx + 5))
                                   if cv2.pointPolygonTest(hull, (float(px), float(sy - top)), False) >= 0]
                            if len(row) >= 5:
                                supported_rows += 1
                                points.extend(row)
                        if supported_rows >= 2:
                            witnesses.append(tuple(points))
                    if not witnesses:
                        continue
                    support_points = max(witnesses, key=len)
            mask = np.zeros((y1 - y0, x1 - x0), dtype=np.uint8)
            mask[hy - y0:hy - y0 + hh, hx - x0:hx - x0 + hw] |= head_mask
            mask[by - y0:by - y0 + bh, bx - x0:bx - x0 + bw] |= body_mask
            candidate = SkinMatch(foot_x, float(foot_y),
                                  (x0, y0 + top, x1 - x0, y1 - y0),
                                  float(min(circularity, solidity)), mask, support_points)
            duplicates = [i for i, old in enumerate(matches)
                          if math.hypot(old.x - candidate.x, old.y - candidate.y) < diameter * .35]
            if duplicates:
                index = duplicates[0]
                if candidate.score > matches[index].score:
                    matches[index] = candidate
            else:
                matches.append(candidate)
    return matches[0] if len(matches) == 1 else None
