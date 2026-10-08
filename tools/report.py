#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""离线复盘一轮 run_*.csv：复刻主程序的读分判连击逻辑，输出验收口径的统计。

用法：
    python tools/report.py                     # 自动取 logs/ 下最新一轮
    python tools/report.py logs/run_xxx.csv    # 指定某一轮
    python tools/report.py --json              # 附带 JSON 摘要

统计口径（与 jumpbot.py 完全一致，便于验收对照）：
    - 落地率 = landed / 所有已发起的跳跃（不可核对不算成功落地）
    - 真实命中率 = score_hits / (score_hits + score_misses)
      判据来自 HUD 分数增量：delta==1 → 普通落点；delta==min(2×连击,32) → 命中中心；
      偶数 delta≥4 且 ≤32 → 计数错位，按增量反推连击数（仍记为命中）；
      其它 → unknown，不计入分母。分数变小视为换局，重置连击。
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _scorelog import as_float, as_int, iter_labels, parse_csv  # noqa: E402

PROJECT_DIR = Path(__file__).resolve().parent.parent
LOGS_DIR = PROJECT_DIR / "logs"


def wilson(successes: int, total: int) -> list[float] | None:
    """Two-sided 95% Wilson interval, expressed in percent."""
    if not total:
        return None
    z = 1.959963984540054
    p = successes / total
    denominator = 1 + z * z / total
    center = (p + z * z / (2 * total)) / denominator
    radius = z * math.sqrt(p * (1 - p) / total + z * z / (4 * total * total)) / denominator
    return [round(max(0, center - radius) * 100, 2), round(min(1, center + radius) * 100, 2)]


