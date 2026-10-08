#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""日志复用的公共部分：CSV 读取 + 与主程序一致的"读分判连击"逻辑。

为什么单独抽出来：`report.py` 和 `miss_report.py` 早期各写了一份相同的判分逻辑，
两边一旦漂移，同一份 CSV 会算出两个命中率。真值口径只有一个，就放在这里。

判分规则（与 jumpbot.py 主循环一致）：
    delta == 1                      → 普通落点
    delta == min(2×连击, 32)        → 命中中心
    4 ≤ delta ≤ 32 且为偶数         → 连击计数错位，按增量反推连击（仍算命中）
    其它                            → 无法判定（不计入分母）
    分数比上一跳低                    → 换局，连击清零
"""
from __future__ import annotations

import csv
import re
from pathlib import Path

BONUS_CAP = 32
TAG_RE = re.compile(r"tb=(-?\d+),(-?\d+),(\d+),(\d+)\s+tw=(\d+)")


def parse_csv(path: Path) -> list[dict]:
    """用 csv 模块解析（不要自己 split(",")）。

    踩过的坑：note 列里带 `tb=x,y,w,h tw=NN` 这类逗号，csv.writer 会给整个字段加引号；
    手写 split 会把引号里的逗号也当分隔符，note 被截断、后面列全部错位——
    实测 3979/3980 行受影响，表现是"明明有新日志，几何分类却全是（旧日志无几何信息）"。
    """
    rows: list[dict] = []
    with Path(path).open("r", encoding="utf-8-sig", newline="") as fh:
        reader = csv.reader(fh)
        try:
            header = next(reader)
        except StopIteration:
            return rows
        for parts in reader:
            if not parts:
                continue
            if len(parts) < len(header):
                parts = parts + [""] * (len(header) - len(parts))
            rows.append(dict(zip(header, parts)))
    return rows


def iter_rows(paths):
    for path in paths:
        for row in parse_csv(path):
            row["_file"] = Path(path).name
            yield row


def as_int(value, default=None):
    try:
        return int(float(value))
    except (TypeError, ValueError):
        return default


def as_float(value, default=None):
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def iter_labels(rows):
    """逐行产出 (row, label, streak)。

    label 取值：
        1  = 命中中心
        0  = 普通落点
        -1 = 有分数但增量异常（读数错乱等），算"无法判定"，计入 unknown
        None = 不适用（非落地行、本局第一跳、分数回落换局）
    """
    streak = 0
    prev_score = None
    for row in rows:
        score = as_int(row.get("score"))
        label = None
        result = (row.get("result") or "").strip()
        if "mask_backend" in row:
            # New logs record the live HUD judgement after every resync. Never
            # infer a delta across a pause: intervening human input is not ours.
            note = row.get("note") or ""
            center = re.search(r"\[center x(\d+)(?:\(重同步\))?\]", note)
            if result == "landed" and center:
                label, streak = 1, int(center.group(1))
            elif result == "landed" and "[miss]" in note:
                label, streak = 0, 0
            else:
                label = -1 if result == "landed" else None
                streak = 0
            prev_score = score if result == "landed" else None
        elif result != "landed":
            prev_score = None
            streak = 0
        elif score is not None:
            if prev_score is None:
                pass
            elif score < prev_score:
                streak = 0
            else:
                delta = score - prev_score
                expect = min(2 * (streak + 1), BONUS_CAP)
                if delta == 1:
                    label, streak = 0, 0
                elif delta == expect:
                    label, streak = 1, streak + 1
                elif delta >= 4 and delta % 2 == 0 and delta <= BONUS_CAP:
                    label, streak = 1, min(delta // 2, BONUS_CAP // 2)
                else:
                    label = -1
            prev_score = score
        yield row, label, streak
