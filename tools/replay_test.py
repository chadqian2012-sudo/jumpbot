#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""控制逻辑重放回归：用**真实录制的连续帧**验证 落地 settle 与 起跳前一致性确认。

现有测试面（regress.py）只覆盖"单帧识别"，状态机/落地判据没有回归保护。
这个脚本用历史 `--save-debug` 录下的三连帧做重放：
    jump_NNN_before.png  起跳前（画面稳定）
    jump_NNN_raw.png     按下瞬间（画面在动）
    jump_NNN_after.png   落地后（画面稳定）
重放序列 [raw, after, after, after, ...] 给 wait_landing，必须判为 landed；
重放 [before, before] 给 acquire_frame，必须给出棋子+目标。

安全：全程 FakeCtrl（绝不发真实输入）+ 假窗口 + JUMPBOT_NO_INPUT=1。

用法：
    python tools\\replay_test.py
    python tools\\replay_test.py --dir debug\\run_20260930_195704 --max 12
"""
from __future__ import annotations

import argparse
import os
import re
import statistics
import sys
import time
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402


class FakeCap:
    """按预设帧序列返回；序列用完后重复最后一帧（模拟"画面已稳定"）。"""

    def __init__(self, frames):
        self.frames = list(frames)
        self.i = 0

    def grab(self, win):
        frame = self.frames[min(self.i, len(self.frames) - 1)]
        self.i += 1
        return frame.copy()

    def close(self):
        pass


class FakeCtrl:
    def __init__(self):
        self.clicks = 0
        self.presses = 0

    def click(self, win, fx, fy):
        self.clicks += 1

    def press(self, win, fx, fy, hold_s):
        self.presses += 1


class FakeWin:
    def __init__(self, frame):
        h, w = frame.shape[:2]
        self.hwnd = 1
        self.pid = 1
        self.title = "replay"
        self.exe = "replay.exe"
        self.client_rect = (0, 0, w, h)

    @property
    def width(self):
        return self.client_rect[2] - self.client_rect[0]

    @property
    def height(self):
        return self.client_rect[3] - self.client_rect[1]

    def describe(self):
        return "replay window"


def pick_dir(explicit: str | None) -> Path | None:
    if explicit:
        d = Path(explicit)
        return d if d.is_dir() else None
    best, best_n = None, 0
    for d in (PROJECT_DIR / "debug").glob("run_*"):
        if not d.is_dir():
            continue
        n = len(list(d.glob("jump_*_before.png")))
        if n > best_n and (d / "jump_001_raw.png").exists():
            best, best_n = d, n
    return best


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    os.environ["JUMPBOT_NO_INPUT"] = "1"
    ap = argparse.ArgumentParser(description="控制逻辑重放回归")
    ap.add_argument("--dir", default="", help="录制帧目录，默认自动挑帧最多的一轮")
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--max", type=int, default=12, help="最多重放几跳")
    args = ap.parse_args()

    run_dir = pick_dir(args.dir or None)
    if run_dir is None:
        print("找不到含 jump_NNN_{before,raw,after}.png 的目录", file=sys.stderr)
        return 2
    cfg = J.Config.load(Path(args.config))
    triples = []
    for before in sorted(run_dir.glob("jump_*_before.png")):
        m = re.search(r"jump_(\d+)_before", before.name)
        if not m:
            continue
        idx = m.group(1)
        raw = run_dir / f"jump_{idx}_raw.png"
        after = run_dir / f"jump_{idx}_after.png"
        if raw.exists() and after.exists():
            triples.append((before, raw, after))
    triples = triples[: args.max]
    print(f"重放目录：{run_dir.name}，取 {len(triples)} 跳")

    win = None
    J.resolve_window = lambda cfg_, wait_s=0.0: win      # 假窗口
    landmark = time.perf_counter()
    vision = J.Vision(cfg)
    vision.masks(cv2.imread(str(triples[0][1])))          # 预热（GPU 首次初始化）
    print(f"预热（含 GPU 初始化）{(time.perf_counter() - landmark) * 1000:.0f} ms")

    land_ok = acquire_ok = 0
    land_ms, acquire_ms = [], []
    failures = []
    for before, raw, after in triples:
        f_raw = cv2.imread(str(raw))
        f_after = cv2.imread(str(after))
        f_before = cv2.imread(str(before))
        if f_raw is None or f_after is None or f_before is None:
            failures.append((raw.name, "帧读不到"))
            continue
        win = FakeWin(f_before)
        # 1) 落地 settle：raw（在动）→ after 连续稳定
        cap = FakeCap([f_raw, f_after, f_after, f_after, f_after])
        vision.reset_prior()
        t0 = time.perf_counter()
        status, _f, piece, target, _bm = J.wait_landing(cap, win, vision, cfg)
        land_ms.append((time.perf_counter() - t0) * 1000)
        if status == "landed" and piece is not None:
            land_ok += 1
        else:
            failures.append((raw.name, f"wait_landing → {status} piece={piece}"))
        # 2) 起跳前一致性确认
        cap2 = FakeCap([f_before, f_before, f_before])
        vision.reset_prior()
        t0 = time.perf_counter()
        got = J.acquire_frame(cap2, cfg, vision, FakeCtrl(), False, {"fresh": False})
        acquire_ms.append((time.perf_counter() - t0) * 1000)
        if got is not None and got[3] is not None:
            acquire_ok += 1
        else:
            failures.append((before.name, "acquire_frame → 无目标"))

    n = len(triples)
    print(f"wait_landing 判为 landed：{land_ok}/{n}"
          f"（中位 {statistics.median(land_ms):.0f} ms）")
    print(f"acquire_frame 拿到目标：{acquire_ok}/{n}"
          f"（中位 {statistics.median(acquire_ms):.0f} ms）")
    if failures:
        print("失败明细：")
        for name, why in failures[:10]:
            print(f"  {name}: {why}")
        return 1
    print("重放回归：全部通过 ✓")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
