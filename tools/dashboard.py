#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""跳一跳实时状态面板（独立控制台窗口）。

两种数据源，按可用性自动选择：
  1) `debug/status.json`（jumpbot.py 每跳写一次）——信息最全：脚本状态、游戏状态、
     PID、窗口句柄、当前分数、实时命中率、连击、最近一跳……
  2) 退回到"读最新 run_*.csv"——不需要脚本配合，哪怕 bot 是旧版本也能看进度
     （只是没有"游戏状态"这一项，显示为 —）。

用法：
    python tools\\dashboard.py                 # 自动选最新日志
    python tools\\dashboard.py --interval 0.5
    python tools\\dashboard.py --once          # 只渲染一帧（调试用）
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _scorelog import as_int, iter_labels, parse_csv  # noqa: E402

PROJECT_DIR = Path(__file__).resolve().parent.parent
LOGS_DIR = PROJECT_DIR / "logs"
DEFAULT_STATUS = PROJECT_DIR / "debug" / "status.json"

# 进程扫描结果缓存（10 秒）与 CSV 解析缓存，见 bot_process / stats_from_csv
_PID_CACHE: tuple[float, int | None, str] = (0.0, None, "")
_CSV_CACHE: dict = {"key": None, "stats": None}

STATE_TEXT = {
    "starting": "启动中",
    "acquiring": "识别/等待画面",
    "playing": "游戏中",
    "pressing": "按压中",
    "landing": "等待落地",
    "dead": "死亡，重开中",
    "start_page": "开始页",
    "game_over": "结算页",
    "blocked_modal": "被弹窗遮挡",
    "waiting_window": "正在等待游戏窗口",
    "countdown": "开跑倒计时",
    "pausing": "准备暂停（收尾当前操作）",
    "paused": "已暂停（可以做其他事情）",
    "resuming": "恢复中（重新定位/识别）",
    "stopped": "已停止",
}


def enable_ansi() -> bool:
    """Windows 控制台打开 VT 转义支持；失败就退化成纯文本。"""
    if os.name != "nt":
        return True
    try:
        import ctypes
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.GetStdHandle(-11)
        mode = ctypes.c_uint32()
        if not kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
            return False
        return bool(kernel32.SetConsoleMode(handle, mode.value | 0x0004))
    except Exception:
        return False


def newest_csv() -> Path | None:
    files = sorted(LOGS_DIR.glob("run_*.csv"), key=lambda p: p.stat().st_mtime)
    return files[-1] if files else None


def lower_priority() -> None:
    """面板把自己降到 BELOW_NORMAL：渲染再忙也绝不和 bot 的识别/按压抢 CPU。"""
    if os.name != "nt":
        return
    try:
        import ctypes
        from ctypes import wintypes
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        kernel32.SetPriorityClass.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel32.SetPriorityClass.restype = wintypes.BOOL
        kernel32.SetPriorityClass(kernel32.GetCurrentProcess(), 0x00004000)
    except Exception:
        pass


def log_exit(reason: str) -> None:
    """记录面板为什么退出（早期版本曾出现"被 bot 启动后立刻消失"，靠这个定位）。"""
    try:
        path = PROJECT_DIR / "debug" / "panel_exit.log"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as fh:
            fh.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} pid={os.getpid()} {reason}\n")
    except Exception:
        pass


def bot_process() -> tuple[int | None, str]:
    """从进程表里找正在跑的 jumpbot.py（仅 CSV 回退模式需要）。

    结果缓存 10 秒：早期版本每次刷新（0.5 秒）都起一个 PowerShell 去查进程表，
    实测 10 秒采样里能反复抓到 powershell 进程——而 bot 的按压末段是**忙等**，
    这种周期性进程创建/销毁会干扰按压时长，必须避免。
    """
    global _PID_CACHE
    if os.name != "nt":
        return None, ""
    now = time.time()
    ts, pid, cmd = _PID_CACHE
    if now - ts < 10.0:
        return pid, cmd
    try:
        out = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "Get-CimInstance Win32_Process -Filter \"Name like '%python%'\" | "
             "Select-Object ProcessId,CommandLine | ConvertTo-Json -Compress"],
            capture_output=True, text=True, timeout=8,
            creationflags=0x08000000,  # CREATE_NO_WINDOW
        ).stdout.strip()
        if not out:
            return None, ""
        data = json.loads(out)
        if isinstance(data, dict):
            data = [data]
        for item in data:
            cmd = (item.get("CommandLine") or "")
            if "jumpbot.py" in cmd and "--dry-run" not in cmd:
                _PID_CACHE = (now, int(item.get("ProcessId")), cmd)
                return _PID_CACHE[1], cmd
    except Exception:
        pass
    _PID_CACHE = (now, None, "")
    return None, ""


