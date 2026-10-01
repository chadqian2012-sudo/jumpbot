#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""离线"跳跃周期"基准：量出一次跳跃里**我们代码**要花多少毫秒（不需要游戏窗口）。

做法：用真实游戏帧 + 假抓帧器/假窗口，跑真实代码路径：
    acquire_frame（抓帧识别 + 两帧一致性确认）
    wait_landing （落地 settle：3 帧稳定 + 脚踩方块）
    read_score   （HUD 读分）
    CSV/状态写入
真实运行里的"每跳总间隔"减去这个基准，就是游戏自身动画（棋子飞行 + 镜头推移）占的时间。

用法：
    python tools\\bench_loop.py
    python tools\\bench_loop.py --frame debug\\dryrun\\dry_000.png --repeat 5
"""
from __future__ import annotations

import argparse
import os
import statistics
import sys
import time
from pathlib import Path

import cv2

PROJECT_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_DIR))

import jumpbot as J  # noqa: E402


class FakeWin:
    def __init__(self, frame):
        h, w = frame.shape[:2]
        self.hwnd = 1
        self.pid = 1
        self.title = "fake"
        self.exe = "fake.exe"
        self.client_rect = (0, 0, w, h)

    @property
    def width(self):
        return self.client_rect[2] - self.client_rect[0]

    @property
    def height(self):
        return self.client_rect[3] - self.client_rect[1]

    def describe(self):
        return "fake window"


class FakeCap:
    """每次 grab 返回同一帧（模拟"画面已经稳定"的落地状态）。"""

    def __init__(self, frame):
        self.frame = frame
        self.calls = 0

    def grab(self, win):
        self.calls += 1
        return self.frame.copy()

    def close(self):
        pass


class FakeCtrl:
    """**绝不会**发出真实输入的控制器。

    事故记录（2026-10-01 10:16）：本基准第一版误用了真实 InputController，又选了"开始页"
    的帧，于是 acquire_frame 走了"检测到界面按钮 → 点击"的分支，向屏幕坐标连续发送左键
    点击（那位置当时是《坦克世界》窗口）。现在这里把 click/press 全部变成空操作并计数，
    并且启动前显式校验：一旦检测到 UI 页面（开始/结算）就直接退出，不做任何基准。
    """

    def __init__(self):
        self.clicks = 0
        self.presses = 0

    def click(self, win, fx, fy):
        self.clicks += 1

    def press(self, win, fx, fy, hold_s):
        self.presses += 1


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="离线跳跃周期基准")
    ap.add_argument("--frame", default=str(PROJECT_DIR / "debug" / "dryrun" / "dry_000.png"))
    ap.add_argument("--config", default=str(PROJECT_DIR / "config.json"))
    ap.add_argument("--repeat", type=int, default=5)
    args = ap.parse_args()

    frame = cv2.imread(args.frame)
    if frame is None:
        print(f"读不到帧：{args.frame}", file=sys.stderr)
        return 2
    # 兜底：即使代码走偏，InputController 也会拒绝一切真实输入（见 jumpbot.InputController）
    os.environ["JUMPBOT_NO_INPUT"] = "1"
    cfg = J.Config.load(Path(args.config))
    # 安全闸 1：必须是"游戏中"的帧，不能是开始/结算页（否则会走点击分支）
    state, _btn, _sc = J.classify_screen(frame)
    if state in (J.ScreenState.START, J.ScreenState.GAME_OVER):
        print(f"拒绝运行：{Path(args.frame).name} 被识别为界面页（{state}），"
              f"请换一张游戏中截图", file=sys.stderr)
        return 2
    vision = J.Vision(cfg)
    win = FakeWin(frame)
    J.resolve_window = lambda cfg_, wait_s=0.0: win      # 假窗口：不碰真实桌面
    # 安全闸 2：控制器替换为空操作（绝不发送真实鼠标事件）
    ctrl = FakeCtrl()

    def timed(fn, repeat):
        out = []
        for _ in range(repeat):
            t0 = time.perf_counter()
            fn()
            out.append((time.perf_counter() - t0) * 1000.0)
        return statistics.median(out)

    cap = FakeCap(frame)
    acquire = timed(lambda: J.acquire_frame(cap, cfg, vision, ctrl,
                                           False, {"fresh": False}), args.repeat)
    vision.reset_prior()
    landing = timed(lambda: J.wait_landing(cap, win, vision, cfg), args.repeat)
    score = timed(lambda: J.read_score(frame, cfg), args.repeat * 10)
    masks = timed(lambda: vision.masks(frame), args.repeat)
    detect = timed(lambda: vision.detect(frame), args.repeat)

    print(f"帧：{Path(args.frame).name}（{frame.shape[1]}x{frame.shape[0]}）"
          f"，每项取 {args.repeat} 次中位")
    print(f"  detect()        {detect:7.1f} ms   ← masks {masks:.1f} ms")
    print(f"  acquire_frame() {acquire:7.1f} ms   ← 抓帧 + 识别 + 两帧一致性")
    print(f"  wait_landing()  {landing:7.1f} ms   ← 落地 settle（假抓帧恒稳定 → 走最短路径）")
    print(f"  read_score()    {score:7.1f} ms")
    if ctrl.clicks or ctrl.presses:
        print(f"  ⚠ 本应无输入，但记录了 clicks={ctrl.clicks} presses={ctrl.presses}")
    code_total = acquire + landing + score
    print(f"  ── 代码侧合计 ≈ {code_total:6.0f} ms（不含按压时长与游戏动画）")
    print()
    print(f"  参考：真实每跳总间隔 2000 ms、按压 601 ms → 游戏动画 ≈ "
          f"{2000 - 601 - code_total:.0f} ms（由总间隔 − 按压 − 代码侧得出）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