def analyze(rows: list[dict]) -> dict:
    landed = died = 0
    hits = misses = unknown = 0
    streak = 0
    best_streak = 0
    alive = 0
    best_alive = 0
    prev_score = None
    life_max_score = None
    best_life_score = None
    death_indices: list[int] = []
    reset_indices: list[int] = []
    errors: list[float] = []
    dists: list[float] = []
    holds: list[int] = []
    for row, label, streak in iter_labels(rows):
        idx = as_int(row.get("index"))
        result = (row.get("result") or "").strip()
        err = as_float(row.get("error_px"))
        dist = as_float(row.get("dist_px"))
        hold = as_int(row.get("hold_ms"))
        if err is not None and abs(err) < 9999:
            errors.append(err)
        if dist is not None:
            dists.append(dist)
        if hold is not None:
            holds.append(hold)

        if result == "landed":
            landed += 1
            alive += 1
            best_alive = max(best_alive, alive)
        elif result in ("died", "dead"):
            died += 1
            if idx is not None:
                death_indices.append(idx)
            if life_max_score is not None:
                best_life_score = max(best_life_score or 0, life_max_score)
            life_max_score = None
            alive = 0

        score = as_int(row.get("score"))
        if score is not None:
            if life_max_score is None or score > life_max_score:
                life_max_score = score
            if prev_score is not None and score < prev_score and idx is not None:
                reset_indices.append(idx)      # 换局（分数回落）
            prev_score = score
        if label == 1:
            hits += 1
        elif label == 0:
            misses += 1
        elif result not in ("died", "dead", "stopped"):
            unknown += 1
        best_streak = max(best_streak, streak)

    if life_max_score is not None:
        best_life_score = max(best_life_score or 0, life_max_score)

    # 误判死亡：死亡行之后的分数比它高 → 游戏其实没结束（同一条命还在涨分）。
    # 成因与修复见 README「误判死亡」；这里把它变成常规指标，防止以后悄悄回归。
    scores = [as_int(r.get("score")) for r in rows]
    false_death_indices: list[int] = []
    for i, row in enumerate(rows):
        if (row.get("result") or "").strip() not in ("died", "dead"):
            continue
        cur = as_int(row.get("score"))
        nxt = next((s for s in scores[i + 1: i + 3] if s is not None), None)
        if cur is not None and nxt is not None and nxt > cur:
            idx = as_int(row.get("index"))
            if idx is not None:
                false_death_indices.append(idx)

    judged = hits + misses
    attempts = sum((r.get("result") or "").strip() != "stopped" for r in rows)
    return {
        "rows": len(rows),
        "attempts": attempts,
        "landed": landed,
        "died": died,
        "unresolved": max(0, attempts - landed - died),
        "land_rate": round(landed / attempts * 100, 2) if attempts else None,
        "land_rate_ci95": wilson(landed, attempts),
        "score_hits": hits,
        "score_misses": misses,
        "score_unknown": unknown,
        "hit_rate": round(hits / judged * 100, 2) if judged else None,
        "hit_rate_ci95": wilson(hits, judged),
        "judgement_coverage": round(judged / attempts * 100, 2) if attempts else None,
        "best_streak": best_streak,
        "best_alive": best_alive,
        "best_life_score": best_life_score,
        "death_indices": death_indices,
        "false_death_indices": false_death_indices,
        "reset_indices": reset_indices,
        "err_mean": round(sum(errors) / len(errors), 1) if errors else None,
        "err_median": round(sorted(errors)[len(errors) // 2], 1) if errors else None,
        "err_abs_median": (
            round(sorted(abs(e) for e in errors)[len(errors) // 2], 1) if errors else None
        ),
        "dist_median": round(sorted(dists)[len(dists) // 2], 1) if dists else None,
        "hold_median": sorted(holds)[len(holds) // 2] if holds else None,
    }


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="复盘一轮 jumpbot CSV 日志")
    ap.add_argument("csv", nargs="?", help="CSV 路径，默认取 logs/ 下最新")
    ap.add_argument("--json", action="store_true", help="额外输出 JSON")
    args = ap.parse_args()

    if args.csv:
        path = Path(args.csv)
    else:
        candidates = sorted(LOGS_DIR.glob("run_*.csv"), key=lambda p: p.stat().st_mtime)
        if not candidates:
            print("logs/ 下没有 run_*.csv", file=sys.stderr)
            return 2
        path = candidates[-1]
    if not path.exists():
        print(f"找不到 {path}", file=sys.stderr)
        return 2

    rows = parse_csv(path)
    s = analyze(rows)
    print(f"文件: {path}")
    print(f"总跳数: {s['rows']}")
    print(f"落地/死亡: {s['landed']} / {s['died']}  → 落地率 {s['land_rate']}%")
    judged = s["score_hits"] + s["score_misses"]
    hit_text = f"{s['hit_rate']}%" if s["hit_rate"] is not None else "n/a"
    print(f"真实命中/普通/无法判定: {s['score_hits']} / {s['score_misses']} / {s['score_unknown']}"
          f"  → 命中率 {hit_text}（分母 {judged}）")
    print(f"判定覆盖率: {s['judgement_coverage']}%；落地/命中 95% Wilson 区间: "
          f"{s['land_rate_ci95']} / {s['hit_rate_ci95']}")
    recent = analyze(rows[-100:])
    print(f"最近 {len(rows[-100:])} 跳: 落地 {recent['land_rate']}%，命中 {recent['hit_rate']}%，"
          f"覆盖率 {recent['judgement_coverage']}%")
    print(f"最长连续命中: {s['best_streak']}；单条命最长存活: {s['best_alive']} 跳；"
          f"单条命最高分: {s['best_life_score']}")
    if s["death_indices"]:
        print(f"死亡位置: {s['death_indices']}")
    if s["false_death_indices"]:
        print(f"⚠ 误判死亡（游戏仍在继续，见 README）: {s['false_death_indices']}")
    print(f"落点误差: 均值 {s['err_mean']}px / 中位 {s['err_median']}px / |误差|中位 {s['err_abs_median']}px")
    print(f"距离中位 {s['dist_median']}px；按压中位 {s['hold_median']}ms")
    if args.json:
        print(json.dumps(s, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