def stats_from_csv(path: Path) -> dict:
    """按 (路径, mtime, size) 缓存：面板每 0.5 秒刷新一次，不能每次都全量解析。"""
    global _CSV_CACHE
    try:
        st = path.stat()
    except OSError:
        st = None
    key = (str(path), st.st_mtime if st else 0.0, st.st_size if st else 0)
    if _CSV_CACHE.get("key") == key and _CSV_CACHE.get("stats") is not None:
        return _CSV_CACHE["stats"]
    rows = parse_csv(path)
    jumps = landed = deaths = 0
    hits = misses = unknown = 0
    streak = best_streak = 0
    score = None
    best_life_score = 0
    life_score = 0
    last = None
    # 判分口径统一走 _scorelog.iter_labels（与 report.py / miss_report.py 同一份实现）
    for row, label, row_streak in iter_labels(rows):
        jumps += 1
        result = (row.get("result") or "").strip()
        if result == "landed":
            landed += 1
        elif result in ("dead", "died"):
            deaths += 1
        s = as_int(row.get("score"))
        if s is not None:
            score = s
            life_score = s
            best_life_score = max(best_life_score, s)
        if label == 1:
            hits += 1
        elif label == 0:
            misses += 1
        elif label == -1:
            unknown += 1
        streak = row_streak
        best_streak = max(best_streak, row_streak)
        last = row
    judged = hits + misses
    out = {
        "jumps": jumps, "landed": landed, "deaths": deaths,
        "hits": hits, "misses": misses, "unknown": unknown,
        "hit_rate": (hits / judged * 100) if judged else None,
        "streak": streak, "best_streak": best_streak,
        "score": score, "life_score": life_score, "best_life_score": best_life_score,
        "last": last, "csv": str(path),
        "mtime": path.stat().st_mtime,
    }
    _CSV_CACHE = {"key": key, "stats": out}
    return out


def progress_bar(ratio: float, width: int = 28) -> str:
    ratio = max(0.0, min(1.0, ratio))
    filled = int(round(ratio * width))
    return "█" * filled + "░" * (width - filled)


