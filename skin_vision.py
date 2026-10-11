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


def _aligned_body(shape, hx, hy, hw, hh):
    bx, by, bw, bh, _contour, solidity = shape
    diameter = (hw + hh) * .5
    return (.55 <= bw / diameter <= 1.65 and 1.15 <= bh / diameter <= 3.8
            and solidity >= .86 and bh / bw >= 1.15
            and -2 <= by - (hy + hh) <= diameter * .35
            and abs((bx + bw * .5) - (hx + hw * .5)) <= diameter * .25)


def _head_material(lab, head, hx, hy, hw, hh):
    """Sample material from a contour's eroded interior in this frame only."""
    head_mask = np.zeros((hh, hw), dtype=np.uint8)
    cv2.drawContours(head_mask, [head - np.array([[[hx, hy]]], np.int32)],
                     -1, 255, cv2.FILLED)
    core = cv2.erode(head_mask, np.ones((3, 3), np.uint8)) > 0
    if not core.any():
        return None
    head_pixels = lab[hy:hy + hh, hx:hx + hw][core].astype(np.float32)
    colour = np.median(head_pixels, axis=0)
    spread = np.percentile(np.linalg.norm(head_pixels[:, 1:] - colour[1:], axis=1), 80)
    return colour, float(np.clip(spread + 6, 12, 20)), core


def _material_plane(pixels, colour, chroma_tolerance):
    values = pixels.astype(np.float32)
    return (((np.linalg.norm(values - colour, axis=2) <= 35)
             & (np.linalg.norm(values[:, :, 1:] - colour[1:], axis=2) <= chroma_tolerance))
            .astype(np.uint8) * 255)