def render(status: dict | None, csv_stats: dict | None, bot_pid: int | None, ansi: bool) -> str:
    lines = []
    now = time.strftime("%H:%M:%S")

    # 状态文件优先：它有游戏状态/窗口/PID/最近一跳等 CSV 里没有的信息。
    # 没有 status.json（或已过期）时才退回 CSV 统计。
    if status:
        hits = int(status.get("hits", 0))
        misses = int(status.get("misses", 0))
        judged = hits + misses
        view = {
            "score": status.get("score"),
            "best_life_score": status.get("best_life_score", 0),
            "hits": hits, "misses": misses, "unknown": int(status.get("unknown", 0)),
            "hit_rate": (hits / judged * 100) if judged else None,
            "landed": int(status.get("landed", 0)), "deaths": int(status.get("deaths", 0)),
            "streak": int(status.get("streak", 0)),
            "best_streak": int(status.get("best_streak", 0)),
            "jumps": int(status.get("jumps", 0)),
            "mtime": float(status.get("updated_at", 0)),
            "source": "status.json",
            "last": {
                "index": status.get("last_index", "?"),
                "timestamp": status.get("last_time", ""),
                "dist_px": status.get("last_dist_px", "?"),
                "hold_ms": status.get("last_hold_ms", "?"),
                "result": status.get("last_result", "?"),
                "score": status.get("score"),
                "note": status.get("last_note", ""),
            },
        }
    else:
        view = csv_stats

    lines.append(f"跳一跳 实时面板          {now}")
    lines.append("=" * 62)

    # 等待游戏窗口时只显示最简信息（用户要求：就一句"正在等待游戏窗口"）
    if status and status.get("state") == "waiting_window":
        lines.append(f"脚本 PID   : {status.get('pid')}")
        lines.append("")
        lines.append("正在等待游戏窗口…")
        lines.append(f"{status.get('pause_hotkey', 'Alt+P')} 暂停/恢复；F8 停止脚本")
        waited = status.get("waited_s")
        if waited:
            lines.append(f"已等待 {int(waited)} 秒")
        lines.append("=" * 62)
        lines.append("Ctrl+C 退出面板（不会停止脚本）")
        if ansi:
            return "\x1b[2J\x1b[H" + "\n".join(lines)
        return "\n".join(lines)

    if status:
        pid = status.get("pid")
        state = STATE_TEXT.get(status.get("state", ""), status.get("state", "?"))
        game = STATE_TEXT.get(status.get("game_state", ""), status.get("game_state") or "—")
        lines.append(f"脚本 PID   : {pid}     状态: {state}      游戏: {game}")
        win = status.get("window") or {}
        if win:
            lines.append(f"游戏窗口   : hwnd={win.get('hwnd')} pid={win.get('pid')} "
                         f"客户区={win.get('client')}")
        lines.append(f"运行时长   : {status.get('elapsed_text', '—')}    "
                     f"本轮进度: {status.get('jumps', 0)}/{status.get('max_jumps', '?')}")
        lines.append(f"暂停/恢复  : {status.get('pause_hotkey', 'Alt+P')} / 控制台 P"
                     f"    已暂停 {status.get('pause_total_s', 0):.1f} 秒")
        if status.get('hotkey_registered') is False:
            lines.append("全局热键   : 未启用（只有控制台 P 可切换暂停）")
        if status.get('hotkey_error'):
            lines.append(f"热键提示   : {status['hotkey_error']}（控制台 P 可用）")
        if status.get('countdown_s') is not None:
            lines.append(f"倒计时     : {status['countdown_s']:.1f} 秒")
        if status.get("mask_backend"):
            lines.append(f"掩码后端   : {status.get('mask_backend')}"
                         f"{'（' + str(status.get('mask_bench_ms')) + '）' if status.get('mask_bench_ms') else ''}")
        if status.get('piece_source'):
            mode = '自动颜色/轮廓适配' if status.get('skin_auto_enabled') else '旧模板模式'
            source = ('完整头身几何' if status['piece_source'] == 'skin-shape' else '初始颜色/模板')
            lines.append(f"人物识别   : {mode}；当前 {source}")
    else:
        src = csv_stats.get("csv") if csv_stats else "（无）"
        running = bot_pid is not None
        pid_text = f"{bot_pid}（可能是启动器进程）" if bot_pid else "—"
        lines.append(f"脚本 PID   : {pid_text}     "
                     f"状态: {'运行中（按 CSV 判断）' if running else '未检测到'}      游戏: —")
        lines.append(f"数据来源   : {Path(src).name if src else '—'}")

    if view:
        score = view.get("score")
        rate = view.get("hit_rate")
        lines.append("-" * 62)
        lines.append(f"当前分数   : {score if score is not None else '—'}"
                     f"        本局最高: {view.get('best_life_score', 0)}")
        if rate is None:
            lines.append("实时命中率 : —（还没有可判定的跳）")
        else:
            lines.append(f"实时命中率 : {rate:5.1f}%  {progress_bar(rate / 100)} "
                         f"({view['hits']}/{view['hits'] + view['misses']})")
        lines.append(f"落地/死亡  : {view['landed']} / {view['deaths']}"
                     f"    落地率: "
                     f"{(view['landed'] / max((status or {}).get('attempts', view['landed'] + view['deaths']), 1) * 100):.1f}%")
        if status and 'recent100' in status:
            recent = status['recent100']
            recent_rate = recent.get('hit_rate')
            text = '—' if recent_rate is None else f'{recent_rate:.1f}%'
            lines.append(f"最近100跳  : 命中 {text}；可判定 {recent.get('judged', 0)}"
                         f" / {recent.get('attempts', 0)}")
            coverage = status.get('judgement_coverage')
            if coverage is not None:
                lines.append(f"判定覆盖率 : {coverage:.1f}%    不可判定 {status.get('unknown', 0)}")
        lines.append(f"连击       : 当前 {view['streak']}   最长 {view['best_streak']}"
                     f"    跳数: {view['jumps']}")
        last = view.get("last") or {}
        if last and last.get("index") not in (None, "?"):
            lines.append("-" * 62)
            note = (last.get("note") or "").strip()
            lines.append(f"最近一跳 #{last.get('index', '?')} {last.get('timestamp', '')} "
                         f"d={last.get('dist_px', '?')}px hold={last.get('hold_ms', '?')}ms "
                         f"结果={last.get('result', '?')} 分数={last.get('score', '—')}")
            lines.append(f"        {note[:110]}")
        age = time.time() - view.get("mtime", time.time())
        lines.append("-" * 62)
        waiting = bool(status and status.get("state") in ("waiting_window", "paused", "countdown", "resuming"))
        stale = (age > 15) and not waiting
        lines.append(f"数据更新   : {age:.1f} 秒前（来源 {view.get('source', 'csv')}）"
                     + ("   ⏳ 控制等待/已暂停，不算卡住" if waiting
                        else ("   ⚠ 超过 15 秒未更新，脚本可能已停止/卡住" if stale else "")))
    else:
        lines.append("-" * 62)
        lines.append("等待脚本开始运行…（未找到 status.json，也没有 run_*.csv）")
    lines.append("=" * 62)
    lines.append("Ctrl+C 退出面板（不会停止脚本）")
    if ansi:
        return "\x1b[2J\x1b[H" + "\n".join(lines)
    return "\n".join(lines)


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    ap = argparse.ArgumentParser(description="跳一跳实时状态面板")
    ap.add_argument("--status", default=str(DEFAULT_STATUS))
    ap.add_argument("--csv", default="", help="指定 CSV；默认选最新")
    ap.add_argument("--interval", type=float, default=0.5)
    ap.add_argument("--once", action="store_true", help="只渲染一帧")
    ap.add_argument("--no-color", action="store_true")
    args = ap.parse_args()

    ansi = (not args.no_color) and enable_ansi()
    status_path = Path(args.status)
    interval = max(args.interval, 0.2)
    lower_priority()
    try:
        while True:
            frame_start = time.time()
            status = None
            if status_path.exists():
                try:
                    data = json.loads(status_path.read_text(encoding="utf-8"))
                    age = time.time() - float(data.get("updated_at", 0))
                    # 等待窗口时脚本只每 2 秒写一次心跳，这里放宽到 120 秒；
                    # 真停止时 status 里 alive=False，会走下面的停止分支。
                    if age < 120.0:
                        status = data
                except Exception:
                    status = None
            csv_path = Path(args.csv) if args.csv else newest_csv()
            csv_stats = None
            if csv_path and csv_path.exists():
                try:
                    csv_stats = stats_from_csv(csv_path)
                except Exception:
                    csv_stats = None
            # 有 status.json 时直接用里面的 pid，绝不为此起 PowerShell 查进程表
            if status is not None:
                pid, _cmd = status.get("pid"), ""
            else:
                pid, _cmd = bot_process()
            text = render(status, csv_stats, pid, ansi)
            if args.once:
                print(text.replace("\x1b[2J\x1b[H", ""))
                return 0
            if ansi:
                sys.stdout.write(text + "\n")
                sys.stdout.flush()
            else:
                # 不用 os.system("cls")：每帧都会起一个 cmd.exe（实测把刷新拖到 ~1.0 秒/帧），
                # 而面板要跑在 bot 旁边，不能周期性抢 CPU。用空行把上一帧顶出屏幕即可。
                print("\n" * 3)
                print(text)
                sys.stdout.flush()      # 输出被重定向到文件时也能实时看到（否则缓冲丢失）
            if status and (status.get("state") == "stopped" or status.get("alive") is False):
                log_exit(f"脚本已停止（state={status.get('state')} alive={status.get('alive')} "
                         f"stop_reason={status.get('stop_reason')}）")
                print("\n[面板] 脚本已停止，3 秒后自动关闭…")
                time.sleep(3)
                return 0
            # 按时间补偿：渲染本身可能耗时几十毫秒，sleep 里扣掉它，
            # 保证"数据更新率"稳定在 --interval（默认 0.5 秒）一次。
            elapsed = time.time() - frame_start
            time.sleep(max(0.05, interval - elapsed))
    except KeyboardInterrupt:
        log_exit("用户 Ctrl+C")
        print("\n[面板] 已退出（脚本不受影响）")
        return 0
    except BaseException as exc:                      # noqa: BLE001
        log_exit(f"异常退出：{type(exc).__name__}: {exc}")
        raise


if __name__ == "__main__":
    raise SystemExit(main())