def _recover_round_heads(lab, fragment, hx, hy, hw, hh):
    """Propose a complete head from a broken edge, not a relaxed head gate.

    Glitter can split the top and bottom head arcs into two flattened contours.
    A tight, fragment-seeded local material component may recover the circle;
    cropped components and components not containing the seed are rejected.
    The caller still applies the original full-head geometry thresholds.
    """
    sample = _head_material(lab, fragment, hx, hy, hw, hh)
    if sample is None:
        return []
    colour, tolerance, seed = sample
    pad = int(math.ceil(max(hw, hh) * .4))
    x0, x1 = max(0, hx - pad), min(lab.shape[1], hx + hw + pad)
    y0, y1 = max(0, hy - pad), min(lab.shape[0], hy + hh + pad)
    plane = _material_plane(lab[y0:y1, x0:x1], colour, tolerance)
    plane = cv2.morphologyEx(plane, cv2.MORPH_CLOSE, np.ones((3, 3), np.uint8))
    contours, _ = cv2.findContours(plane, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    heads = []
    for contour in contours:
        x, y, w, h = cv2.boundingRect(contour)
        if x <= 0 or y <= 0 or x + w >= x1 - x0 or y + h >= y1 - y0:
            continue
        component = np.zeros(plane.shape, np.uint8)
        cv2.drawContours(component, [contour], -1, 255, cv2.FILLED)
        seed_hits = component[hy-y0:hy-y0+hh, hx-x0:hx-x0+hw][seed]
        if (seed_hits > 0).mean() < .6:
            continue
        heads.append(contour + np.array([[[x0, y0]]], np.int32))
    return heads


def _material_bodies(lab, head, hx, hy, hw, hh):
    """Recover an outline from this frame's head material, never a hue table.

    Gold glitter can join Canny edges to the platform and open an otherwise
    complete body contour. A head-seeded local Lab mask proposes a closed
    outline instead. Border-touching components are rejected: same-colour
    platforms/occlusion cannot invent a cropped, apparently tapered body.
    All normal geometry/material/support/uniqueness gates still apply.
    """
    diameter = (hw + hh) * .5
    sample = _head_material(lab, head, hx, hy, hw, hh)
    if sample is None:
        return []
    colour, chroma_tolerance, core = sample
    cx = hx + hw * .5
    x0 = max(0, int(math.floor(cx - diameter * .85)) - 2)
    x1 = min(lab.shape[1], int(math.ceil(cx + diameter * .85)) + 2)
    y0 = hy + hh
    y1 = min(lab.shape[0], int(math.ceil(y0 + diameter * 4.15)) + 2)
    if y1 <= y0 or x1 <= x0:
        return []
    material = _material_plane(lab[y0:y1, x0:x1], colour, chroma_tolerance)
    # Close single-pixel glitter gaps, not the space between head and body.
    kernel = np.ones((3, 3), np.uint8)
    planes = [cv2.morphologyEx(material, cv2.MORPH_CLOSE, kernel)]
    # A shaded head can look smooth even when its body has metallic boundary
    # holes. Repair scale comes from the validated head diameter, not a texture
    # or colour label. Keep the original proposal and all original validators.
    # Cropped/platform-connected components still fail below.
    repair_width = int(np.clip(round(diameter * .20) | 1, 3, 7))
    if repair_width > 3:
        repair = np.ones((repair_width, repair_width), np.uint8)
        planes.append(cv2.morphologyEx(material, cv2.MORPH_CLOSE, repair))
    head_l = lab[hy:hy + hh, hx:hx + hw, 0]
    head_texture = cv2.absdiff(head_l, cv2.GaussianBlur(head_l, (3, 3), 0))
    grain = float(np.median(head_texture[core]))
    if grain >= 1:
        # Metallic flecks can distinguish a gold body from a flat, same-colour
        # cardboard top. Derive the texture scale from the head in this frame;
        # it is an independent proposal, never an excuse to accept a cut body.
        roi_l = lab[y0:y1, x0:x1, 0]
        texture = cv2.absdiff(roi_l, cv2.GaussianBlur(roi_l, (3, 3), 0))
        textured = material & ((texture >= np.clip(grain * .5, 2, 4)).astype(np.uint8) * 255)
        textured = cv2.morphologyEx(textured, cv2.MORPH_CLOSE, kernel)
        # Flat platform seams are thin bridges, not the substantial body.
        width = max(3, int(round(diameter * .14)) | 1)
        substantial = np.ones((width, width), np.uint8)
        textured = cv2.morphologyEx(textured, cv2.MORPH_OPEN, substantial)
        planes.append(cv2.morphologyEx(textured, cv2.MORPH_CLOSE, substantial))
        # Use the head's *local* fleck coverage to propose a textured region,
        # not only isolated bright fleck pixels. Sparse gaps in a gold body
        # otherwise fragment it even when a nearby flat yellow top is cleanly
        # excluded. Material, geometry, waist, support and border gates remain
        # unchanged; thin seams still need to survive the substantial opening.
        threshold = float(np.clip(grain * .5, 2, 4))
        head_coverage = float((head_texture[core] >= threshold).mean())
        density = cv2.blur((texture >= threshold).astype(np.float32), (width, width))
        gate = float(np.clip(head_coverage * .6, .25, .5))
        region = material & ((density >= gate).astype(np.uint8) * 255)
        region = cv2.morphologyEx(region, cv2.MORPH_CLOSE, kernel)
        region = cv2.morphologyEx(region, cv2.MORPH_OPEN, substantial)
        planes.append(cv2.morphologyEx(region, cv2.MORPH_CLOSE, substantial))
    # Shading can quantize metallic flecks to just one Lab unit. A dense fine
    # texture witness in the head may propose a body region in all Lab channels;
    # this does not lower any final geometry/colour/support acceptance gate.
    head_lab = lab[hy:hy + hh, hx:hx + hw]
    head_detail = cv2.absdiff(head_lab, cv2.GaussianBlur(head_lab, (3, 3), 0)).max(axis=2)
    fine_coverage = float((head_detail[core] >= 1).mean())
    if fine_coverage >= .6:
        roi_lab = lab[y0:y1, x0:x1]
        detail = cv2.absdiff(roi_lab, cv2.GaussianBlur(roi_lab, (3, 3), 0)).max(axis=2)
        width = max(3, int(round(diameter * .14)) | 1)
        substantial = np.ones((width, width), np.uint8)
        density = cv2.blur((detail >= 1).astype(np.float32), (width, width))
        region = material & ((density >= np.clip(fine_coverage * .6, .25, .5)).astype(np.uint8) * 255)
        region = cv2.morphologyEx(region, cv2.MORPH_CLOSE, kernel)
        region = cv2.morphologyEx(region, cv2.MORPH_OPEN, substantial)
        planes.append(cv2.morphologyEx(region, cv2.MORPH_CLOSE, substantial))
    contours = []
    for plane in planes:
        found, _ = cv2.findContours(plane, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
        contours.extend(found)
    shapes = []
    for contour in contours:
        x, y, bw, bh = cv2.boundingRect(contour)
        if x <= 0 or y <= 0 or x + bw >= x1 - x0 or y + bh >= y1 - y0:
            continue
        hull_area = cv2.contourArea(cv2.convexHull(contour))
        if hull_area < 20:
            continue
        solidity = cv2.contourArea(contour) / hull_area
        shapes.append((x + x0, y + y0, bw, bh,
                       contour + np.array([[[x0, y0]]], np.int32), solidity))
    return shapes


def _has_waist(row_widths):
    height = len(row_widths)
    neck = float(np.median(row_widths[max(1, height // 8):max(2, height // 3)]))
    base = float(np.median(row_widths[height // 2:max(height // 2 + 1, height * 7 // 8)]))
    if neck >= 3 and base >= neck * 1.15:
        return True
    # Some skins have a broad shoulder/collar, with the waist farther down.
    # Require a sustained constriction followed by a wider base, not one noisy
    # row or a cylinder. Keep the same minimum widening as the original path.
    band = max(3, height // 10)
    stop = min(height // 2, height - band)
    widths = [float(np.median(row_widths[y:y + band]))
              for y in range(height // 4, stop + 1)]
    collar = float(np.median(row_widths[max(1, height // 12):max(2, height // 6)]))
    waist = min(widths, default=0)
    lower = [float(np.median(row_widths[y:y + band]))
             for y in range(height // 2, max(height // 2, height * 7 // 8 - band) + 1)]
    sustained_base = max(lower, default=0)
    return ((neck >= 3 and sustained_base >= neck * 1.15)
            or (waist >= 3 and waist < collar and sustained_base >= waist * 1.15))


def _nested_silhouette(first, first_head, second, second_head):
    """Coalesce an interior fragment only under the same validated head.

    Both proposals have already passed every complete-person validator. A
    material mask can nevertheless split a glitter body into a shorter closed
    outline. Sharing a head alone is insufficient: the smaller full silhouette
    must be enclosed and at least 95% covered by the larger one. Two separate
    heads or branching/non-nested bodies remain ambiguous.
    """
    ax, ay, aw, ah = first_head
    bx, by, bw, bh = second_head
    overlap = max(0, min(ax + aw, bx + bw) - max(ax, bx)) * max(
        0, min(ay + ah, by + bh) - max(ay, by))
    if overlap / max(aw * ah + bw * bh - overlap, 1) < .85:
        return False
    small, large = sorted((first, second), key=lambda m: m.bbox[2] * m.bbox[3])
    sx, sy, sw, sh = small.bbox
    lx, ly, lw, lh = large.bbox
    if sx < lx or sy < ly or sx + sw > lx + lw or sy + sh > ly + lh:
        return False
    core = small.mask > 0
    enclosing = large.mask[sy - ly:sy - ly + sh, sx - lx:sx - lx + sw] > 0
    return bool(core.any() and enclosing[core].mean() >= .95)


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
        elif (max(8, width * .018) <= bw <= width * .10
              and .65 <= bw / bh <= 1.55 and circularity >= .78 and solidity >= .72):
            for recovered in _recover_round_heads(lab, hull, x, y, bw, bh):
                rx, ry, rw, rh = cv2.boundingRect(recovered)
                recovered_hull = cv2.convexHull(recovered)
                recovered_area = cv2.contourArea(recovered_hull)
                if recovered_area < 20:
                    continue
                perimeter = cv2.arcLength(recovered_hull, True)
                roundness = 4 * math.pi * recovered_area / max(perimeter * perimeter, 1)
                recovered_solidity = cv2.contourArea(recovered) / recovered_area
                if (max(8, width * .018) <= rw <= width * .10
                        and .80 <= rw / rh <= 1.25 and roundness >= .84
                        and recovered_solidity >= .72):
                    heads.append((rx, ry, rw, rh, recovered_hull, roundness))

    matches = []
    match_heads = []
    for hx, hy, hw, hh, head, circularity in heads:
        diameter = (hw + hh) * .5
        bodies = [shape for shape in shapes if _aligned_body(shape, hx, hy, hw, hh)]
        # An edge proposal may pass the bounding box gates but later fail its
        # silhouette/material checks. Keep independent material proposals too;
        # their existence never bypasses any of the common validators below.
        bodies += [shape for shape in _material_bodies(lab, head, hx, hy, hw, hh)
                   if _aligned_body(shape, hx, hy, hw, hh)]
        for bx, by, bw, bh, body, solidity in bodies:
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
            if not _has_waist(row_widths):
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
            candidate_head = (hx, hy + top, hw, hh)
            duplicates = [i for i, old in enumerate(matches)
                          if math.hypot(old.x - candidate.x, old.y - candidate.y) < diameter * .35
                          or _nested_silhouette(old, match_heads[i], candidate, candidate_head)]
            if duplicates:
                index = duplicates[0]
                old = matches[index]
                nested = _nested_silhouette(old, match_heads[index], candidate, candidate_head)
                # For proven nesting the longer validated silhouette represents
                # the foot; a high-score interior fragment cannot truncate it.
                if ((nested and candidate.y > old.y)
                        or (candidate.score > old.score and (not nested or candidate.y == old.y))):
                    matches[index] = candidate
                    match_heads[index] = candidate_head
            else:
                matches.append(candidate)
                match_heads.append(candidate_head)
    return matches[0] if len(matches) == 1 else None
