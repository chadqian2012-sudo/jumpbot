#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
跳一跳 屏幕识别自动脚本（自适应版）

流程：截取微信小游戏「跳一跳」窗口画面 -> CV 识别棋子与目标方块 ->
计算按压时长 -> 模拟鼠标按住/松开 -> 用落点误差在线修正系数 -> 死亡自动重开。

用法见 README.md，或运行：  python jumpbot.py --help
"""
from __future__ import annotations

import argparse
import csv
import ctypes
import json
import math
import os
import re
import subprocess
import sys
import threading
import time
from ctypes import wintypes
from dataclasses import dataclass, field, asdict
from pathlib import Path
from contextlib import nullcontext

from run_control import (RunControl, WindowsHotkeyListener, ControlInterrupted,
                         parse_hotkey, validate_timing)

import cv2
import numpy as np

def _is_frozen() -> bool:
    """是否运行在 PyInstaller 打包出来的 exe 里。"""
    return bool(getattr(sys, "frozen", False))


def _app_dir() -> Path:
    """可写目录：config/logs/debug 放这里（打包后 = exe 所在目录）。"""
    if _is_frozen():
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parent


def _bundle_dir() -> Path:
    """只读资源目录：templates/ 等（打包后在 sys._MEIPASS 解包目录）。"""
    if _is_frozen():
        return Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent))
    return Path(__file__).resolve().parent


PROJECT_DIR = _app_dir()          # 兼容旧代码里的用法：可写目录
DATA_DIR = _bundle_dir()          # 只读资源目录
DEFAULT_CONFIG_PATH = PROJECT_DIR / "config.json"
TEMPLATE_DIR = DATA_DIR / "templates"
LOG_DIR = PROJECT_DIR / "logs"
DEBUG_DIR = PROJECT_DIR / "debug"
STATUS_PATH = DEBUG_DIR / "status.json"

VK_F8 = 0x77
_CONTROL: RunControl | None = None


def log(msg: str) -> None:
    """带时间戳的控制台输出。"""
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


_STATUS: dict = {}
_CUDA_ENV_READY = False      # CUDA 运行时路径只需铺一次（否则 PATH 会被反复追加直到爆掉）
_OPENCL_RUNTIMES: dict = {}  # 进程内复用上下文/程序，Vision 各自持有队列、内核和缓冲区
_OPENCL_FAILED: set = set()
_OPENCL_CHECKED: set = set()


def status_write(**fields) -> None:
    """把当前运行状态写到 debug/status.json，供 tools/dashboard.py 实时显示。

    原子写（临时文件 + replace），失败一律吞掉——状态面板绝不能影响主流程。
    """
    if _CONTROL is not None:
        fields.update(_CONTROL.snapshot())
        if fields.get("state") != "stopped" and not _CONTROL.stopped:
            if _CONTROL.pause_requested:
                fields["state"] = "paused" if _CONTROL.paused_at is not None else "pausing"
            elif _CONTROL.needs_resume:
                fields["state"] = "resuming"
    _STATUS.update(fields)
    _STATUS["updated_at"] = time.time()
    try:
        STATUS_PATH.parent.mkdir(parents=True, exist_ok=True)
        tmp = STATUS_PATH.with_suffix(".tmp")
        tmp.write_text(json.dumps(_STATUS, ensure_ascii=False), encoding="utf-8")
        os.replace(tmp, STATUS_PATH)
    except Exception:
        pass


def spawn_dashboard() -> None:
    """在新控制台窗口里弹出实时状态面板（失败只提示，不影响运行）。"""
    try:
        script = DATA_DIR / "tools" / "dashboard.py"
        if not script.exists():
            log("未找到 tools/dashboard.py，跳过状态面板")
            return
        creation = 0x00000010 if os.name == "nt" else 0   # CREATE_NEW_CONSOLE
        if _is_frozen():
            # 打包后没有独立 python.exe：让 exe 自己以 --panel 模式跑面板脚本
            cmd = [sys.executable, "--panel", "--status", str(STATUS_PATH)]
        else:
            cmd = [sys.executable, str(script), "--status", str(STATUS_PATH)]
        subprocess.Popen(cmd, creationflags=creation, close_fds=True)
        log("已弹出实时状态面板窗口（PID/状态/分数/实时命中率）")
    except Exception as exc:
        log(f"启动状态面板失败（不影响运行）：{exc}")


def run_panel(rest_args: list) -> int:
    """以 --panel 启动实时面板（打包后复用同一个 exe）。"""
    script = DATA_DIR / "tools" / "dashboard.py"
    if not script.exists():
        print(f"找不到面板脚本：{script}", file=sys.stderr)
        return 2
    import runpy
    argv_backup = sys.argv
    try:
        sys.argv = [str(script)] + list(rest_args)
        runpy.run_path(str(script), run_name="__main__")
    except SystemExit as exc:                     # 面板用 SystemExit 返回码
        return int(exc.code or 0)
    finally:
        sys.argv = argv_backup
    return 0


def ensure_user_config() -> None:
    """打包后首次运行：把内置的默认 config.json 复制到 exe 旁边，方便用户直接改。"""
    try:
        if DEFAULT_CONFIG_PATH.exists():
            return
        src = DATA_DIR / "config.json"
        if src.exists() and src.resolve() != DEFAULT_CONFIG_PATH.resolve():
            DEFAULT_CONFIG_PATH.write_text(src.read_text(encoding="utf-8"), encoding="utf-8")
            log(f"已生成默认配置：{DEFAULT_CONFIG_PATH}")
    except Exception as exc:
        log(f"生成默认配置失败（将使用内置默认值）：{exc}")


def setup_stdio() -> None:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass


def make_dpi_aware() -> None:
    """使用物理像素坐标（本机 150% 缩放 -> 2560x1440）。"""
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception:
        try:
            ctypes.windll.user32.SetProcessDPIAware()
        except Exception:
            pass


_WINMM = None


def boost_timer_resolution() -> None:
    """把系统时钟粒度设成 1ms，避免 time.sleep 的 15.6ms 量化（≈42px 落点误差）。"""
    global _WINMM
    try:
        _WINMM = ctypes.WinDLL("winmm")
        _WINMM.timeBeginPeriod(1)
        log("已将系统定时器精度提升到 1ms")
    except Exception as exc:
        log(f"提升定时器精度失败（{exc}），按压时长会有约 15ms 量化误差")


def restore_timer_resolution() -> None:
    global _WINMM
    if _WINMM is not None:
        try:
            _WINMM.timeEndPeriod(1)
        except Exception:
            pass
        _WINMM = None


def effective_cv2_threads(threads: int) -> int:
    return min(4, os.cpu_count() or 1) if int(threads) == 0 else max(1, int(threads))


def tune_cpu(threads: int = 4, priority: str = "above") -> None:
    """CPU 侧加速：控制 OpenCV 线程数 + 提升本进程优先级。

    * OpenCV 线程数：我们的图只有 553x1050（约 0.6MP），实测**4 线程最快**
      （detect 20.31ms / masks 9.94ms），开满 20 线程反而更慢
      （22.70ms / 11.58ms，线程调度开销）。固定 4 线程还能避免每个算子都拉起
      20 个线程去和游戏抢 CPU。
    * 进程优先级：ABOVE_NORMAL 让识别/按压不被别的程序（微信渲染、面板）挤掉；
      面板自己用 BELOW_NORMAL，绝不和 bot 抢。
    """
    try:
        threads = effective_cv2_threads(threads)
        cv2.setNumThreads(threads)
        cv2.setUseOptimized(True)
    except Exception as exc:
        log(f"设置 OpenCV 线程数失败：{exc}")
    if os.name != "nt":
        return
    classes = {"above": 0x00008000, "below": 0x00004000, "normal": 0x00000020}
    val = classes.get(priority)
    if not val:
        return
    try:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        # 必须显式声明签名：否则伪句柄 -1 会被当成 32 位 int 传进去，
        # SetPriorityClass 失败且不报错（实测优先级一直是 Normal）。
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        kernel32.SetPriorityClass.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel32.SetPriorityClass.restype = wintypes.BOOL
        handle = kernel32.GetCurrentProcess()
        if kernel32.SetPriorityClass(handle, val):
            log(f"进程优先级已设为 {priority}（{val:#x}），OpenCV 线程数 "
                f"{cv2.getNumThreads()}")
        else:
            log(f"设置进程优先级失败（GetLastError={ctypes.get_last_error()}）")
    except Exception as exc:
        log(f"设置进程优先级失败：{exc}")


# --------------------------------------------------------------------------------------
# Win32 层
# --------------------------------------------------------------------------------------

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

user32.EnumWindows.argtypes = [WNDENUMPROC, wintypes.LPARAM]
user32.EnumWindows.restype = wintypes.BOOL
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.IsWindowVisible.restype = wintypes.BOOL
user32.IsIconic.argtypes = [wintypes.HWND]
user32.IsIconic.restype = wintypes.BOOL
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.restype = ctypes.c_int
user32.GetWindowTextLengthW.argtypes = [wintypes.HWND]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetClientRect.restype = wintypes.BOOL
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowRect.restype = wintypes.BOOL
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
user32.ClientToScreen.restype = wintypes.BOOL
user32.GetForegroundWindow.restype = wintypes.HWND
user32.SetForegroundWindow.argtypes = [wintypes.HWND]
user32.SetForegroundWindow.restype = wintypes.BOOL
user32.BringWindowToTop.argtypes = [wintypes.HWND]
user32.BringWindowToTop.restype = wintypes.BOOL
user32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
user32.ShowWindow.restype = wintypes.BOOL
user32.AttachThreadInput.argtypes = [wintypes.DWORD, wintypes.DWORD, wintypes.BOOL]
user32.AttachThreadInput.restype = wintypes.BOOL
user32.WindowFromPoint.argtypes = [wintypes.POINT]
user32.WindowFromPoint.restype = wintypes.HWND
user32.GetAncestor.argtypes = [wintypes.HWND, ctypes.c_uint]
user32.GetAncestor.restype = wintypes.HWND
user32.GetSystemMetrics.argtypes = [ctypes.c_int]
user32.GetSystemMetrics.restype = ctypes.c_int
user32.GetAsyncKeyState.argtypes = [ctypes.c_int]
user32.GetAsyncKeyState.restype = ctypes.c_short
user32.PostMessageW.argtypes = [wintypes.HWND, ctypes.c_uint, wintypes.WPARAM, wintypes.LPARAM]
user32.PostMessageW.restype = wintypes.BOOL
user32.PrintWindow.argtypes = [wintypes.HWND, wintypes.HDC, ctypes.c_uint]
user32.PrintWindow.restype = wintypes.BOOL
user32.GetWindowDC.argtypes = [wintypes.HWND]
user32.GetWindowDC.restype = wintypes.HDC
user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
user32.ReleaseDC.restype = ctypes.c_int
user32.keybd_event.argtypes = [wintypes.BYTE, wintypes.BYTE, wintypes.DWORD, ctypes.c_void_p]

kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.QueryFullProcessImageNameW.argtypes = [
    wintypes.HANDLE,
    wintypes.DWORD,
    wintypes.LPWSTR,
    ctypes.POINTER(wintypes.DWORD),
]
kernel32.GetCurrentThreadId.restype = wintypes.DWORD

gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
gdi32.CreateCompatibleDC.restype = wintypes.HDC
gdi32.CreateCompatibleBitmap.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int]
gdi32.CreateCompatibleBitmap.restype = wintypes.HBITMAP
gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
gdi32.SelectObject.restype = wintypes.HGDIOBJ
gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]
gdi32.DeleteDC.argtypes = [wintypes.HDC]
gdi32.GetDIBits.argtypes = [
    wintypes.HDC,
    wintypes.HBITMAP,
    wintypes.UINT,
    wintypes.UINT,
    ctypes.c_void_p,
    ctypes.c_void_p,
    wintypes.UINT,
]
gdi32.GetDIBits.restype = ctypes.c_int

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
GA_ROOT = 2


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class INPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("mi", MOUSEINPUT)]


INPUT_MOUSE = 0
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_ABSOLUTE = 0x8000
SM_CXSCREEN = 0
SM_CYSCREEN = 1

WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_LBUTTONUP = 0x0202
MK_LBUTTON = 0x0001


@dataclass
class WindowInfo:
    hwnd: int
    title: str
    pid: int
    exe: str
    client_rect: tuple

    @property
    def width(self) -> int:
        return self.client_rect[2] - self.client_rect[0]

    @property
    def height(self) -> int:
        return self.client_rect[3] - self.client_rect[1]

    def describe(self) -> str:
        l, t, r, b = self.client_rect
        name = Path(self.exe).name if self.exe else "?"
        return (
            f"hwnd={self.hwnd} pid={self.pid} exe={name} title={self.title!r} "
            f"client=({l},{t})-({r},{b}) size={self.width}x{self.height}"
        )


def _window_text(hwnd: int) -> str:
    n = user32.GetWindowTextLengthW(hwnd)
    buf = ctypes.create_unicode_buffer(n + 2)
    user32.GetWindowTextW(hwnd, buf, n + 1)
    return buf.value


def _window_pid(hwnd: int) -> int:
    pid = wintypes.DWORD(0)
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return int(pid.value)


def process_exe(pid: int) -> str:
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return ""
    try:
        buf = ctypes.create_unicode_buffer(1024)
        size = wintypes.DWORD(len(buf))
        if kernel32.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)):
            return buf.value
        return ""
    finally:
        kernel32.CloseHandle(handle)


def client_screen_rect(hwnd: int) -> tuple:
    rect = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(rect))
    origin = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(origin))
    return (origin.x, origin.y, origin.x + rect.right, origin.y + rect.bottom)


def find_windows(title_pattern: str, exe_hint: str = "") -> list:
    """按标题正则 + 进程名关键字查找顶层可见窗口，按客户区面积降序返回。"""
    pattern = re.compile(title_pattern)
    found: list = []

    @WNDENUMPROC
    def _cb(hwnd, _lparam):
        if not user32.IsWindowVisible(hwnd):
            return True
        title = _window_text(hwnd)
        if not title or not pattern.search(title):
            return True
        pid = _window_pid(hwnd)
        exe = process_exe(pid)
        if exe_hint and exe_hint.lower() not in exe.lower():
            return True
        rect = client_screen_rect(hwnd)
        if rect[2] - rect[0] <= 0 or rect[3] - rect[1] <= 0:
            return True
        info = WindowInfo(hwnd=int(hwnd), title=title, pid=pid, exe=exe, client_rect=rect)
        found.append(info)
        return True

    user32.EnumWindows(_cb, 0)
    found.sort(key=lambda wi: wi.width * wi.height, reverse=True)
    return found


def force_foreground(hwnd: int, timeout: float = 2.0) -> bool:
    """把窗口切到前台并确认成功（处理 Windows 前台锁定）。"""
    control_check()
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, 9)
    if user32.GetForegroundWindow() == hwnd:
        return True

    deadline = time.time() + timeout
    while time.time() < deadline:
        control_check()
        user32.keybd_event(0x12, 0, 0, None)
        user32.keybd_event(0x12, 0, 2, None)
        target_tid = user32.GetWindowThreadProcessId(hwnd, None)
        current_tid = kernel32.GetCurrentThreadId()
        attached = False
        if target_tid and target_tid != current_tid:
            attached = bool(user32.AttachThreadInput(current_tid, target_tid, True))
        try:
            user32.BringWindowToTop(hwnd)
            user32.SetForegroundWindow(hwnd)
        finally:
            if attached:
                user32.AttachThreadInput(current_tid, target_tid, False)
        time.sleep(0.1)
        if user32.GetForegroundWindow() == hwnd:
            return True
    return user32.GetForegroundWindow() == hwnd


def foreground_hwnd() -> int:
    return int(user32.GetForegroundWindow())


def point_is_window(hwnd: int, x: int, y: int) -> bool:
    window = user32.WindowFromPoint(wintypes.POINT(int(x), int(y)))
    if not window:
        return False
    root = user32.GetAncestor(window, GA_ROOT)
    return int(root or window) == int(hwnd)


def _send_mouse(flags: int, x: int | None = None, y: int | None = None) -> None:
    inp = INPUT(type=INPUT_MOUSE)
    inp.mi = MOUSEINPUT(0, 0, 0, flags, 0, None)
    if x is not None and y is not None:
        screen_w = user32.GetSystemMetrics(SM_CXSCREEN)
        screen_h = user32.GetSystemMetrics(SM_CYSCREEN)
        inp.mi.dx = int(round(x * 65535.0 / max(screen_w - 1, 1)))
        inp.mi.dy = int(round(y * 65535.0 / max(screen_h - 1, 1)))
        inp.mi.dwFlags = flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT))


class InputController:
    """前台 SendInput 与后台 PostMessage 两种输入方式。

    内置安全闸（事故后加固，2026-10-01）：任何一次输入前都确认坐标**确实落在目标窗口上**
    （`WindowFromPoint`），否则直接阻止并打日志。事故回顾：一个基准脚本误用真实控制器 + 误选
    "开始页"帧，触发了"识别到界面按钮 → 点击"分支，把左键点进了当时在前台的另一个游戏窗口。
    另外支持环境变量 `JUMPBOT_NO_INPUT=1`：任何工具/测试设了它就只能空转、绝不发真实输入。
    """

    def __init__(self, background: bool = False):
        self.background = background
        self.expected_generation = None

    def press(self, win: WindowInfo, fx: float, fy: float, hold_s: float) -> None:
        guard = (_CONTROL.input_operation(self.expected_generation)
                 if _CONTROL is not None else nullcontext())
        with guard:
            self._press(win, fx, fy, hold_s)

    def _press(self, win: WindowInfo, fx: float, fy: float, hold_s: float) -> None:
        x = int(win.client_rect[0] + fx * win.width)
        y = int(win.client_rect[1] + fy * win.height)
        if self.background:
            if os.environ.get("JUMPBOT_NO_INPUT") == "1":
                log("JUMPBOT_NO_INPUT=1：已阻止本次后台按键")
                return
            cx = int(fx * win.width)
            cy = int(fy * win.height)
            lparam = (cy << 16) | (cx & 0xFFFF)
            user32.PostMessageW(win.hwnd, WM_MOUSEMOVE, 0, lparam)
            user32.PostMessageW(win.hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lparam)
            try:
                time.sleep(hold_s)
            finally:
                # 无论正常结束还是被 Ctrl+C / 异常打断，都必须把按键放开：
                # 卡在按下状态会让游戏一直蓄力，还会把之后所有点击变成拖拽。
                user32.PostMessageW(win.hwnd, WM_LBUTTONUP, 0, lparam)
        else:
            if os.environ.get("JUMPBOT_NO_INPUT") == "1":
                log("JUMPBOT_NO_INPUT=1：已阻止本次按键")
                return
            if not point_is_window(win.hwnd, x, y):
                log(f"安全闸：目标点 ({x},{y}) 不在窗口 {win.title!r} 上，本次输入已阻止")
                if _CONTROL is not None:
                    raise ControlInterrupted("按压点被遮挡（未发送输入）")
                return
            _send_mouse(0, x, y)
            time.sleep(0.012)
            control_check(self.expected_generation)
            if not point_is_window(win.hwnd, x, y):
                raise ControlInterrupted("按压点已被遮挡（未发送按下）")
            _send_mouse(MOUSEEVENTF_LEFTDOWN, x, y)
            try:
                hold_precise(hold_s)
            finally:
                _send_mouse(MOUSEEVENTF_LEFTUP, x, y)

    def click(self, win: WindowInfo, fx: float, fy: float) -> None:
        self.press(win, fx, fy, 0.06)


def hold_precise(hold_s: float, busy_margin_s: float = 0.003) -> None:
    """高精度保持：先 sleep 到接近目标，再忙等收尾，避免 sleep 的时钟粒度误差。"""
    if hold_s <= 0:
        return
    deadline = time.perf_counter() + hold_s
    coarse = hold_s - busy_margin_s
    if coarse > 0:
        time.sleep(coarse)
    while time.perf_counter() < deadline:
        pass


# --------------------------------------------------------------------------------------
# 抓帧层
# --------------------------------------------------------------------------------------


def _mss_factory():
    import mss as mss_module

    return getattr(mss_module, "MSS", None) or mss_module.mss


def estimate_background_rows(frame: np.ndarray, band_ratio: float = 0.03) -> np.ndarray:
    """逐行估计背景颜色（背景是平滑渐变，两侧边缘通常就是背景）。

    取左右两条边带的中位数颜色；只信任两侧一致的行走线性拟合，避免方块压到画面边缘时
    把方块当成背景。
    """
    h, w = frame.shape[:2]
    band = max(3, int(round(band_ratio * w)))
    left = np.median(frame[:, :band, :].astype(np.float32), axis=1)
    right = np.median(frame[:, w - band:, :].astype(np.float32), axis=1)
    agree = np.abs(left - right).sum(axis=1) < 36
    estimate = 0.5 * (left + right)
    ys = np.arange(h, dtype=np.float32)
    model = np.empty((h, 3), dtype=np.float32)
    for channel in range(3):
        if int(agree.sum()) >= 20:
            slope, intercept = np.polyfit(ys[agree], estimate[agree, channel], 1)
            model[:, channel] = slope * ys + intercept
        else:
            model[:, channel] = float(np.median(estimate[:, channel]))
    return np.clip(model, 0, 255)


class Capturer:
    """screen: mss 截屏（要求窗口可见）；printwindow: Win32 PrintWindow（可截被遮挡窗口）。"""

    def __init__(self, mode: str = "screen"):
        self.mode = mode
        self._sct = None
        if mode == "screen":
            self._sct = _mss_factory()()

    def close(self) -> None:
        if self._sct is not None:
            try:
                self._sct.close()
            except Exception:
                pass
            self._sct = None

    def grab(self, win: WindowInfo) -> np.ndarray:
        l, t, r, b = win.client_rect
        w, h = r - l, b - t
        if self.mode == "printwindow":
            return self._grab_printwindow(win.hwnd, w, h)
        shot = self._sct.grab({"left": l, "top": t, "width": w, "height": h})
        frame = np.asarray(shot, dtype=np.uint8)
        return np.ascontiguousarray(frame[:, :, :3])

    @staticmethod
    def _grab_printwindow(hwnd: int, w: int, h: int) -> np.ndarray:
        window_dc = user32.GetWindowDC(hwnd)
        if not window_dc:
            raise RuntimeError("GetWindowDC 失败")
        mem_dc = gdi32.CreateCompatibleDC(window_dc)
        bitmap = gdi32.CreateCompatibleBitmap(window_dc, w, h)
        old = gdi32.SelectObject(mem_dc, bitmap)
        try:
            user32.PrintWindow(hwnd, mem_dc, 2)  # PW_RENDERFULLCONTENT

            class BITMAPINFOHEADER(ctypes.Structure):
                _fields_ = [
                    ("biSize", wintypes.DWORD),
                    ("biWidth", wintypes.LONG),
                    ("biHeight", wintypes.LONG),
                    ("biPlanes", wintypes.WORD),
                    ("biBitCount", wintypes.WORD),
                    ("biCompression", wintypes.DWORD),
                    ("biSizeImage", wintypes.DWORD),
                    ("biXPelsPerMeter", wintypes.LONG),
                    ("biYPelsPerMeter", wintypes.LONG),
                    ("biClrUsed", wintypes.DWORD),
                    ("biClrImportant", wintypes.DWORD),
                ]

            class BITMAPINFO(ctypes.Structure):
                _fields_ = [
                    ("bmiHeader", BITMAPINFOHEADER),
                    ("bmiColors", wintypes.DWORD * 3),
                ]

            bmi = BITMAPINFO()
            bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
            bmi.bmiHeader.biWidth = w
            bmi.bmiHeader.biHeight = -h
            bmi.bmiHeader.biPlanes = 1
            bmi.bmiHeader.biBitCount = 32
            bmi.bmiHeader.biCompression = 0
            buf = ctypes.create_string_buffer(w * h * 4)
            gdi32.GetDIBits(mem_dc, bitmap, 0, h, buf, ctypes.byref(bmi), 0)
            frame = np.frombuffer(buf, dtype=np.uint8).reshape((h, w, 4))
            return np.ascontiguousarray(frame[:, :, :3])
        finally:
            gdi32.SelectObject(mem_dc, old)
            gdi32.DeleteObject(bitmap)
            gdi32.DeleteDC(mem_dc)
            user32.ReleaseDC(hwnd, window_dc)


# --------------------------------------------------------------------------------------
# 配置
# --------------------------------------------------------------------------------------


@dataclass
class Config:
    window_title_pattern: str = "^跳一跳$"
    window_exe_hint: str = "WeChatAppEx"
    k: float = 2.61                 # ms / 物理像素（开环标定的零点：k=2.43 时均 −31px、2.70 时均 +16px）
    k0: float = 2.61                # k 限幅基准，运行时等于初始 k
    b: float = 0.0                  # ms，可选截距
    gamma: float = 0.3              # 自适应增益
    max_k_ratio: float = 1.06       # 收紧限幅：避免控制器漂进“系统性跳短”区间
    min_k_ratio: float = 0.94
    max_step_ratio: float = 0.05    # 单次调整幅度上限
    press_duration_min_ms: float = 60.0
    press_duration_max_ms: float = 1300.0
    press_point: list = field(default_factory=lambda: [0.5, 0.72])
    press_bias_ratio: float = 0.0   # 按压时长整体偏置（+0.02 = 一律多按 2%，觉得跳短就调大）
    axis_slope: float = 0.5774      # 等距投影下两个跳跃轴向的斜率 |dy/dx|
    # 目标向量偏离轴向的最大横向比例。棋子站在便利店等“宽方块”偏前位置时，
    # 到目标中心的向量会明显偏离等距轴向（实测 0.35 附近），但目标块本身是对的
    # （跳跃只能修正沿轴分量，横向偏差无法修正也不需要修正）。
    axis_max_cross_ratio: float = 0.45
    target_consistency_px: float = 8.0   # 连续两帧目标中心允许的差异
    detect_retry_limit: int = 40         # 识别失败时最多重试次数（期间不发按键）
    transfer_a: float = 0.0         # 标定结果：跳跃距离(px) = a * 按压时长(ms) + c
    transfer_c: float = 0.0         # transfer_a>0 时启用该模型，否则回退 k*d+b
    residual_trim_ratio: float = 0.03    # 在线残差修正的限幅（±3%）
    window_wait_s: float = 120.0    # 目标窗口消失后等待它回来的最长时间
    pause_hotkey: str = "Alt+P"
    startup_wait_s: float = 0.0     # 首次启动等待用户打开小程序；0 = 无限等待
    start_delay_s: float = 3.0
    blind_jump_limit: int = 3       # 连续识别失败时，允许“盲跳”推进的次数上限
    modal_dark_threshold: float = 110.0   # 中央区域平均亮度低于此值 = 深色弹窗
    modal_dismiss_point: list = field(default_factory=lambda: [0.127, 0.914])  # 弹窗返回键
    top_bar_ratio: float = 0.06          # 微信小游戏顶栏高度占比
    bottom_bar_ratio: float = 0.0
    # ---- 窗口尺寸守卫 ----
    # 识别里所有**绝对像素**阈值（中心白点面积 14~260、最小方块宽 120px、落点安定容差 3px…）
    # 都按下面这个基准尺寸标定。客户区尺寸一变，"开始游戏"按钮的模板匹配就会从 0.95
    # 掉到 0.79 之类，表现为"点不进游戏 / 识别不了"（实测 2026-10-01：532x1016 时）。
    #   auto = 自动调回基准尺寸（默认）；warn = 只警告不改；off = 不检查
    window_ref_width: int = 553
    window_ref_height: int = 1050
    window_size_policy: str = "auto"
    window_size_tol_px: int = 1
    piece_hue_min: int = 100             # 棋子是深蓝紫色（默认皮肤）
    piece_hue_max: int = 155
    piece_sat_min: int = 55              # 棋子：饱和度高（排除灰白方块与背景）
    piece_val_max: int = 160             # 棋子：亮度低（背景很亮）
    block_dark_delta: int = 30           # 比背景暗这么多 = 方块
    block_hue_delta: int = 15            # 色相偏离背景这么多 = 彩色方块
    block_hue_sat_min: int = 50
    block_sat_delta: int = 45            # 饱和度比背景高这么多 = 鲜艳方块（应对背景也是暖色相时）
    # 与背景的 BGR 总绝对差超过该值 = 非背景。取 45 是为了排除“方块投在背景上的阴影”
    # （阴影会保留背景的色相/饱和度，只是整体变暗，色距常到 30~40）。
    block_color_delta: int = 45
    # 去饱和判据：背景是有色彩的淡色（S≈40），浅灰屋顶/S≈0 的方块与它饱和度差很大，
    # 而阴影几乎不改变饱和度。用这一条专门抓“浅灰方块 on 有色背景”。
    # 去饱和判据阈值。实测“浅灰屋顶 on 淡绿背景”饱和度只差 20~21，
    # 而背景自身差 0、半透明光圈差 ~6、阴影差 ~5，所以 15 能干净分开。
    block_pale_delta: int = 15
    block_white_sat_max: int = 20        # 近纯白且高亮 = 白色方块
    block_white_val_min: int = 245
    # 比背景更亮、同时饱和度不高于背景 = 浅色方块（0 = 关闭该判据）。
    # 实测“淡绿背景上的便利店白屋顶”只比背景亮 7~8 灰阶、饱和度还更低
    # （屋顶 S=0 / 背景 S=12），暗/色相/饱和/纯白/色距五条判据全都抓不到它，
    # 导致连通域只剩店身+影子，顶角取到影子的尖，目标点算短 → 跳短摔死。
    block_bright_delta: int = 0
    # 顶面估计宽度 < 该比例 × 连通域宽度 → 判为“退化估计”（典型来源：影子尖端、
    # 被裁切的碎片）。退化候选只在最后兜底时才允许使用，且优先级低于正常候选。
    block_degenerate_top_ratio: float = 0.10
    # 中心标记（游戏自带的白色小圆点）检测。0 = 关闭。
    # 实测：唱片机转盘中心、快递箱、便利店屋顶都会画一个亮白小点，它就是游戏的
    # 中心判定点；而这些"抬高台面"的顶面中心估计会偏 20~40px（2026-10-01 实测
    # 唱片机：估计 (445,427) vs 白点 (443,466)）。开启后用它直接当目标点。
    center_dot_max_offset_px: float = 0.0
    center_dot_search_radius_px: int = 90
    # 只在"顶面估计明显不可信"的大方块上启用中心标记：
    #   top_width < center_dot_ratio_gate × 连通域宽度  且  连通域宽度 ≥ center_dot_min_block_width
    # 这样能覆盖唱片机这类"抬高台面"（实测 48px 偏差），又不会在小方块（咖啡杯等）
    # 上把杯壁高光误当中心点（实测那会把瞄点拉偏 67px）。
    center_dot_ratio_gate: float = 0.20
    center_dot_min_block_width: int = 200
    center_dot_val_min: int = 238
    center_dot_sat_max: int = 14
    center_dot_min_area: int = 14
    center_dot_max_area: int = 260
    center_dot_bright_delta: int = 12
    # 落地帧里找中心白点的搜索半径：从"落地方块估计中心"往下量（估计值可能偏 20~25px）
    center_dot_landing_radius_px: float = 70.0
    # 极端退化候选（顶宽/连通域宽 < center_dot_degenerate_ratio）上把白点搜索半径放大到
    # 这个值。这类候选只占 0.18%，但顶面估计会偏 50~103px，默认半径必然漏掉白点；
    # 放大只在该类生效，正常跳数完全不受影响（见 find_target 里的现场证据注释）。
    center_dot_degenerate_ratio: float = 0.12
    center_dot_degenerate_radius_px: float = 120.0
    # 瞄点中心算法：estimate=老的"边缘拟合/等宽平台期"结果；
    # geometric=顶角y + 半顶宽*slope（等距投影下菱形/椭圆顶面的几何中心）。
    # 默认仍是 estimate：geometric 在"有白点"的帧上把瞄点误差从 10.20px 降到 6.83px，
    # 但生产代码在这些帧上本来就用白点描准（它真正生效的是没有白点的帧，而那批帧没有
    # 真值可量）；1000 vs 1000 的同码 A/B 里两者聚合命中率无显著差异（96.88% vs 97.30%，
    # p=0.583，二手指标 |err|<12 也略偏 estimate）。详见 README「瞄点几何中心/A-B」。
    aim_center_mode: str = "estimate"
    #  “粘连块”补偿实验开关（默认 0 = 关闭）。
    # 实测 2026-10-01 第 4 轮 3980 跳：顶面估计宽度/连通域宽度 < 0.45 的候选
    # （影子/多块粘连，顶面估不准）只有 91.9% 真实命中率，而其它类是 96.8~99.5%；
    # 唱片机等抬高台面上实测瞄点偏"远"11~48px。low_ratio_trim_ratio>0 时，
    # 对这类候选把按压距离按比例缩短，用于 A/B 对照实验。
    low_ratio_gate: float = 0.45
    low_ratio_trim_ratio: float = 0.0
    piece_min_area_ratio: float = 0.0006
    piece_max_area_ratio: float = 0.03
    piece_aspect_min: float = 1.5        # 棋子高宽比下限（方块/图标是扁的或圆的）
    piece_min_width_ratio: float = 0.03  # 棋子最小宽度（占画面宽）
    piece_min_y_ratio: float = 0.50      # 棋子落脚点必须位于画面下半部（挡掉开局动画/HUD 误检）
    piece_template_min_score: float = 0.45   # 与 templates/piece.png 的最低相关度
    piece_prior_radius: float = 0.30
    piece_scan_min_score: float = 0.55     # 模板扫描的最低相关度（用于跨皮肤找棋子）
    # 模板扫描（piece_template_scan，实测 17.6ms/次）只在颜色路径**找不到棋子**时才跑。
    # 颜色路径是主路径（跨皮肤也有多模板兜底），这一步原本每帧无条件跑；实测它在
    # 绝大多帧里只是"再确认一次"，把 detect() 的 46.8ms 撑掉近 40%。默认关，
    # 用 tools/ab_vision 在历史帧上验证过再开。
    piece_scan_only_when_needed: bool = False
    # 掩码计算走 cv2 的 uint8(SIMD) 实现：与 numpy 版**逐像素等价**（已用真实帧验证），
    # 但 masks() 从 ~17.5ms 降到 ~7ms。默认开；出问题可置 false 回退旧实现。
    masks_simd: bool = True
    # 掩码后端：cpu(numpy 旧实现) / simd(cv2 uint8，等价但更快) / gpu(CuPy on CUDA)。
    # gpu 需要 nvidia-*-cu12 pip 组件或本地 CUDA Toolkit；不可用时自动回退 simd。
    # 实测（553x1050 帧，RTX 3070 Ti）：cpu 18.1ms / simd 11.5ms / gpu 2.1~4.3ms（含传输）。
    # auto：启动时实测 gpu/simd 各 5 次，GPU 至少快 1.5 倍才用——实测当别的程序
    # 吃满 GPU（例如在全屏游戏里）时 CUDA 内核会排队，masks 从 3.4ms 掉到 188ms。
    masks_backend: str = "auto"
    cuda_enabled: bool = True
    opencl_enabled: bool = True  # 可选 pyopencl；AMD/Intel/NVIDIA 的 OpenCL 1.2+ 驱动
    opencl_device: str = ""      # 留空实测所有 GPU；也可按名称/厂商子串限定设备
    cv2_threads: int = 4          # 0=按逻辑 CPU 数自动选择 1~4；可手动指定
    piece_anchor_dy_ratio: float = 0.019  # 棋子锚点相对包围盒底边的下移量（占画面高）
    landing_color_tol: int = 100          # 顶面颜色相似度容差（L1，用于滤掉棋子阴影）
    landing_method: str = "auto"          # auto=优先用顶面色过滤阴影，失败回退方块掩码
    flow_min_response: float = 0.2        # 相位相关可信度下限
    block_min_area_ratio: float = 0.008
    block_max_area_ratio: float = 0.35
    block_min_width_ratio: float = 0.15
    block_min_height_ratio: float = 0.08
    target_min_up_ratio: float = 0.03    # 目标方块中心必须高于棋子锚点（占画面高）
    target_exclude_radius_ratio: float = 0.2  # 锚点这么近的方块判定为“脚下那块”
    # 细长竖直的小物件（例如跳板旗杆）救援：当选中的目标连通域高宽比超过该值，
    # 且同轴上存在面积 ≥3 倍、达到严格尺寸门限的大方块时，改选那个大方块。
    # 实测（2026-10-01 03:14 死亡）：旗杆 50x115 在 152px 处被选中，真正的大方块在
    # 443.7px（超过旧上限 442px 被丢弃）→ 跳短摔死。0 = 关闭该救援。
    item_rescue_min_aspect: float = 1.8
    min_jump_dist_ratio: float = 0.14     # 合理跳跃距离下限（占画面宽）；实测存在 86~104px 的贴邻方块
    max_jump_dist_ratio: float = 0.9      # 合理跳跃距离上限（实测存在 443.7px 的真实方块）
    settle_diff_threshold: float = 2.2
    settle_anchor_tol_px: float = 3.0
    settle_frames: int = 3
    settle_timeout_s: float = 2.6
    # 轮询节奏（只影响"多久看一眼"，不改变任何判据）：
    # 实测抓帧 8.8ms、detect 12.3ms，所以轮询间隔几乎是纯延迟。
    # 落地等待 50ms→30ms：settle 需连续 3 帧稳定，确认提前 ~60ms；
    # 抓帧轮询 60ms→40ms：起跳前"连续两帧一致"的确认提前 ~40ms。
    settle_poll_ms: float = 30.0
    acquire_poll_ms: float = 40.0
    score_reread_first_ms: float = 100.0   # HUD 未刷新时的第一次重读间隔（原 250ms）
    restart_point: list = field(default_factory=lambda: [0.621, 0.761])
    restart_wait_s: float = 1.2
    # 分数区域要留够宽度：实测 5 位数（如 10209）会延伸到 x≈0.47w，
    # 早期 0.46w 的右边界把最后一位切掉，导致读到恒定值、误触发卡死保护。
    score_roi: list = field(default_factory=lambda: [0.08, 0.13, 0.80, 0.24])
    center_hit_tolerance_px: float = 12.0
    score_bonus_cap: int = 32            # 连击加成上限：min(2×连击数, 32)
    digit_min_height_ratio: float = 0.02
    max_jumps: int = 100
    max_runtime_s: float = 1800.0

    @classmethod
    def load(cls, path: Path, overrides: dict | None = None) -> "Config":
        data = {}
        if path.exists():
            data = json.loads(path.read_text(encoding="utf-8"))
        cfg = cls()
        for key, value in data.items():
            if hasattr(cfg, key):
                setattr(cfg, key, value)
            else:
                log(f"警告：配置项 {key} 无法识别，已忽略")
        for key, value in (overrides or {}).items():
            if value is not None and hasattr(cfg, key):
                setattr(cfg, key, value)
        env_backend = os.environ.get("JUMPBOT_MASKS_BACKEND")
        if env_backend:
            cfg.masks_backend = env_backend
        env_device = os.environ.get("JUMPBOT_OPENCL_DEVICE")
        if env_device:
            cfg.opencl_device = env_device
        if cfg.masks_backend not in ("auto", "cpu", "simd", "gpu", "cuda", "opencl"):
            raise ValueError(f"未知掩码后端：{cfg.masks_backend}")
        parse_hotkey(cfg.pause_hotkey)
        cfg.startup_wait_s = validate_timing(cfg.startup_wait_s, "startup_wait_s")
        cfg.start_delay_s = validate_timing(cfg.start_delay_s, "start_delay_s")
        return cfg

    def save(self, path: Path) -> None:
        path.write_text(
            json.dumps(asdict(self), ensure_ascii=False, indent=2), encoding="utf-8"
        )


# --------------------------------------------------------------------------------------
# 识别层（先给占位，下一步按真实截图调参实现）
# --------------------------------------------------------------------------------------


@dataclass
class Piece:
    """棋子。x/y 为落脚锚点（包围盒底边中点）。"""

    x: float
    y: float
    bbox: tuple
    area: int
    tpl_score: float = 0.0      # 与棋子模板的归一化相关（越高越像棋子）


_PIECE_TEMPLATES: list | None = None


def load_piece_templates() -> list:
    """棋子模板（templates/piece*.png，可放多套皮肤）。

    游戏在分数达到 3000 时会解锁新皮肤，棋子外观会变；用“多模板 + 模板扫描”
    才能跨皮肤识别，而不是依赖固定色相带。
    """
    global _PIECE_TEMPLATES
    if _PIECE_TEMPLATES is None:
        _PIECE_TEMPLATES = []
        if TEMPLATE_DIR.exists():
            for path in sorted(TEMPLATE_DIR.glob("piece*.png")):
                img = cv2.imread(str(path))
                if img is not None:
                    _PIECE_TEMPLATES.append(img)
    return _PIECE_TEMPLATES


def load_piece_template() -> np.ndarray | None:
    """兼容旧调用：返回第一个棋子模板。"""
    tpls = load_piece_templates()
    return tpls[0] if tpls else None


def piece_template_score(frame: np.ndarray, bbox: tuple) -> float:
    """把候选区域与所有棋子模板比对，返回最高相关度。"""
    tpls = load_piece_templates()
    if not tpls:
        return 0.0
    x, y, bw, bh = bbox
    h, w = frame.shape[:2]
    x0, y0 = max(0, x), max(0, y)
    x1, y1 = min(w, x + bw), min(h, y + bh)
    if x1 - x0 < 6 or y1 - y0 < 6:
        return 0.0
    best = 0.0
    for tpl in tpls:
        crop = cv2.resize(frame[y0:y1, x0:x1], (tpl.shape[1], tpl.shape[0]))
        a = crop.astype(np.float32).ravel()
        b = tpl.astype(np.float32).ravel()
        a = a - a.mean()
        b = b - b.mean()
        denom = float(np.linalg.norm(a) * np.linalg.norm(b)) + 1e-6
        best = max(best, float((a * b).sum() / denom))
    return best


def piece_template_scan(frame: np.ndarray, cfg: Config) -> list:
    """在画面下半部做多模板扫描，返回模板匹配到的棋子候选。

    这条路径不依赖颜色（色相带只覆盖默认紫色皮肤），换皮肤后仍能找到棋子。
    """
    tpls = load_piece_templates()
    if not tpls:
        return []
    h, w = frame.shape[:2]
    top = int(0.30 * h)
    search = frame[top:, :]
    found = []
    for tpl in tpls:
        th, tw = tpl.shape[:2]
        if th >= search.shape[0] or tw >= search.shape[1]:
            continue
        res = cv2.matchTemplate(search, tpl, cv2.TM_CCOEFF_NORMED)
        _mn, max_val, _ml, max_loc = cv2.minMaxLoc(res)
        if max_val < cfg.piece_scan_min_score:
            continue
        x = int(max_loc[0])
        y = int(max_loc[1]) + top
        # 与颜色路径保持一致：锚点取棋子底边中点
        anchor_y = y + th - 1
        if anchor_y < cfg.piece_min_y_ratio * h:
            continue
        found.append(Piece(float(x + tw / 2.0), float(anchor_y), (x, y, tw, th),
                           int(tw * th), tpl_score=float(max_val)))
    return found


@dataclass
class Block:
    """方块。x/y 为顶面中心，bbox 为整块立方体的包围盒。"""

    x: float
    y: float
    bbox: tuple
    area: int
    top_y: float = 0.0        # 顶面后角（最高点）的 y
    top_width: float = 0.0    # 顶面最宽处的宽度（用于菱形内点判定）
    dot: tuple | None = None  # 若用"游戏自带的中心白点"校正过瞄点，这里存 (cx, cy, area)


def top_face_center(comp_mask: np.ndarray, slope: float = 0.5774) -> tuple | None:
    """由方块连通域求顶面（菱形）中心。

    顶面后角是该连通域最上面一行的中点。等距投影下顶面的两条上边缘是斜率固定的直线
    （|dx/dy| = 1/slope ≈ 1.732），因此可以沿这两条预测线向下走：只要实际左右边界仍
    贴合预测线，就说明还在菱形上半部；边界一旦脱离预测线，那一行就是菱形最宽处，也就是
    顶面中心行。这比“等宽度平台期”规则精确得多，也不受相邻方块粘连影响。

    圆形台面等没有直边的方块会拟合失败，此时回退原来的平台期规则。
    """
    h, w = comp_mask.shape[:2]
    rows = np.flatnonzero(comp_mask.any(axis=1))
    if rows.size == 0:
        return None
    y_top = int(rows[0])
    top_cols = np.flatnonzero(comp_mask[y_top])
    if top_cols.size == 0:
        return None
    bbox_cols = np.flatnonzero(comp_mask.any(axis=0))
    bbox_width = float(bbox_cols[-1] - bbox_cols[0] + 1)
    # 真方块（菱形或圆台）最上面一行都很窄；矩形 UI（如排行榜卡片、标题栏）会整行铺满。
    # 多个方块粘成一个连通域时，顶行可能出现“几段各自很窄”（几个方块顶角同高），
    # 这时必须逐个当候选顶角处理——取平均会落在两段之间的空隙上，后续拟合全废。
    narrow_limit = max(8.0, 0.30 * bbox_width)
    top_segments = [
        seg for seg in np.split(top_cols, np.where(np.diff(top_cols) > 1)[0] + 1)
        if seg.size <= narrow_limit
    ]
    if not top_segments:
        # 顶行不窄：可能是“平屋顶”类特殊方块（便利店等）。整宽 UI（标题栏/排行榜卡片）
        # 已被最小面积与最小高度挡掉，这里直接走兜底估计。
        return _top_face_center_fallback(comp_mask, float(top_cols.mean()), y_top, bbox_width)
    fitted = []
    for seg in top_segments:
        corner_x = float(seg.mean())
        if corner_x <= 10 or corner_x >= w - 10:
            # 顶角被画面裁掉：不能用它当中心 x，改走兜底（会按未裁的一侧反推）
            return _top_face_center_fallback(comp_mask, corner_x, y_top, bbox_width)
        by_edges = _edge_fit_from_corner(comp_mask, corner_x, y_top, slope, bbox_width)
        if by_edges is not None:
            fitted.append(by_edges)
    if fitted:
        # 顶宽最大的那个最可能是真正的方块顶面（而不是粘连出来的小碎片）
        return max(fitted, key=lambda item: item[2])
    x_back = float(top_segments[0].mean())
    if x_back <= 10 or x_back >= w - 10:
        return _top_face_center_fallback(comp_mask, x_back, y_top, bbox_width)

    # ---- 回退路径：等宽平台期规则 ----
    bbox_height = int(np.flatnonzero(comp_mask.any(axis=1))[-1]) - y_top + 1
    best_width = 0.0
    best_row = y_top
    last_growth_row = y_top
    row_limit = min(h, y_top + max(int(0.45 * bbox_height), 8))
    for row in range(y_top, row_limit):
        if best_width > 0 and row - last_growth_row >= 8:
            # 连续 8 行不再变宽，说明已经到达菱形最宽处
            return x_back, float(best_row), float(best_width), float(y_top)
        cols = np.flatnonzero(comp_mask[row])
        if cols.size == 0:
            continue
        split = np.where(np.diff(cols) > 1)[0]
        for seg in np.split(cols, split + 1):
            if not (seg[0] - 2 <= x_back <= seg[-1] + 2):
                continue
            seg_width = float(seg[-1] - seg[0] + 1)
            left_dist = x_back - float(seg[0])
            right_dist = float(seg[-1]) - x_back
            if abs(left_dist - right_dist) > max(3.0, 0.045 * seg_width):
                # 左右不再对称，说明已开始被相邻方块/阴影污染
                return (
                    (x_back, float(best_row), float(best_width), float(y_top))
                    if best_width > 0 else None
                )
            if seg_width > best_width + 1.5:
                best_width = seg_width
                best_row = row
                last_growth_row = row
            elif seg_width > best_width:
                best_width = seg_width
    if best_width <= 0:
        return _top_face_center_fallback(comp_mask, x_back, y_top, bbox_width)
    if best_width < 0.15 * bbox_width:
        # 顶宽与连通域宽度严重不符（多为多块粘连），再试一次兜底估计
        return _top_face_center_fallback(comp_mask, x_back, y_top, bbox_width)
    return x_back, float(best_row), float(best_width), float(y_top)


def _edge_fit_from_corner(comp_mask: np.ndarray, x_back: float, y_top: int,
                          slope: float, bbox_width: float) -> tuple | None:
    """以给定顶角为起点，按等距投影斜率拟合顶面两条上边缘，求顶面中心。

    返回 (cx, cy, 顶宽, y_top)；拟合不成立（例如圆台、或被污染）时返回 None。
    """
    h, w = comp_mask.shape[:2]
    rows = np.flatnonzero(comp_mask.any(axis=1))
    if rows.size == 0:
        return None
    dx_per_dy = 1.0 / max(slope, 1e-6)
    tol = max(3.0, 0.025 * bbox_width)
    y_bottom = int(rows[-1])
    limit = min(y_bottom, y_top + int(0.6 * bbox_width * dx_per_dy) + 4)
    last_edge_row = None
    misses = 0
    last_growth_row = None
    prev_chosen = None
    for y in range(y_top + 1, limit + 1):
        cols = np.flatnonzero(comp_mask[y])
        if cols.size == 0:
            break
        split = np.where(np.diff(cols) > 1)[0]
        segment = None
        for seg in np.split(cols, split + 1):
            if seg[0] - 2 <= x_back <= seg[-1] + 2:
                segment = seg
                break
        if segment is None:
            break
        span = y - y_top
        predicted_left = x_back - span * dx_per_dy
        predicted_right = x_back + span * dx_per_dy
        if abs(segment[0] - predicted_left) <= tol and abs(segment[-1] - predicted_right) <= tol:
            last_edge_row = y
            misses = 0
            left_d = x_back - float(segment[0])
            right_d = float(segment[-1]) - x_back
            # 优先用右边界判扩张（相邻方块多从左下方粘连）；右边界被画面裁掉时改用左边界
            chosen = left_d if segment[-1] >= w - 2 else right_d
            if prev_chosen is not None and chosen > prev_chosen:
                last_growth_row = y
            prev_chosen = chosen
        else:
            misses += 1
            if misses >= 3:
                break
    if last_edge_row is None or last_edge_row - y_top < 6:
        return None
    vertex_row = last_growth_row if last_growth_row is not None else last_edge_row
    fitted_width = 2.0 * float(vertex_row - y_top) * dx_per_dy
    # 自洽性：真方块顶宽不会远小于它所在连通域的宽度，否则视为拟合提前收敛
    if fitted_width < 0.30 * bbox_width:
        return None
    return x_back, float(vertex_row), fitted_width, float(y_top)


def _top_face_center_fallback(comp_mask: np.ndarray, x_back: float, y_top: int,
                              bbox_width: float) -> tuple | None:
    """三层兜底：用等距投影的固定比例直接估顶面中心。

    菱形“顶点行”距后角 0.2887·W；方块下半部（顶点行以下）的可见宽度就约等于 W，
    所以在 y_top + 0.2887·bbox_width 这一行量一次宽度即可反推中心行。
    多块粘连时这条路径仍常能给出可用的近似值，总比完全放弃好。
    """
    h, w = comp_mask.shape[:2]
    row = int(round(y_top + 0.2887 * bbox_width))
    if row >= h:
        return None
    cols = np.flatnonzero(comp_mask[row])
    if cols.size < 2:
        return None
    segs = np.split(cols, np.where(np.diff(cols) > 1)[0] + 1)
    seg = max(segs, key=len)
    width = float(seg[-1] - seg[0] + 1)
    if width < 20:
        return None
    # 中心 x：正常情况下就是顶角 x；但顶角被画面裁掉时，用未被裁的那一侧反推
    clipped_left = float(seg[0]) <= 1.0
    clipped_right = float(seg[-1]) >= w - 2
    if clipped_left and not clipped_right:
        center_x = float(seg[-1]) - width / 2.0
    elif clipped_right and not clipped_left:
        center_x = float(seg[0]) + width / 2.0
    else:
        center_x = float(x_back)
    return center_x, y_top + 0.2887 * width, width, float(y_top)


def find_dot_near(frame: np.ndarray, cx0: float, cy0: float, radius: float,
                  cfg: Config) -> tuple | None:
    """在 (cx0, cy0) 周围 radius 像素内找游戏自带的中心标记（亮白小圆点）。

    返回 (cx, cy, area) 或 None。只接受：够亮（V≥阈值）、够白（S≤阈值）、
    面积在区间内、近似圆形，且**比紧邻的一圈明显更亮**——最后这条是为了排除
    "整片白墙面/白台阶/浅色背景"这类大块白色（它们整体亮，但不比邻居更亮）。
    """
    h, w = frame.shape[:2]
    radius = float(radius)
    x0 = int(max(0, round(cx0 - radius)))
    x1 = int(min(w, round(cx0 + radius) + 1))
    y0 = int(max(0, round(cy0 - radius)))
    y1 = int(min(h, round(cy0 + radius) + 1))
    if x1 - x0 < 6 or y1 - y0 < 6:
        return None
    roi = frame[y0:y1, x0:x1]
    hsv = cv2.cvtColor(roi, cv2.COLOR_BGR2HSV)
    sat = hsv[:, :, 1].astype(np.int16)
    val = hsv[:, :, 2].astype(np.int16)
    mask = ((val >= cfg.center_dot_val_min) & (sat <= cfg.center_dot_sat_max)).astype(np.uint8)
    if int(mask.sum()) == 0:
        return None
    num, _labels, stats, centroid = cv2.connectedComponentsWithStats(mask, 8)
    best = None
    ring_kernel = np.ones((5, 5), np.uint8)
    for idx in range(1, num):
        area = int(stats[idx, cv2.CC_STAT_AREA])
        if not (cfg.center_dot_min_area <= area <= cfg.center_dot_max_area):
            continue
        bw = int(stats[idx, cv2.CC_STAT_WIDTH])
        bh = int(stats[idx, cv2.CC_STAT_HEIGHT])
        if max(bw, bh) > 2.0 * min(bw, bh):
            continue
        cx = float(centroid[idx][0]) + x0
        cy = float(centroid[idx][1]) + y0
        dist = math.hypot(cx - cx0, cy - cy0)
        if dist > radius:
            continue
        # 该连通域必须**比紧邻它的一圈**明显更亮——用局部环带做参考，
        # 这样"整片浅色背景/白墙面"不会因为整体亮而被误当成中心点。
        comp = _labels == idx
        grown = cv2.dilate(comp.astype(np.uint8), ring_kernel, iterations=1).astype(bool)
        ring = grown & ~comp
        if not ring.any():
            continue
        ref_local = float(np.median(val[ring]))
        if float(val[comp].mean()) < ref_local + cfg.center_dot_bright_delta:
            continue
        if best is None or dist < best[0]:
            best = (dist, cx, cy, area)
    if best is None:
        return None
    return best[1], best[2], best[3]


def find_center_dot(frame: np.ndarray, block: "Block", cfg: Config) -> tuple | None:
    """目标方块的中心标记：在估计中心附近按 `center_dot_max_offset_px` 半径搜索。"""
    if cfg.center_dot_max_offset_px <= 0:
        return None
    return find_dot_near(frame, block.x, block.y, cfg.center_dot_max_offset_px, cfg)


class Vision:
    """基于颜色/连通域的识别：

    - 背景：高饱和淡色渐变（S 中等、V 很高）
    - 方块：纯灰（S 很低、V 中等）
    - 棋子：暗且高饱和（S 高、V 低）
    """

    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.piece: Piece | None = None
        self._gpu_state: bool | None = None      # None=未尝试, True=可用, False=不可用
        self._cp = None
        self._gpu_kernel = None
        self._gpu_hsv_tables = None
        self._gpu_buffers = None                # 只保留当前帧尺寸的设备缓冲区
        self._cl_state: bool | None = None
        self._cl = None
        self._cl_candidates = []
        self._cl_runtime = None
        self._cl_buffers: dict = {}
        self._last_mask_backend = None
        self._auto_backend: str | None = None    # masks_backend="auto" 时的实测结果

    def reset_prior(self) -> None:
        """丢弃上一帧的棋子位置先验（换场景/跳跃后必须清，否则会锁死在旧位置）。"""
        self.piece = None

    # ---------- 掩码 ----------

    def _pick_backend(self, frame: np.ndarray) -> str:
        """比较 CPU/CUDA/OpenCL 的真实端到端耗时，并核对设备输出后再启用。"""
        if self._auto_backend is not None:
            return self._auto_backend
        cpu = "simd" if self.cfg.masks_simd else "cpu"
        expected = self._masks_impl(cpu, frame)
        funcs = [(cpu, lambda f: self._masks_impl(cpu, f)),
                 ("gpu", self._masks_gpu), ("opencl", self._masks_opencl)]
        cand = []
        for name, fn in funcs:
            candidate_masks = fn(frame)                 # 初始化/编译/预热不计时
            if candidate_masks is None:
                continue
            if any(not np.array_equal(a, b) for a, b in zip(expected, candidate_masks)):
                log(f"{name} 掩码核对失败，本次禁用该后端")
                if name == "gpu":
                    self._gpu_state = False
                elif name == "opencl":
                    self._cl_state = False
                continue
            samples = []
            for _ in range(5):
                t0 = time.perf_counter()
                if fn(frame) is None:
                    break
                samples.append((time.perf_counter() - t0) * 1000)
            if len(samples) == 5:
                cand.append((name, sorted(samples)[2]))
        simd_ms = next(ms for name, ms in cand if name == cpu)
        self._simd_ref_ms = simd_ms
        accelerators = [(n, ms) for n, ms in cand if n != cpu and ms * 1.5 <= simd_ms]
        self._auto_backend = min(accelerators, key=lambda x: x[1])[0] if accelerators else cpu
        self._gpu_ms_buf = []
        detail = "、".join(f"{n} {ms:.2f}ms" for n, ms in cand) or "都不可用"
        log(f"掩码后端自动选择：{self._auto_backend}（实测 {detail}）")
        status_write(mask_backend=self._auto_backend, mask_bench_ms=detail)
        return self._auto_backend

    def _note_backend_timing(self, backend: str, ms: float) -> None:
        """auto 模式下监测所选后端：GPU 被别的程序抢走时（实测可慢 20 倍）切回 SIMD。"""
        if backend not in ("gpu", "opencl") or self._auto_backend != backend:
            return
        buf = getattr(self, "_gpu_ms_buf", None)
        if buf is None:
            buf = self._gpu_ms_buf = []
        buf.append(ms)
        if len(buf) > 20:
            del buf[:-20]
        if len(buf) >= 10:
            med = sorted(buf)[len(buf) // 2]
            ref = getattr(self, "_simd_ref_ms", None)
            limit = max(2.0 * ref, 30.0) if ref else 30.0
            if med > limit:
                log(f"{backend} 掩码变慢（滚动中位 {med:.0f}ms > {limit:.0f}ms，"
                    f"可能被其他程序占用），切换回 SIMD")
                self._auto_backend = "simd" if self.cfg.masks_simd else "cpu"
                self._gpu_ms_buf = []
                status_write(mask_backend=self._auto_backend,
                             mask_bench_ms=f"{backend} 退化到 {med:.0f}ms，已切回 CPU")

    def _init_gpu(self) -> bool:
        """按需初始化 CuPy（含 CUDA 运行时路径自动发现）。失败返回 False 并永久回退。"""
        if not self.cfg.cuda_enabled or os.environ.get("JUMPBOT_DISABLE_CUDA") == "1":
            self._gpu_state = False
            return False
        if self._gpu_state is not None:
            return self._gpu_state
        self._gpu_state = False
        try:
            # NVIDIA pip 组件把 DLL 放在 site-packages/nvidia/*/bin，
            # 且不会自动进 PATH/CUDA_PATH；这里替 CuPy 铺好路再 import。
            # 注意：**每个进程只铺一次**。早期版本每次 new Vision 都追加一遍 PATH，
            # 几百帧之后 PATH 超过 32767 字符上限 → 初始化报错并静默回退 CPU。
            global _CUDA_ENV_READY
            if not _CUDA_ENV_READY:
                import site
                roots = []
                # 1) 真正的 CUDA Toolkit 优先：CuPy 在缺 CUDA 时会派生
                #    `python -m cuda.pathfinder...` 子进程探测，而打包环境下这条路会挂；
                #    把 CUDA_PATH 指到完整 Toolkit 就能跳过探测（实测唯一可靠的方式）。
                env_root = os.environ.get("CUDA_PATH")
                if env_root:
                    roots.append(Path(env_root))
                toolkit_base = Path(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA")
                if toolkit_base.exists():
                    roots += sorted((p for p in toolkit_base.glob("v*") if p.is_dir()),
                                    reverse=True)
                # 2) pip 的 nvidia 组件（只有运行时 DLL，没有完整 Toolkit 时的兜底）
                roots += [Path(sp) / "nvidia"
                          for sp in list(site.getsitepackages()) + [site.getusersitepackages()]]
                # 3) 打包后资源目录
                roots += [DATA_DIR / "nvidia", PROJECT_DIR / "nvidia"]
                for root in roots:
                    if not root.exists():
                        continue
                    if (root / "bin").exists():          # 完整 Toolkit 布局
                        bins = [str(root / "bin")]
                    else:                                 # nvidia/<component>/bin 布局
                        bins = [str(p) for p in root.glob("*/bin") if p.exists()]
                    if bins:
                        cur = os.environ.get("PATH", "")
                        add = [b for b in bins if b not in cur]
                        if add:
                            os.environ["PATH"] = os.pathsep.join(add + [cur])
                        if (root / "bin").exists():
                            os.environ["CUDA_PATH"] = str(root)   # Toolkit 优先，覆盖旧值
                        else:
                            os.environ.setdefault("CUDA_PATH", str(root))
                        break
                _CUDA_ENV_READY = True
            import cupy  # noqa: F401  （导入即验证驱动/运行时可用）
            cupy.arange(1)          # 触发一次真实初始化，失败会在这里抛
            self._cp = cupy
            self._gpu_state = True
            log(f"GPU 掩码后端已启用（CuPy {cupy.__version__}，设备 "
                f"{cupy.cuda.runtime.getDeviceProperties(0)['name'].decode()}）")
        except Exception as exc:
            log(f"GPU 掩码后端不可用，回退 CPU：{exc}")
        return self._gpu_state

    def _masks_gpu(self, frame: np.ndarray):
        """单个融合 CUDA 内核：HSV 转换沿用 OpenCV 的整数查表/舍入规则。

        只上传 BGR 帧和背景行，设备缓冲区按帧尺寸复用，两个掩码一次阻塞下载。
        返回的是本帧新分配的 CPU 数组，下一帧不会覆盖调用者保留的结果。
        """
        if not self._init_gpu():
            return None
        cfg = self.cfg
        cp = self._cp
        try:
            h, w = frame.shape[:2]
            bg = estimate_background_rows(frame)
            bg_u8 = np.clip(bg, 0, 255).astype(np.uint8).reshape(-1, 1, 3)
            if self._gpu_kernel is None:
                # OpenCV RGB2HSV_b: hsv_shift=12，查表项使用 cvRound（ties-to-even）。
                # 保留整数 HSV 语义，避免浮点转换在阈值边缘改变识别结果。
                divisors = np.arange(1, 256, dtype=np.float64)
                tables = np.zeros((2, 256), np.int32)
                tables[0, 1:] = np.rint((255 << 12) / divisors).astype(np.int32)
                tables[1, 1:] = np.rint((180 << 12) / (6.0 * divisors)).astype(np.int32)
                self._gpu_hsv_tables = cp.asarray(tables)
                self._gpu_kernel = cp.RawKernel(r'''
                // Adapted from OpenCV RGB2HSV_b (Apache-2.0); see THIRD_PARTY_NOTICES.md.
                __device__ __forceinline__ void jumpbot_bgr_hsv(
                    const unsigned char* bgr, const int* tables,
                    int& hue, int& sat, int& val)
                {
                    int blue = bgr[0], green = bgr[1], red = bgr[2];
                    val = max(blue, max(green, red));
                    int delta = val - min(blue, min(green, red));
                    sat = (delta * tables[val] + 2048) >> 12;
                    hue = val == red ? green - blue
                        : val == green ? blue - red + 2 * delta
                        : red - green + 4 * delta;
                    hue = (hue * tables[256 + delta] + 2048) >> 12;
                    if (hue < 0) hue += 180;
                }
                extern "C" __global__ void jumpbot_masks(
                    const unsigned char* bgr, const unsigned char* bg,
                    const int* tables, unsigned char* out,
                    int n, int width, int top, int bottom,
                    double color_delta, double dark_delta, double hue_delta,
                    double hue_sat_min, double sat_delta, double pale_delta,
                    double white_sat_max, double white_val_min, double bright_delta,
                    double piece_sat_min, double piece_val_max,
                    double piece_hue_min, double piece_hue_max)
                {
                    int i = blockIdx.x * blockDim.x + threadIdx.x;
                    if (i >= n) return;
                    int row = i / width;
                    if (row < top || row >= bottom) {
                        out[i] = out[n + i] = 0;
                        return;
                    }
                    int p = 3 * i, b = 3 * row;
                    int hue, sat, val, bh, bs, bv;
                    jumpbot_bgr_hsv(bgr + p, tables, hue, sat, val);
                    jumpbot_bgr_hsv(bg + b, tables, bh, bs, bv);
                    // 先提升为有符号整数，再相减；uint8 差/三通道和不会回绕。
                    int db = (int)bgr[p] - bg[b];
                    int dg = (int)bgr[p + 1] - bg[b + 1];
                    int dr = (int)bgr[p + 2] - bg[b + 2];
                    int dh = hue - bh;
                    int diff = (db < 0 ? -db : db) + (dg < 0 ? -dg : dg)
                             + (dr < 0 ? -dr : dr);
                    bool block = val < bv - dark_delta
                        || ((dh < 0 ? -dh : dh) > hue_delta && sat > hue_sat_min)
                        || sat - bs > sat_delta || bs - sat > pale_delta
                        || (sat < white_sat_max && val > white_val_min)
                        || diff > color_delta
                        || (bright_delta > 0 && val - bv > bright_delta && sat <= bs);
                    bool piece = ((sat > piece_sat_min && val < piece_val_max)
                                  || (val < 110 && sat > 25))
                                 && hue >= piece_hue_min && hue <= piece_hue_max;
                    out[i] = piece ? 255 : 0;
                    out[n + i] = block ? 255 : 0;
                }
                ''', "jumpbot_masks", options=("--std=c++11",))
            if self._gpu_buffers is None or self._gpu_buffers[0] != (h, w):
                self._gpu_buffers = ((h, w), cp.empty((h, w, 3), cp.uint8),
                                     cp.empty((h, 3), cp.uint8),
                                     cp.empty((2, h, w), cp.uint8))
            _shape, g_bgr, g_bg, g_masks = self._gpu_buffers
            g_bgr.set(np.ascontiguousarray(frame))
            g_bg.set(bg_u8.reshape(h, 3))
            # 与 _canvas_mask 的 Python 切片语义一致，含负数/越界比率。
            top = int(round(cfg.top_bar_ratio * h))
            bottom = int(round((1.0 - cfg.bottom_bar_ratio) * h))
            top, bottom, _step = slice(top, bottom).indices(h)
            thresholds = tuple(np.float64(x) for x in (
                cfg.block_color_delta, cfg.block_dark_delta, cfg.block_hue_delta,
                cfg.block_hue_sat_min, cfg.block_sat_delta, cfg.block_pale_delta,
                cfg.block_white_sat_max, cfg.block_white_val_min, cfg.block_bright_delta,
                cfg.piece_sat_min, cfg.piece_val_max, cfg.piece_hue_min, cfg.piece_hue_max))
            n = h * w
            self._gpu_kernel(((n + 255) // 256,), (256,),
                             (g_bgr, g_bg, self._gpu_hsv_tables, g_masks, np.int32(n), np.int32(w),
                              np.int32(top), np.int32(bottom), *thresholds))
            masks = g_masks.get()                # 当前流阻塞下载；无需额外全局同步
            return masks[0], masks[1]
        except Exception as exc:
            log(f"GPU 掩码计算出错，回退 CPU：{exc}")
            self._gpu_state = False
            self._gpu_buffers = None
            return None

    def _init_opencl(self) -> bool:
        """可选 OpenCL 1.2 后端；无库/驱动/合格设备时保持 CPU 可用。"""
        if not self.cfg.opencl_enabled or os.environ.get("JUMPBOT_DISABLE_OPENCL") == "1":
            self._cl_state = False
            return False
        if self._cl_state is not None:
            return self._cl_state
        self._cl_state = False
        try:
            import pyopencl as cl
            self._cl = cl
            devices = []
            for platform in cl.get_platforms():
                try:
                    devices.extend(platform.get_devices(device_type=cl.device_type.GPU))
                except cl.Error:
                    continue
            selector = self.cfg.opencl_device.casefold().strip()
            cache_dir = DEBUG_DIR / "opencl_cache"
            cache_dir.mkdir(parents=True, exist_ok=True)
            for device in devices:
                name = f"{device.vendor} {device.name}"
                if selector and selector not in name.casefold():
                    continue
                version = re.search(r"OpenCL (\d+)\.(\d+)", device.version)
                if (not device.available or not device.compiler_available or version is None
                        or tuple(map(int, version.groups())) < (1, 2)):
                    continue
                key = device.int_ptr
                if key in _OPENCL_FAILED:
                    continue
                try:
                    # 某些驱动的首次编译会直接终止进程（无法由 try/except 捕获）。
                    # 先在禁输入子进程预热并核对；失败/超时仅禁用该设备。
                    if key not in _OPENCL_CHECKED and os.environ.get("JUMPBOT_OPENCL_PROBE") != "1":
                        cmd = [sys.executable]
                        if not _is_frozen():
                            cmd.append(str(Path(__file__).resolve()))
                        cmd += ["--_opencl-check", device.name]
                        env = dict(os.environ, JUMPBOT_NO_INPUT="1", JUMPBOT_DISABLE_CUDA="1",
                                   JUMPBOT_OPENCL_PROBE="1")
                        check = subprocess.run(cmd, env=env, capture_output=True, timeout=20,
                                               encoding="utf-8", errors="replace",
                                               creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
                        if check.returncode != 0:
                            raise RuntimeError(f"隔离自检退出码 {check.returncode}；"
                                               f"{(check.stdout + check.stderr)[-500:]}")
                        _OPENCL_CHECKED.add(key)
                    if key not in _OPENCL_RUNTIMES:
                        context = cl.Context(devices=[device])
                        # 整数阈值：不要求 fp64、CUDA Toolkit 或某个显卡厂商扩展。
                        source = r'''
                        // OpenCV RGB2HSV_b adaptation; Apache-2.0 (THIRD_PARTY_NOTICES.md).
                        typedef struct { int h; int s; int v; } Hsv;
                        inline Hsv jumpbot_bgr_hsv(__global const uchar* p,
                                                  __constant const int* tables)
                        {
                            int b = p[0], g = p[1], r = p[2];
                            Hsv hsv;
                            hsv.v = max(b, max(g, r));
                            int delta = hsv.v - min(b, min(g, r));
                            hsv.s = (delta * tables[hsv.v] + 2048) >> 12;
                            int hue = hsv.v == r ? g - b
                                : hsv.v == g ? b - r + 2 * delta : r - g + 4 * delta;
                            hue = (hue * tables[256 + delta] + 2048) >> 12;
                            hsv.h = hue < 0 ? hue + 180 : hue;
                            return hsv;
                        }
                        __kernel void jumpbot_masks(__global const uchar* bgr,
                            __global const uchar* bg, __constant const int* tables,
                            __constant const int* t, __global uchar* out,
                            int n, int width, int top, int bottom)
                        {
                            int i = get_global_id(0);
                            if (i >= n) return;
                            int row = i / width;
                            if (row < top || row >= bottom) {
                                out[i] = out[n + i] = 0;
                                return;
                            }
                            int p = 3 * i, b = 3 * row;
                            Hsv c = jumpbot_bgr_hsv(bgr + p, tables);
                            Hsv base = jumpbot_bgr_hsv(bg + b, tables);
                            int db = (int)bgr[p] - bg[b];
                            int dg = (int)bgr[p + 1] - bg[b + 1];
                            int dr = (int)bgr[p + 2] - bg[b + 2];
                            int diff = abs(db) + abs(dg) + abs(dr);
                            int dh = c.h - base.h;
                            int block = base.v - c.v > t[1]
                                || (abs(dh) > t[2] && c.s > t[3])
                                || c.s - base.s > t[4] || base.s - c.s > t[5]
                                || (c.s < t[6] && c.v > t[7]) || diff > t[0]
                                || (t[13] && c.v - base.v > t[8] && c.s <= base.s);
                            int piece = ((c.s > t[9] && c.v < t[10])
                                         || (c.v < 110 && c.s > 25))
                                        && c.h >= t[11] && c.h <= t[12];
                            out[i] = piece ? 255 : 0;
                            out[n + i] = block ? 255 : 0;
                        }
                        '''
                        program = cl.Program(context, source).build(
                            options=["-cl-std=CL1.2"], cache_dir=str(cache_dir))
                        divisors = np.arange(1, 256, dtype=np.float64)
                        tables = np.zeros((2, 256), np.int32)
                        tables[0, 1:] = np.rint((255 << 12) / divisors).astype(np.int32)
                        tables[1, 1:] = np.rint((180 << 12) / (6 * divisors)).astype(np.int32)
                        table_buffer = cl.Buffer(context, cl.mem_flags.READ_ONLY | cl.mem_flags.COPY_HOST_PTR,
                                                 hostbuf=tables)
                        _OPENCL_RUNTIMES[key] = (context, program, table_buffer, name, source)
                    context, program, tables, name, source = _OPENCL_RUNTIMES[key]
                    runtime = {"key": key, "context": context, "program": program,
                               "queue": cl.CommandQueue(context),
                               "kernel": cl.Kernel(program, "jumpbot_masks"),
                               "tables": tables, "name": name, "source": source}
                    self._cl_candidates.append(runtime)
                except Exception as exc:
                    _OPENCL_FAILED.add(key)
                    log(f"OpenCL 设备 {name} 不可用，跳过：{exc}")
            self._cl_state = bool(self._cl_candidates)
            if not self._cl_state:
                log("OpenCL 未找到可用 GPU，回退 CPU")
        except Exception as exc:
            log(f"OpenCL 后端不可用，回退 CPU：{exc}")
        return self._cl_state

    def _run_opencl(self, runtime: dict, frame: np.ndarray):
        cl, cfg = self._cl, self.cfg
        h, w = frame.shape[:2]
        bg = np.clip(estimate_background_rows(frame), 0, 255).astype(np.uint8)
        # 左侧都是整数，>t 等价于 >floor(t)，<t 等价于 <ceil(t)。
        # 这样也兼容小数配置，内核不需要双精度浮点扩展。
        t = np.array((math.floor(cfg.block_color_delta), math.floor(cfg.block_dark_delta),
                      math.floor(cfg.block_hue_delta), math.floor(cfg.block_hue_sat_min),
                      math.floor(cfg.block_sat_delta), math.floor(cfg.block_pale_delta),
                      math.ceil(cfg.block_white_sat_max), math.floor(cfg.block_white_val_min),
                      math.floor(cfg.block_bright_delta), math.floor(cfg.piece_sat_min),
                      math.ceil(cfg.piece_val_max), math.ceil(cfg.piece_hue_min),
                      math.floor(cfg.piece_hue_max), int(cfg.block_bright_delta > 0)), dtype=np.int32)
        key, context, queue = runtime["key"], runtime["context"], runtime["queue"]
        buffers = self._cl_buffers.get(key)
        if buffers is None or buffers[0] != (h, w):
            mf = cl.mem_flags
            buffers = ((h, w), cl.Buffer(context, mf.READ_ONLY, size=h*w*3),
                       cl.Buffer(context, mf.READ_ONLY, size=h*3),
                       cl.Buffer(context, mf.READ_ONLY, size=t.nbytes),
                       cl.Buffer(context, mf.WRITE_ONLY, size=h*w*2))
            self._cl_buffers[key] = buffers
        _shape, bgr_buffer, bg_buffer, t_buffer, out_buffer = buffers
        # 保留本地引用直到阻塞下载完成；异步上传的输入不会提前释放。
        bgr = np.ascontiguousarray(frame)
        cl.enqueue_copy(queue, bgr_buffer, bgr, is_blocking=False)
        cl.enqueue_copy(queue, bg_buffer, bg, is_blocking=False)
        cl.enqueue_copy(queue, t_buffer, t, is_blocking=False)
        top = int(round(cfg.top_bar_ratio * h))
        bottom = int(round((1 - cfg.bottom_bar_ratio) * h))
        top, bottom, _step = slice(top, bottom).indices(h)
        n = h * w
        runtime["kernel"](queue, (n,), None, bgr_buffer, bg_buffer, runtime["tables"],
                          t_buffer, out_buffer, np.int32(n), np.int32(w),
                          np.int32(top), np.int32(bottom))
        result = np.empty((2, h, w), dtype=np.uint8)
        cl.enqueue_copy(queue, result, out_buffer, is_blocking=True)
        return result[0], result[1]

    def _masks_opencl(self, frame: np.ndarray):
        """对可用 GPU 逐像素核对并实测选卡；每次返回独立的 CPU 结果。"""
        if not self._init_opencl():
            return None
        try:
            if self._cl_runtime is None:
                expected = self._masks_simd(frame)
                candidates = []
                for runtime in self._cl_candidates:
                    try:
                        got = self._run_opencl(runtime, frame)
                        if any(not np.array_equal(a, b) for a, b in zip(expected, got)):
                            raise ValueError("OpenCL 输出与 CPU 逐像素核对失败")
                        samples = []
                        for _ in range(3):
                            t0 = time.perf_counter()
                            self._run_opencl(runtime, frame)
                            samples.append(time.perf_counter() - t0)
                        candidates.append((sorted(samples)[1], runtime))
                    except Exception as exc:
                        self._cl_buffers.pop(runtime["key"], None)
                        log(f"OpenCL 设备 {runtime['name']} 执行失败，跳过：{exc}")
                if not candidates:
                    self._cl_state = False
                    return None
                _ms, self._cl_runtime = min(candidates, key=lambda x: x[0])
                chosen = self._cl_runtime["key"]
                self._cl_buffers = {chosen: self._cl_buffers[chosen]}
                log(f"OpenCL 掩码设备：{self._cl_runtime['name']}")
            return self._run_opencl(self._cl_runtime, frame)
        except Exception as exc:
            log(f"OpenCL 掩码计算出错，回退 CPU：{exc}")
            self._cl_state = False
            self._cl_buffers.clear()
            return None

    def _masks_simd(self, frame: np.ndarray):
        """掩码计算：全部用 cv2 的 uint8(SIMD) 运算，与 numpy 版**逐像素等价**。

        等价性依据（都为整数运算，不存在浮点误差）：
          * `cv2.compare` 成立处返回 255，正好等于旧实现 `.astype(uint8)*255`；
          * `cv2.subtract` 在 uint8 上饱和到 0，而旧实现是"负差 > 阈值"恒为 False，
            两边结果一致（已在 4 张真实帧上逐像素比对：差异 0）；
          * `|ΔB|+|ΔG|+|ΔR|` 用 uint16 累加，最大 765 不溢出，与旧实现的 int 求和一致。
        """
        cfg = self.cfg
        hsv = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        # 连续通道避免每次 cv2 算子重新复制 strided 的 HSV 子视图。
        hue, sat, val = cv2.split(hsv)

        bg = estimate_background_rows(frame)
        bg_u8 = np.clip(bg, 0, 255).astype(np.uint8).reshape(-1, 1, 3)
        bg_full = np.ascontiguousarray(np.broadcast_to(bg_u8, frame.shape))
        # 背景每行同色：只转换 h 个像素，避免转换整张展开的背景。
        bg_row_hsv = cv2.cvtColor(bg_u8, cv2.COLOR_BGR2HSV).reshape(-1, 3)
        bg_h, bg_s, bg_v = (np.ascontiguousarray(np.broadcast_to(bg_row_hsv[:, c:c+1],
                                                               frame.shape[:2]))
                          for c in range(3))

        # darker: val < bg_v - delta
        darker = cv2.compare(val, cv2.subtract(bg_v, cfg.block_dark_delta),
                             cv2.CMP_LT)
        # hue_diff: |hue - bg_h| > delta 且 sat > min
        hue_diff = cv2.bitwise_and(
            cv2.compare(cv2.absdiff(hue, bg_h), cfg.block_hue_delta, cv2.CMP_GT),
            cv2.compare(sat, cfg.block_hue_sat_min, cv2.CMP_GT),
        )
        sat_vivid = cv2.compare(cv2.subtract(sat, bg_s), cfg.block_sat_delta,
                                cv2.CMP_GT)
        sat_pale = cv2.compare(cv2.subtract(bg_s, sat), cfg.block_pale_delta,
                               cv2.CMP_GT)
        pure_white = cv2.bitwise_and(
            cv2.compare(sat, cfg.block_white_sat_max, cv2.CMP_LT),
            cv2.compare(val, cfg.block_white_val_min, cv2.CMP_GT),
        )
        if cfg.block_bright_delta > 0:
            bright = cv2.bitwise_and(
                cv2.compare(cv2.subtract(val, bg_v), cfg.block_bright_delta,
                            cv2.CMP_GT),
                cv2.compare(sat, bg_s, cv2.CMP_LE),
            )
        else:
            bright = None

        diff = cv2.absdiff(frame, bg_full)
        ch_b, ch_g, ch_r = cv2.split(diff)
        total = cv2.add(ch_b, ch_g, dtype=cv2.CV_16U)
        cv2.add(total, ch_r, dst=total, dtype=cv2.CV_16U)
        color_far = cv2.compare(total, cfg.block_color_delta, cv2.CMP_GT)

        canvas = self._canvas_mask(frame)
        block_mask = darker
        for extra in (hue_diff, sat_vivid, sat_pale, pure_white, bright, color_far):
            if extra is not None:
                cv2.bitwise_or(block_mask, extra, dst=block_mask)
        cv2.bitwise_and(block_mask, canvas, dst=block_mask)

        piece_mask = cv2.bitwise_or(
            cv2.bitwise_and(
                cv2.compare(sat, cfg.piece_sat_min, cv2.CMP_GT),
                cv2.compare(val, cfg.piece_val_max, cv2.CMP_LT),
            ),
            cv2.bitwise_and(
                cv2.compare(val, 110, cv2.CMP_LT),
                cv2.compare(sat, 25, cv2.CMP_GT),
            ),
        )
        cv2.bitwise_and(piece_mask,
                        cv2.compare(hue, cfg.piece_hue_min, cv2.CMP_GE),
                        dst=piece_mask)
        cv2.bitwise_and(piece_mask,
                        cv2.compare(hue, cfg.piece_hue_max, cv2.CMP_LE),
                        dst=piece_mask)
        cv2.bitwise_and(piece_mask, canvas, dst=piece_mask)
        return piece_mask, block_mask

    def _canvas_mask(self, frame: np.ndarray) -> np.ndarray:
        h, w = frame.shape[:2]
        mask = np.zeros((h, w), dtype=np.uint8)
        top = int(round(self.cfg.top_bar_ratio * h))
        bottom = int(round((1.0 - self.cfg.bottom_bar_ratio) * h))
        mask[top:bottom, :] = 255
        return mask

    def masks(self, frame: np.ndarray):
        backend = self.cfg.masks_backend
        if backend == "auto":
            backend = self._pick_backend(frame)
            t0 = time.perf_counter()
            result = self._masks_impl(backend, frame)
            self._note_backend_timing(backend, (time.perf_counter() - t0) * 1000.0)
            # 定期复测：不管当前用的是 gpu 还是 simd 都要计数，
            # 否则"选到 simd 之后 GPU 空出来了"这种情况永远切不回去。
            self._auto_calls = getattr(self, "_auto_calls", 0) + 1
            if self._auto_calls % 300 == 0:
                self._auto_backend = None
            return result
        return self._masks_impl(backend, frame)

    def _masks_impl(self, backend: str, frame: np.ndarray):
        if backend in ("gpu", "cuda", "opencl"):
            gpu = self._masks_opencl(frame) if backend == "opencl" else self._masks_gpu(frame)
            if gpu is not None:
                self._last_mask_backend = "opencl" if backend == "opencl" else "gpu"
                return gpu
            if self.cfg.masks_backend == "auto":
                self._auto_backend = "simd" if self.cfg.masks_simd else "cpu"
                self._gpu_ms_buf = []
            status_write(mask_backend="simd" if self.cfg.masks_simd else "cpu",
                         mask_bench_ms=f"{backend} 不可用，已回退 CPU")
        if backend != "cpu" and self.cfg.masks_simd:
            self._last_mask_backend = "simd"
            return self._masks_simd(frame)
        self._last_mask_backend = "cpu"
        hsv = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        sat = hsv[:, :, 1]
        val = hsv[:, :, 2]
        hue = hsv[:, :, 0]
        piece_mask = (
            (
                ((sat > self.cfg.piece_sat_min) & (val < self.cfg.piece_val_max))
                | ((val < 110) & (sat > 25))
            )
            & (hue >= self.cfg.piece_hue_min)
            & (hue <= self.cfg.piece_hue_max)
        )

        # 方块相对背景的三类偏离：更暗、色相不同、纯白（白色方块）
        bg = estimate_background_rows(frame)
        bg_u8 = np.clip(bg, 0, 255).astype(np.uint8).reshape(-1, 1, 3)
        bg_hsv = cv2.cvtColor(bg_u8, cv2.COLOR_BGR2HSV).reshape(-1, 3).astype(np.int16)
        val_i = val.astype(np.int16)
        hue_i = hue.astype(np.int16)
        darker = val_i < (bg_hsv[:, 2][:, None] - self.cfg.block_dark_delta)
        hue_diff = (np.abs(hue_i - bg_hsv[:, 0][:, None]) > self.cfg.block_hue_delta) & (
            sat > self.cfg.block_hue_sat_min
        )
        # 背景是淡色、方块是鲜艳色时，色相可能接近（例如粉背景 vs 橙块），但饱和度差很大
        sat_vivid = (sat.astype(np.int16) - bg_hsv[:, 1][:, None]) > self.cfg.block_sat_delta
        # 反向：背景是有色淡色、方块是灰白面（例如淡色背景上的浅灰“便利店”屋顶）。
        # 阈值比 sat_vivid 小，因为阴影几乎不改变饱和度、而屋顶差很大，这样能只抓方块。
        sat_pale = (bg_hsv[:, 1][:, None] - sat.astype(np.int16)) > self.cfg.block_pale_delta
        pure_white = (sat < self.cfg.block_white_sat_max) & (val > self.cfg.block_white_val_min)
        # 比背景更亮、且不比背景更饱和 = 浅色方块（淡背景上的白屋顶）。背景自身逐行是
        # 线性渐变模型，同一行内亮度基本恒定，所以这里用很小的亮度差就能分开。
        if self.cfg.block_bright_delta > 0:
            bright = ((val_i - bg_hsv[:, 2][:, None]) > self.cfg.block_bright_delta) & (
                sat.astype(np.int16) <= bg_hsv[:, 1][:, None]
            )
        else:
            bright = np.zeros_like(darker)
        # 与背景的整体色差（应对“浅灰屋顶 on 淡紫背景”这类低对比度方块：
        # 它既不比背景暗 30、也不算纯白、饱和度差又刚好卡在阈值下）
        # 用 cv2.absdiff（SIMD）替代 int16 手写减法：4 张真实帧上逐像素比对**完全一致**，
        # 单帧省 ~2ms（masks 是每帧都跑的热点）。
        bg_bgr = np.clip(bg, 0, 255).astype(np.uint8).reshape(-1, 1, 3)
        bg_full = np.ascontiguousarray(np.broadcast_to(bg_bgr, frame.shape))
        color_far = cv2.absdiff(frame, bg_full).sum(axis=2) > self.cfg.block_color_delta
        block_mask = (
            darker | hue_diff | sat_vivid | sat_pale | pure_white | bright | color_far
        ).astype(np.uint8) * 255
        canvas = self._canvas_mask(frame)
        piece_mask = (piece_mask.astype(np.uint8) * 255) & canvas
        block_mask = block_mask & canvas
        return piece_mask, block_mask

    # ---------- 棋子 ----------

    def find_piece(self, frame: np.ndarray, mask: np.ndarray) -> Piece | None:
        cfg = self.cfg
        h, w = frame.shape[:2]
        mask = cv2.morphologyEx(mask, cv2.MORPH_OPEN, np.ones((5, 5), np.uint8), iterations=1)
        mask = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, np.ones((5, 5), np.uint8), iterations=1)

        num, _labels, stats, _cent = cv2.connectedComponentsWithStats(mask, 8)
        canvas_area = w * h
        min_area = cfg.piece_min_area_ratio * canvas_area
        max_area = cfg.piece_max_area_ratio * canvas_area
        candidates = []
        for i in range(1, num):
            x, y, bw, bh, area = stats[i]
            if area < min_area or area > max_area:
                continue
            if bw < cfg.piece_min_width_ratio * w:
                continue
            if bh < bw * cfg.piece_aspect_min:
                continue
            if y < self.cfg.top_bar_ratio * h:
                continue
            # 棋子始终站在画面下半部；开局动画/HUD 里的深色图形常在上半部
            if (y + bh) < cfg.piece_min_y_ratio * h:
                continue
            # 被窗口边缘切掉一半的“棋子”不可信
            if x <= 1 or x + bw >= w - 1:
                continue
            anchor = (x + bw / 2.0, y + bh)
            bbox = (int(x), int(y), int(bw), int(bh))
            candidates.append(
                Piece(anchor[0], anchor[1], bbox, int(area),
                      tpl_score=piece_template_score(frame, bbox))
            )
        scan_hits = []
        if (not cfg.piece_scan_only_when_needed) or (not candidates):
            scan_hits = piece_template_scan(frame, cfg)
        if scan_hits:
            # 与颜色候选合并去重（位置接近的取模板分更高的）
            for hit in scan_hits:
                replaced = False
                for idx, cand in enumerate(candidates):
                    if math.hypot(hit.x - cand.x, hit.y - cand.y) < 0.05 * w:
                        if hit.tpl_score > cand.tpl_score:
                            candidates[idx] = hit
                        replaced = True
                        break
                if not replaced:
                    candidates.append(hit)
        if not candidates:
            return None

        # 模板校验：去掉“暗色但不像棋子”的干扰物（例如药瓶的深色部分）。
        # 若所有候选都没过阈值，就保留原候选，避免模板过期时完全读不到棋子。
        if load_piece_template() is not None:
            good = [p for p in candidates if p.tpl_score >= cfg.piece_template_min_score]
            if good:
                candidates = good

        prior = self.piece
        if prior is not None:
            limit = cfg.piece_prior_radius * w
            near = [p for p in candidates if math.hypot(p.x - prior.x, p.y - prior.y) <= limit]
            if near:
                # 先验只用来圈定范围；范围内优先取“最像棋子”的那个
                near.sort(key=lambda p: (p.tpl_score, -math.hypot(p.x - prior.x, p.y - prior.y)),
                          reverse=True)
                return near[0]
            # 先验邻域内没有任何候选 → 先验已失效（镜头移动/误检），退回按面积选最可信的
            self.piece = None
        candidates.sort(key=lambda p: (p.tpl_score, p.area), reverse=True)
        return candidates[0]

    # ---------- 目标方块 ----------

    def find_target(
        self,
        frame: np.ndarray,
        block_mask: np.ndarray,
        piece: Piece,
        piece_mask: np.ndarray | None = None,
    ) -> Block | None:
        cfg = self.cfg
        h, w = frame.shape[:2]
        mask = block_mask.copy()

        # 只抹掉棋子本身（膨胀后的棋子掩码），保留周围方块用于定位
        if piece_mask is not None and piece_mask.any():
            grown = cv2.dilate(piece_mask, np.ones((9, 9), np.uint8), iterations=2)
            mask[grown > 0] = 0

        kernel = np.ones((5, 5), np.uint8)
        mask = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, kernel, iterations=2)
        mask = cv2.morphologyEx(mask, cv2.MORPH_OPEN, np.ones((3, 3), np.uint8), iterations=1)
        # 填充内部空洞（键盘提示字母、半透明光圈会把方块打出洞）
        contours, _hier = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
        filled = np.zeros_like(mask)
        cv2.drawContours(filled, contours, -1, 255, thickness=-1)
        mask = filled

        num, _labels, stats, _cent = cv2.connectedComponentsWithStats(mask, 8)
        comp = _labels
        canvas_area = w * h
        max_area = cfg.block_max_area_ratio * canvas_area

        def collect(min_area_ratio: float, min_width_ratio: float,
                    min_height_ratio: float) -> tuple:
            """返回 (正常候选, 退化候选)。

            退化 = 顶面估计宽度远小于连通域宽度，典型是“影子与方块粘成一团、
            顶角取到影子尖”的情形：这种候选算出的目标点会明显偏近/偏侧，
            用它跳跃会跳短摔死，所以只在最后兜底时降级使用。
            """
            good, degen = [], []
            for idx in range(1, num):
                bx, by, bwidth, bheight, area = stats[idx]
                if area < min_area_ratio * canvas_area or area > max_area:
                    continue
                # 只保留“最小宽度”约束：整宽 UI（标题栏、排行榜卡片）已经由
                # top_face_center 的“顶行必须很窄”检查挡掉，这里再限上限会误杀
                # “多个方块粘成的接近整宽的连通域”。
                if bwidth < min_width_ratio * w:
                    continue
                if bheight < min_height_ratio * h:
                    continue
                center = top_face_center(comp == idx, cfg.axis_slope)
                if center is None:
                    continue
                cx, cy, top_width, top_y = center
                if (cfg.aim_center_mode == "geometric"
                        and top_width >= cfg.low_ratio_gate * bwidth):
                    # 等距投影下菱形/椭圆顶面的中心行必然满足
                    # y = 顶角y + 0.5*顶宽*slope（顶角到中心行的竖直距离 = 半宽*slope）。
                    # 实测 172 帧"游戏自带中心白点"真值：欧氏中位误差 8.16px→3.11px、
                    # 沿轴偏置 +3.71px→-0.57px，且各子组（小方块/大方块/退化块）都不变差，
                    # 退化块上两式本来重合（见 README「瞄点几何中心」）。
                    # 只在"顶面估计可信"（顶宽/连通域宽度 >= low_ratio_gate）时替换：
                    # 退化候选上几何式不一定成立，实测会让个别稳定帧从"有目标"变成"没目标"
                    # （虽然会重试自愈，但没必要冒这个险）。
                    cy_geo = top_y + 0.5 * top_width * cfg.axis_slope
                    # 防退化护栏：顶宽不可能超过连通域宽度，中心也不该跑出连通域竖直范围
                    if top_width <= bwidth and by <= cy_geo <= by + bheight:
                        cy = cy_geo
                block = Block(cx, cy, (int(bx), int(by), int(bwidth), int(bheight)), int(area),
                              top_y=top_y, top_width=top_width)
                need_width = max(12.0, cfg.block_degenerate_top_ratio * bwidth)
                (good if top_width >= need_width else degen).append(block)
            return good, degen

        # 直接按“合法目标”的三个硬条件筛选，而不是去猜哪块是脚下块：
        #   1) 中心必须在棋子上方；
        #   2) 沿轴距离必须落在合理跳跃区间内；
        #   3) 必须基本贴着跳跃轴向（横向偏移不能过大）。
        # 棋子脚下那块会被 2) 或 3) 自然筛掉（它要么太近，要么中心被棋子遮挡算歪）。
        min_dist = cfg.min_jump_dist_ratio * w
        max_dist = cfg.max_jump_dist_ratio * w

        def select(blocks: list):
            picked = []
            for block in blocks:
                if block.y >= piece.y - cfg.target_min_up_ratio * h:
                    continue
                _axis, along, cross = axis_components(
                    block.x - piece.x, block.y - piece.y, cfg.axis_slope
                )
                if not (min_dist <= along <= max_dist):
                    continue
                if cross / max(abs(along), 1.0) > cfg.axis_max_cross_ratio:
                    continue
                picked.append((along, block))
            picked.sort(key=lambda item: item[0])
            return picked

        # 分级放宽：严格条件选不出目标时，依次放宽
        #   1) 尺寸（药瓶/礼盒等特殊小方块）
        #   2) 距离下限 + 横向比（棋子站在便利店等宽方块偏前位置时，
        #      到目标中心的实测距离会偏小、方向也会偏轴）
        # 宁可跳得勉强，也不要因为一步选不出目标而整轮停止（死亡可自动重开）。
        good1, _degen1 = collect(cfg.block_min_area_ratio, cfg.block_min_width_ratio,
                                 cfg.block_min_height_ratio)
        best = select(good1)
        good2, degen2 = collect(cfg.block_min_area_ratio * 0.4, 0.05, 0.05)
        if not best:
            best = select(good2)
            if best:
                log("已放宽方块尺寸门限（特殊小方块）")
        if not best:
            picked = []
            for block in good2 + degen2:
                if block.y >= piece.y - cfg.target_min_up_ratio * h:
                    continue
                _axis, along, cross = axis_components(
                    block.x - piece.x, block.y - piece.y, cfg.axis_slope
                )
                if along < cfg.min_jump_dist_ratio * w * 0.5 or along > cfg.max_jump_dist_ratio * w:
                    continue
                if cross / max(abs(along), 1.0) > 0.60:
                    continue
                tier = 1 if block in degen2 else 0
                picked.append((tier, along, block))
            picked.sort(key=lambda item: (item[0], item[1]))
            if picked:
                best = [(item[1], item[2]) for item in picked]
                log("已放宽距离下限与横向比（贴邻/宽方块场景）")
                if picked[0][0] == 1:
                    log("警告：目标方块顶面估计退化（影子/碎片粘连），正在使用兜底候选")
        if not best:
            return None
        chosen = best[0][1]
        # 救援：细长竖直的小物件（跳板旗杆之类）常常比真正的大方块更近，
        # 会被“取最近”规则误选。这里让位给同轴上明显更大的正常方块。
        if cfg.item_rescue_min_aspect > 0 and chosen.bbox[3] > cfg.item_rescue_min_aspect * chosen.bbox[2]:
            alt = None
            for block in good1 + good2:
                if block is chosen:
                    continue
                if block.area < max(3 * chosen.area, cfg.block_min_area_ratio * canvas_area):
                    continue
                _axis, along, cross = axis_components(
                    block.x - piece.x, block.y - piece.y, cfg.axis_slope
                )
                if not (min_dist <= along <= max_dist):
                    continue
                if cross / max(abs(along), 1.0) > 0.30:
                    continue
                if alt is None or along < alt[0]:
                    alt = (along, block)
            if alt is not None:
                log(f"选中候选是细长小物件（{chosen.bbox[2]}x{chosen.bbox[3]}，面积 "
                    f"{chosen.area}），改选同轴 {alt[0]:.0f}px 处的大方块（面积 {alt[1].area}）")
                return alt[1]
        # 中心标记：方块上有游戏自带的亮白小圆点时，它就是真正的中心判定点，
        # 比"顶面中心估计"准（抬高的台面会偏 20~40px）。只在"顶面估计明显不可信
        # 的大方块"上启用，见 Config.center_dot_ratio_gate；0 可整体关闭。
        if (cfg.center_dot_max_offset_px > 0
                and chosen.top_width < cfg.center_dot_ratio_gate * max(chosen.bbox[2], 1)
                and chosen.bbox[2] >= cfg.center_dot_min_block_width):
            # 极端退化候选（顶宽/连通域宽 < center_dot_degenerate_ratio）的顶面估计已
            # 完全不可信——实测瞄点会偏离真中心 50~103px，而默认 55px 搜索半径因此
            # 找不到白点，只能拿那个坏估计去瞄。实测代价：2026-10-01 13:58 #147 就
            # 这么摔掉了一条 17015 分的命（aim 偏 ~80px，棋子从便利店屋顶左缘掉下去）。
            # 这类候选占全部跳数 0.18%，**放大半径只在这一类上生效，对正常跳数零影响**。
            # 现场帧复核：16 个极端退化样本里，55px 能找到白点 3 个，120px 能找回 6 个。
            radius = cfg.center_dot_max_offset_px
            if (cfg.center_dot_degenerate_radius_px > radius
                    and chosen.top_width < cfg.center_dot_degenerate_ratio * max(chosen.bbox[2], 1)):
                radius = cfg.center_dot_degenerate_radius_px
            dot = find_dot_near(frame, chosen.x, chosen.y, radius, cfg)
            if dot is not None and (abs(dot[0] - chosen.x) > 3.0 or abs(dot[1] - chosen.y) > 3.0):
                # 注意：find_target 每帧会被调用多次（acquire_frame / wait_landing），
                # 这里**不能**打日志，否则一帧刷几十行。主循环在真正起跳前统一打一条。
                return Block(dot[0], dot[1], chosen.bbox, chosen.area,
                             top_y=chosen.top_y, top_width=chosen.top_width, dot=dot)
        return chosen

    def detect(self, frame: np.ndarray):
        piece_mask, block_mask = self.masks(frame)
        piece = self.find_piece(frame, piece_mask)
        if piece is None:
            return None, None, piece_mask, block_mask
        target = self.find_target(frame, block_mask, piece, piece_mask)
        self.piece = piece
        return piece, target, piece_mask, block_mask

    def detect_piece_only(self, frame: np.ndarray):
        """只找棋子，跳过 find_target（返回 piece, piece_mask, block_mask）。

        wait_landing 的落地确认只用到"棋子锚点 + 脚下方块掩码"，它每跳要跑 3 轮，
        而 find_target 的结果在那条路径上只用于 --save-debug 的标注图（实测
        find_target 16.5ms，占 detect() 的 41%）。先验 self.piece 的更新规则与
        detect() 逐字保持一致，否则 find_piece 的后续行为会变。
        """
        piece_mask, block_mask = self.masks(frame)
        piece = self.find_piece(frame, piece_mask)
        if piece is None:
            return None, piece_mask, block_mask
        self.piece = piece
        return piece, piece_mask, block_mask


def axis_components(dx: float, dy: float, slope: float) -> tuple:
    """把“棋子→目标”向量分解到等距投影的两个轴向上。

    返回 (单位轴向 u, 沿轴长度 along, 横向偏移 cross)。跳跃方向被游戏锁死为两个轴向，
    所以应按沿轴投影长度计算按压时长：这样即使棋子落偏，误差也不会沿轴累积，而是每跳
    按固定比例衰减。
    """
    norm = math.sqrt(1.0 + slope * slope)
    sign = 1.0 if dx >= 0 else -1.0
    ux = sign / norm
    uy = -slope / norm
    along = dx * ux + dy * uy
    cross = abs(dx * (-uy) + dy * ux)
    return (ux, uy), along, cross


def press_distance(piece: "Piece", target: "Block", cfg: Config) -> tuple:
    """按压距离：**沿等距轴向的投影长度**，与主循环完全一致。

    调试入口（--dry-run / --jump-test / --image）早期用的是欧氏距离，数值比实跑大
    15~20%（实测同一帧：欧氏 95.6px vs 沿轴 80.6px），会让"离线算出来的 hold"跟
    真实按压时长对不上。所有入口统一走这个函数。
    返回 (along, cross)。
    """
    _axis, along, cross = axis_components(target.x - piece.x, target.y - piece.y, cfg.axis_slope)
    return along, cross


def annotate(frame: np.ndarray, piece: Piece | None, target: Block | None,
             hold_ms: float | None, extra: str = "") -> np.ndarray:
    img = frame.copy()
    if piece is not None:
        x, y, bw, bh = piece.bbox
        cv2.rectangle(img, (x, y), (x + bw, y + bh), (0, 0, 255), 2)
        cv2.drawMarker(img, (int(piece.x), int(piece.y)), (0, 0, 255), cv2.MARKER_CROSS, 18, 2)
    if target is not None:
        x, y, bw, bh = target.bbox
        cv2.rectangle(img, (x, y), (x + bw, y + bh), (0, 255, 0), 2)
        cv2.drawMarker(img, (int(target.x), int(target.y)), (0, 255, 0), cv2.MARKER_CROSS, 24, 2)
    if piece is not None and target is not None:
        p1 = (int(piece.x), int(piece.y))
        p2 = (int(target.x), int(target.y))
        cv2.line(img, p1, p2, (255, 0, 0), 2)
        dist = math.hypot(p2[0] - p1[0], p2[1] - p1[1])
        label = f"d={dist:.0f}px"
        if hold_ms is not None:
            label += f"  hold={hold_ms:.0f}ms"
        cv2.putText(img, label, (10, 80), cv2.FONT_HERSHEY_SIMPLEX, 0.7, (0, 0, 0), 4, cv2.LINE_AA)
        cv2.putText(img, label, (10, 80), cv2.FONT_HERSHEY_SIMPLEX, 0.7, (255, 255, 255), 2, cv2.LINE_AA)
    if extra:
        cv2.putText(img, extra, (10, 118), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (0, 0, 0), 4, cv2.LINE_AA)
        cv2.putText(img, extra, (10, 118), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (0, 255, 255), 2, cv2.LINE_AA)
    return img


def read_score(frame: np.ndarray, cfg: Config):
    """数字模板匹配读分；模板缺失时返回 None。"""
    features = load_digit_features()
    if not features:
        return None
    roi = crop_score_roi(frame, cfg)
    if roi is None:
        return None
    glyphs = segment_glyphs(roi)
    if not glyphs:
        return None
    digits = []
    for glyph in glyphs:
        matched = classify_glyph(glyph, features)
        if matched is None:
            return None
        digits.append(matched)
    try:
        return int("".join(digits))
    except ValueError:
        return None


GLYPH_SIZE = (24, 32)


def crop_score_roi(frame: np.ndarray, cfg: Config) -> np.ndarray | None:
    h, w = frame.shape[:2]
    y0 = int(cfg.score_roi[1] * h)
    y1 = int(cfg.score_roi[3] * h)
    x0 = int(cfg.score_roi[0] * w)
    x1 = int(cfg.score_roi[2] * w)
    roi = frame[y0:y1, x0:x1]
    return roi if roi.size else None


def segment_glyphs(roi: np.ndarray) -> list:
    """把分数区域切成单个数字字形（深色字 + 浅色背景）。"""
    gray = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)
    _thr, binary = cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY_INV + cv2.THRESH_OTSU)
    binary = cv2.morphologyEx(binary, cv2.MORPH_CLOSE, np.ones((3, 3), np.uint8), iterations=1)
    num, _labels, stats, _cent = cv2.connectedComponentsWithStats(binary, 8)
    boxes = []
    min_height = max(6, int(0.35 * roi.shape[0]))
    for i in range(1, num):
        x, y, bw, bh, area = stats[i]
        if bh < min_height or area < 25:
            continue
        boxes.append((int(x), int(y), int(bw), int(bh)))
    boxes.sort(key=lambda b: b[0])
    glyphs = []
    for x, y, bw, bh in boxes:
        glyphs.append(binary[y:y + bh, x:x + bw])
    return glyphs


def glyph_feature(glyph: np.ndarray, size=GLYPH_SIZE) -> np.ndarray:
    """把字形归一化成特征图。

    必须**保持宽高比**再居中填充：早期版本直接拉伸到固定尺寸，会把 “2/3/8” 这类
    宽高比不同的字形压成相似形状，导致聚类把不同数字混在一起。
    """
    height, width = size[1], size[0]
    h, w = glyph.shape[:2]
    scale = height / max(h, 1)
    new_w = max(1, min(width, int(round(w * scale))))
    resized = cv2.resize(glyph, (new_w, height), interpolation=cv2.INTER_AREA)
    canvas = np.zeros((height, width), np.uint8)
    x0 = (width - new_w) // 2
    canvas[:, x0:x0 + new_w] = resized
    return canvas.astype(np.float32) / 255.0


def glyph_similarity(a: np.ndarray, b: np.ndarray) -> float:
    fa = glyph_feature(a)
    fb = glyph_feature(b)
    denom = float(np.linalg.norm(fa) * np.linalg.norm(fb)) + 1e-6
    return float((fa * fb).sum() / denom)


def classify_glyph(glyph: np.ndarray, features: dict, threshold: float = 0.80):
    """features: {数字: 已归一化特征图}（见 load_digit_features，特征只算一次）。"""
    fa = glyph_feature(glyph)
    norm_a = float(np.linalg.norm(fa)) + 1e-6
    best = None
    for label, fb in features.items():
        score = float((fa * fb).sum() / (norm_a * (float(np.linalg.norm(fb)) + 1e-6)))
        if best is None or score > best[0]:
            best = (score, label)
    if best is None or best[0] < threshold:
        return None
    return best[1]


_DIGIT_TEMPLATES: dict | None = None
_DIGIT_FEATURES: dict | None = None


def load_digit_templates() -> dict:
    """加载 0-9 数字模板（进程内只读一次；早期版本每次读分都重读 10 个 PNG）。"""
    global _DIGIT_TEMPLATES
    if _DIGIT_TEMPLATES is None:
        _DIGIT_TEMPLATES = {}
        if TEMPLATE_DIR.exists():
            for path in sorted(TEMPLATE_DIR.glob("digit_*.png")):
                digit = path.stem.split("_")[-1]
                img = cv2.imread(str(path), cv2.IMREAD_GRAYSCALE)
                if img is not None:
                    _DIGIT_TEMPLATES[digit] = img
    return _DIGIT_TEMPLATES


def load_digit_features() -> dict:
    """数字模板的特征图（同样是进程内只算一次）。"""
    global _DIGIT_FEATURES
    if _DIGIT_FEATURES is None:
        _DIGIT_FEATURES = {label: glyph_feature(img)
                           for label, img in load_digit_templates().items()}
    return _DIGIT_FEATURES


_UI_TEMPLATES: dict | None = None


def load_ui_templates() -> dict:
    """加载界面按钮模板（ui_start=开始游戏，ui_restart=再玩一局），存灰度图。

    直接缓存灰度版：match_ui 在抓帧循环里被高频调用，早期版本每次都把模板重转灰度。
    """
    global _UI_TEMPLATES
    if _UI_TEMPLATES is None:
        _UI_TEMPLATES = {}
        if TEMPLATE_DIR.exists():
            for path in sorted(TEMPLATE_DIR.glob("ui_*.png")):
                img = cv2.imread(str(path))
                if img is not None:
                    _UI_TEMPLATES[path.stem[3:]] = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    return _UI_TEMPLATES


def match_ui(frame: np.ndarray, threshold: float = 0.70):
    """匹配已知界面按钮，返回 (名称, 中心坐标, 相似度)。"""
    tpls = load_ui_templates()
    if not tpls:
        return None
    gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
    best = None
    for name, tpl_gray in tpls.items():
        if tpl_gray.shape[0] >= gray.shape[0] or tpl_gray.shape[1] >= gray.shape[1]:
            continue
        res = cv2.matchTemplate(gray, tpl_gray, cv2.TM_CCOEFF_NORMED)
        _min_val, max_val, _min_loc, max_loc = cv2.minMaxLoc(res)
        if best is None or max_val > best[2]:
            best = (name, max_loc, float(max_val))
    if best is None or best[2] < threshold:
        return None
    name, loc, score = best
    tpl_h, tpl_w = tpls[name].shape[:2]
    return name, (loc[0] + tpl_w / 2.0, loc[1] + tpl_h / 2.0), score


# --------------------------------------------------------------------------------------
# 入口与 probe
# --------------------------------------------------------------------------------------


class WindowGoneError(RuntimeError):
    """目标窗口暂时找不到（可能是被关掉或最小化到托盘）。"""


def resolve_window(cfg: Config, wait_s: float = 0.0, draining: bool = False) -> WindowInfo:
    """查找目标窗口；wait_s > 0 时会在窗口暂时消失期间等待它回来。"""
    deadline = time.time() + max(wait_s, 0.0)
    logged = False
    wait_start = time.time()
    next_report = wait_start + 5.0
    while True:
        if not draining:
            control_check()
        elif poll_stop_key():
            raise ControlInterrupted("用户停止")
        windows = find_windows(cfg.window_title_pattern, cfg.window_exe_hint)
        if windows:
            if len(windows) > 1:
                log(f"匹配到 {len(windows)} 个窗口，取客户区最大的一个：")
                for wi in windows:
                    log("  - " + wi.describe())
            return windows[0]
        if draining and _CONTROL is not None and _CONTROL.pause_requested:
            raise WindowGoneError("暂停收尾期间窗口消失（不等待、不判死亡）")
        if time.time() >= deadline:
            raise WindowGoneError(
                f"未找到标题匹配 {cfg.window_title_pattern!r} 的窗口。"
                "请先在微信中打开「跳一跳」小游戏窗口后再运行。"
            )
        if not logged:
            log(f"目标窗口暂时找不到，等待它重新出现（最多 {wait_s:.0f} 秒）…")
            logged = True
        # 每 5 秒刷新一次状态与日志：早期版本这里**完全静默**地等 120 秒，
        # 面板和日志都毫无动静，看起来就是"卡死"。
        if time.time() >= next_report:
            waited = int(time.time() - wait_start)
            log(f"仍在等待「跳一跳」窗口…（已等 {waited} 秒 / 上限 {wait_s:.0f} 秒）")
            status_write(state="waiting_window", game_state=None, alive=True,
                         waited_s=waited, stop_reason="正在等待游戏窗口")
            next_report = time.time() + 5.0
        cooperative_sleep(0.5, draining=draining)


SWP_NOZORDER = 0x0004
SWP_NOACTIVATE = 0x0010


def ensure_window_size(win: WindowInfo, cfg: Config) -> WindowInfo:
    """把游戏窗口的**客户区**校正回标定基准尺寸（默认 553x1050）。

    为什么必须做：识别里所有**绝对像素**阈值都按基准尺寸标定（中心白点面积 14~260、
    最小方块宽 120px、落点安定容差 3px…）。窗口被拖动/缩放后，最直接的症状是
    "开始游戏"按钮的模板匹配从 0.95 掉到 ~0.79 → 点不进游戏，看起来就像"识别不了"
    （实测 2026-10-01：客户区 532x1016 时的表现，识别本身还能出东西但阈值全失配）。

    做法：按"外框 = 客户区 + 边框"换算后调 SetWindowPos，迭代收敛到精确尺寸
    （不能靠用户拖边框，差几个像素阈值照样对不上）。policy=warn 只提示不改，off 不管。
    """
    control_check()
    try:
        policy = str(getattr(cfg, "window_size_policy", "auto")).lower()
        if policy == "off":
            return win
        tw = int(getattr(cfg, "window_ref_width", 553))
        th = int(getattr(cfg, "window_ref_height", 1050))
        tol = max(0, int(getattr(cfg, "window_size_tol_px", 1)))
        if abs(win.width - tw) <= tol and abs(win.height - th) <= tol:
            return win
        if policy != "auto":
            log(f"警告：游戏窗口客户区 {win.width}x{win.height} 与标定基准 {tw}x{th} 不一致，"
                f"识别阈值可能失配（window_size_policy={policy}，只警告不改）")
            return win

        hwnd = wintypes.HWND(win.hwnd)
        before = (win.width, win.height)
        for _ in range(4):
            control_check()
            wr = wintypes.RECT()
            cr = wintypes.RECT()
            if not user32.GetWindowRect(hwnd, ctypes.byref(wr)):
                break
            if not user32.GetClientRect(hwnd, ctypes.byref(cr)):
                break
            ow, oh = wr.right - wr.left, wr.bottom - wr.top
            cw, ch = cr.right - cr.left, cr.bottom - cr.top
            if abs(cw - tw) <= tol and abs(ch - th) <= tol:
                break
            user32.SetWindowPos(hwnd, None, wr.left, wr.top,
                                ow + (tw - cw), oh + (th - ch),
                                SWP_NOZORDER | SWP_NOACTIVATE)
            cooperative_sleep(0.25)
        new = resolve_window(cfg, 0.5)
        after = (new.width, new.height)
        if after != before:
            log(f"窗口尺寸已自动校正：客户区 {before[0]}x{before[1]} → {after[0]}x{after[1]}"
                f"（标定基准 {tw}x{th}；绝对像素阈值都按基准标定）")
        elif after != (tw, th):
            log(f"警告：窗口尺寸校正后仍是 {after[0]}x{after[1]}（基准 {tw}x{th}），"
                f"识别阈值可能失配")
        status_write(window={"hwnd": new.hwnd, "pid": new.pid, "title": new.title,
                             "client": list(new.client_rect)})
        return new
    except ControlInterrupted:
        raise
    except Exception as exc:
        # 尺寸守卫绝不能把主流程搞挂：出问题就按原窗口继续
        log(f"窗口尺寸校正失败（忽略，按当前尺寸继续）：{exc}")
        return win


def selftest(args) -> int:
    """部署自检：不碰游戏窗口，验证依赖 / 模板 / GPU 后端 / 掩码一致性 / 计时。"""
    ok = True
    log(f"jumpbot selftest：frozen={_is_frozen()}  app_dir={PROJECT_DIR}")
    log(f"资源目录：{DATA_DIR}")
    try:
        import numpy, cv2 as _cv2, mss as _mss
        log(f"依赖：numpy {numpy.__version__} / opencv {_cv2.__version__} / mss {getattr(_mss,'__version__','?')}")
    except Exception as exc:
        log(f"依赖导入失败：{exc}")
        return 2
    tpl = sorted(TEMPLATE_DIR.glob("*.png"))
    log(f"模板：{TEMPLATE_DIR} 共 {len(tpl)} 个 → "
        f"{[p.name for p in tpl[:6]]}{' …' if len(tpl) > 6 else ''}")
    if len(tpl) == 0:
        log("警告：没有找到任何模板，识别会失效")
        ok = False
    else:
        # 顺带验证 PNG 读取链路（打包时可能漏掉解码器）
        probe_img = cv2.imread(str(tpl[0]), cv2.IMREAD_GRAYSCALE)
        if probe_img is None or probe_img.size == 0:
            log(f"警告：模板 {tpl[0].name} 读不出来（图像解码器缺失？）")
            ok = False
        else:
            log(f"图像解码：{tpl[0].name} {probe_img.shape[1]}x{probe_img.shape[0]} ✓")

    cfg = Config.load(Path(args.config))
    h, w = 1050, 553
    frame = np.full((h, w, 3), (230, 220, 215), np.uint8)
    frame[h // 2:, :] = (200, 195, 190)                 # 造一点渐变背景
    cv2.rectangle(frame, (150, 600), (330, 760), (90, 120, 95), -1)     # 假方块
    cv2.rectangle(frame, (250, 430), (420, 560), (235, 235, 235), -1)   # 假白块
    cv2.circle(frame, (280, 520), 12, (60, 45, 40), -1)                  # 假棋子
    bench, outputs, actual = {}, {}, {}
    for backend in ("cpu", "simd", "gpu", "opencl"):
        c = Config(**{**cfg.__dict__, "masks_backend": backend, "masks_simd": backend != "cpu"})
        v = Vision(c)
        v.masks(frame)          # 预热：GPU 首次调用要建上下文 + JIT 编译内核（约 1 秒）
        t0 = time.perf_counter()
        for _ in range(10):
            outputs[backend] = v.masks(frame)
        bench[backend] = (time.perf_counter() - t0) / 10 * 1000
        actual[backend] = v._last_mask_backend
    # auto：启动时真正走的路径 → 报出它会选谁（GPU 被别人占用时会选 simd）
    c_auto = Config(**{**cfg.__dict__, "masks_backend": "auto"})
    v_auto = Vision(c_auto)
    v_auto.masks(frame)
    t0 = time.perf_counter()
    for _ in range(10):
        v_auto.masks(frame)
    auto_ms = (time.perf_counter() - t0) / 10 * 1000
    log(f"auto 后端：选择 {v_auto._auto_backend}，实测 {auto_ms:.2f} ms")
    details = []
    ps, bs = outputs["simd"]
    for backend in ("cpu", "simd", "gpu", "opencl"):
        label = f"{bench[backend]:.2f}ms"
        if actual[backend] != backend:
            label = f"不可用（回退 {actual[backend]} {label}）"
        details.append(f"{backend} {label}")
        if actual[backend] != backend:
            continue
        pg, bg = outputs[backend]
        diff_p, diff_b = int((ps != pg).sum()), int((bs != bg).sum())
        log(f"真实 {backend} vs SIMD 掩码差异：piece={diff_p} block={diff_b}（应为 0）")
        if diff_p or diff_b:
            ok = False
    log("masks 计时（合成帧）：" + " / ".join(details))
    c_simd = Config(**{**cfg.__dict__, "masks_backend": "simd", "masks_simd": True})
    piece, target, _pm, _bm = Vision(c_simd).detect(frame)
    log(f"detect 链路：piece={'有' if piece else '无'} target={'有' if target else '无'}")
    log("自检结果：" + ("全部通过 ✓" if ok else "有项目未通过 ✗"))
    return 0 if ok else 1


def cupydebug(args) -> int:
    """打印 CuPy 导入/初始化的完整异常（打包排障用）。"""
    import traceback
    log(f"frozen={_is_frozen()} DATA_DIR={DATA_DIR}")
    roots = [Path(sp) / "nvidia" for sp in __import__("site").getsitepackages()]
    roots += [DATA_DIR / "nvidia", PROJECT_DIR / "nvidia"]
    for root in roots:
        if root.exists():
            bins = sorted(str(p) for p in root.glob("*/bin") if p.exists())
            log(f"发现 CUDA 运行时目录 {root}：{bins}")
            if bins:
                os.environ["PATH"] = os.pathsep.join(bins + [os.environ.get("PATH", "")])
                os.environ.setdefault("CUDA_PATH", str(root))
    try:
        import cupy
        log(f"cupy {cupy.__version__} 导入成功；设备数 "
            f"{cupy.cuda.runtime.getDeviceCount()}")
        a = cupy.arange(8, dtype=cupy.uint8)
        log(f"elementwise 测试：{(a + 1).sum()}")
        return 0
    except Exception:
        log("CuPy 导入失败，完整 traceback：")
        traceback.print_exc()
        return 1


def probe(args) -> int:
    cfg = Config.load(Path(args.config), {"k": args.k} if args.k else None)
    win = resolve_window(cfg)
    win = ensure_window_size(win, cfg)
    log("目标窗口：" + win.describe())
    if not args.no_activate:
        ok = force_foreground(win.hwnd)
        log(f"切前台：{'成功' if ok else '失败'}")
        if not ok:
            log("提示：若切前台失败，请手动点一下游戏窗口后重试。")
        time.sleep(0.6)
        win = resolve_window(cfg)
        log("切前台后客户区：" + win.describe())

    if args.click_xy:
        fx, fy = (float(v) for v in args.click_xy.split(","))
        ctrl = InputController(background=args.background)
        log(f"调试单击：fx={fx} fy={fy} "
            f"屏幕坐标=({int(win.client_rect[0] + fx * win.width)},"
            f"{int(win.client_rect[1] + fy * win.height)})")
        ctrl.click(win, fx, fy)
        time.sleep(0.4)

    if args.samples > 0:
        out_dir = Path(args.out_dir) if args.out_dir else DEBUG_DIR / "samples"
        out_dir.mkdir(parents=True, exist_ok=True)
        cap = Capturer(args.capture)
        try:
            for i in range(args.samples):
                win = resolve_window(cfg)
                frame = cap.grab(win)
                path = out_dir / f"sample_{i:03d}.png"
                cv2.imwrite(str(path), frame)
                log(f"样本 {i + 1}/{args.samples} -> {path} 平均亮度={float(frame.mean()):.1f}")
                time.sleep(args.sample_interval)
        finally:
            cap.close()
        return 0

    cap = Capturer(args.capture)
    try:
        frame = cap.grab(win)
    finally:
        cap.close()

    out_dir = Path(args.out_dir) if args.out_dir else DEBUG_DIR
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "probe.png"
    cv2.imwrite(str(out_path), frame)
    mean = float(frame.mean())
    log(f"已保存抓帧：{out_path} 尺寸={frame.shape[1]}x{frame.shape[0]} 平均亮度={mean:.1f}")
    if mean < 5:
        log("警告：画面几乎全黑，可能被防截屏，请尝试 --capture printwindow。")
    return 0


def run_image(args) -> int:
    """离线识别：对已有截图跑识别流程，用于验证/调参。"""
    import glob as glob_module

    cfg = Config.load(Path(args.config), {"k": args.k} if args.k else None)
    paths = sorted(glob_module.glob(args.image, recursive=True))
    if not paths:
        log(f"--image 未匹配到文件：{args.image}")
        return 2
    out_dir = Path(args.out_dir) if args.out_dir else DEBUG_DIR / "annotated"
    out_dir.mkdir(parents=True, exist_ok=True)
    vision = Vision(cfg)
    ok_count = 0
    for path in paths:
        frame = cv2.imread(path)
        if frame is None:
            log(f"跳过无法读取的文件：{path}")
            continue
        piece, target, _pm, _bm = vision.detect(frame)
        hold_ms = None
        if piece is not None and target is not None:
            dist, _cross = press_distance(piece, target, cfg)
            hold_ms = dist * cfg.k + cfg.b
            ok_count += 1
        vis = annotate(frame, piece, target, hold_ms, extra=Path(path).name)
        out_path = out_dir / (Path(path).stem + "_annotated.png")
        cv2.imwrite(str(out_path), vis)
        if piece is None or target is None:
            log(f"{Path(path).name}: 识别失败 piece={piece} target={target} -> {out_path}")
        else:
            dist, _cross = press_distance(piece, target, cfg)
            log(
                f"{Path(path).name}: piece=({piece.x:.1f},{piece.y:.1f}) "
                f"target=({target.x:.1f},{target.y:.1f}) d={dist:.1f}px "
                f"hold={hold_ms:.0f}ms -> {out_path}"
            )
    log(f"离线识别完成：{ok_count}/{len(paths)} 张成功")
    return 0


def dry_run(args) -> int:
    """实机抓帧 + 识别 + 标注，不发送任何输入。"""
    cfg = Config.load(
        Path(args.config),
        {"k": args.k, "gamma": args.gamma},
    )
    win = resolve_window(cfg)
    log("目标窗口：" + win.describe())
    if not args.no_activate:
        ok = force_foreground(win.hwnd)
        log(f"切前台：{'成功' if ok else '失败'}")
        time.sleep(0.4)
        win = resolve_window(cfg)

    out_dir = Path(args.out_dir) if args.out_dir else DEBUG_DIR / "dryrun"
    out_dir.mkdir(parents=True, exist_ok=True)
    vision = Vision(cfg)
    cap = Capturer(args.capture)
    count = max(args.samples, 1)
    try:
        for i in range(count):
            frame = cap.grab(win)
            piece, target, _pm, _bm = vision.detect(frame)
            hold_ms = None
            if piece is not None and target is not None:
                dist, _cross = press_distance(piece, target, cfg)
                hold_ms = dist * cfg.k + cfg.b
            vis = annotate(frame, piece, target, hold_ms, extra=f"dry-run #{i + 1}")
            out_path = out_dir / f"dry_{i:03d}.png"
            cv2.imwrite(str(out_path), vis)
            if piece is None or target is None:
                log(f"#{i + 1} 识别失败 piece={piece} target={target} -> {out_path}")
            else:
                dist, _cross = press_distance(piece, target, cfg)
                log(
                    f"#{i + 1} piece=({piece.x:.1f},{piece.y:.1f}) "
                    f"target=({target.x:.1f},{target.y:.1f}) d={dist:.1f}px "
                    f"hold={hold_ms:.0f}ms -> {out_path}"
                )
            if i < count - 1:
                time.sleep(args.sample_interval)
    finally:
        cap.close()
    return 0


def jump_once(args) -> int:
    """联调：按指定时长按住一次，然后连拍观察落点与镜头变化。"""
    cfg = Config.load(Path(args.config), {"k": args.k, "gamma": args.gamma})
    win = resolve_window(cfg)
    log("目标窗口：" + win.describe())
    ok = force_foreground(win.hwnd)
    log(f"切前台：{'成功' if ok else '失败'}")
    time.sleep(0.4)
    win = resolve_window(cfg)

    out_dir = Path(args.out_dir) if args.out_dir else DEBUG_DIR / "jumponce"
    out_dir.mkdir(parents=True, exist_ok=True)
    cap = Capturer(args.capture)
    vision = Vision(cfg)
    ctrl = InputController(background=args.background)
    try:
        win = ensure_playing(cap, cfg, vision, ctrl, foreground=not args.background)
        if win is None:
            log("无法进入可玩状态：请确认游戏窗口可见且没有被遮挡。")
            return 3
        frame = cap.grab(win)
        piece, target, _pm, _bm = vision.detect(frame)
        if piece is None or target is None:
            log(f"跳跃前识别失败 piece={piece} target={target}")
            return 2
        dist, _cross = press_distance(piece, target, cfg)
        hold_ms = args.hold_ms if args.hold_ms > 0 else dist * cfg.k + cfg.b
        log(
            f"跳跃前 piece=({piece.x:.1f},{piece.y:.1f}) target=({target.x:.1f},{target.y:.1f}) "
            f"d(沿轴)={dist:.1f}px -> 按压 {hold_ms:.0f}ms (后台={args.background})"
        )
        cv2.imwrite(str(out_dir / "before.png"), annotate(frame, piece, target, hold_ms, "before"))
        if not args.dry_run:
            ctrl.press(win, cfg.press_point[0], cfg.press_point[1], hold_ms / 1000.0)
            log("已发送按住/松开")
        burst = max(args.burst, 1)
        for i in range(burst):
            time.sleep(args.burst_interval)
            win = resolve_window(cfg)
            frame = cap.grab(win)
            piece2, target2, _pm2, _bm2 = vision.detect(frame)
            vis = annotate(frame, piece2, target2, None, f"after #{i + 1}")
            path = out_dir / f"after_{i:02d}.png"
            cv2.imwrite(str(path), vis)
            desc = "识别失败"
            if piece2 is not None:
                desc = f"piece=({piece2.x:.1f},{piece2.y:.1f})"
                if target2 is not None:
                    desc += f" target=({target2.x:.1f},{target2.y:.1f})"
            log(f"连拍 {i + 1}/{burst}: {desc} -> {path}")
    finally:
        cap.close()
    return 0


# --------------------------------------------------------------------------------------
# 主循环
# --------------------------------------------------------------------------------------


@dataclass
class JumpRecord:
    index: int
    timestamp: str
    k_before: float
    dist_px: float
    hold_ms: float
    k_after: float
    error_px: float | None
    result: str
    score: int | None
    note: str = ""
    # 分阶段耗时（毫秒，仅用于性能分析；老日志没有这些列，工具会自动跳过）
    t_acquire_ms: float = 0.0
    t_landing_ms: float = 0.0
    t_score_ms: float = 0.0
    t_loop_ms: float = 0.0
    # error_px 的来源：mask=方块掩码估计（历史口径，噪声 ±28px 且符号会反）；
    # flow=相位相关法；空=没测到误差。**注意：这里不再写 dot**——
    # 2026-10-01 13:10 实测发现"落地帧中心白点"当不了真值：35 个 dot 样本里，
    # 12 行测量误差 >40px 的跳被游戏判为"命中中心"（落地时棋子正好把白点挡住，
    # 检测器抓到的多是别的高光/边界）。所以白点值单独放 err_dot_px，不覆盖 error_px，
    # 保持 error_px 与全部历史日志同口径。
    err_src: str = ""
    err_dot_px: float | None = None   # 落地帧中心白点量出来的误差（**仅供参考，非真值**）
    # 本跳瞄点是否用了游戏中心白点（1=用了，误差≈0）。用它能把"瞄点 A/B"
    # 限定在"瞄点没用白点"的那批帧上，那才是改动真正生效的人群。
    aim_dot: int = 0
    # --bias-sweep 用的档位（百分比）：本跳把沿轴距离乘了 (1+bias_pct/100)。
    # 0 表示未启用扫描。
    bias_pct: float = 0.0


def _small_gray(frame: np.ndarray, cfg: Config) -> np.ndarray:
    h, w = frame.shape[:2]
    top = int(round(cfg.top_bar_ratio * h))
    gray = cv2.cvtColor(frame[top:, :], cv2.COLOR_BGR2GRAY)
    return cv2.resize(gray, (96, 176), interpolation=cv2.INTER_AREA)


def frame_diff(prev: np.ndarray | None, cur: np.ndarray) -> float:
    if prev is None:
        return 999.0
    return float(np.abs(prev.astype(np.int16) - cur.astype(np.int16)).mean())


_STOP_REQUESTED = False
_LAST_STOP_REASON = ""      # play() 结束时写入，供 --daemon 判断"这轮该不该续跑"


def poll_stop_key() -> bool:
    """轮询 F8。一旦按下就锁存，避免主循环 2 秒一次轮询漏掉短按。"""
    global _STOP_REQUESTED
    if user32.GetAsyncKeyState(VK_F8) & 0x8000:
        _STOP_REQUESTED = True
    if _CONTROL is not None:
        if _STOP_REQUESTED:
            _CONTROL.stop("用户停止（F8 / q / Esc）")
        return _CONTROL.stopped
    return _STOP_REQUESTED


def f8_pressed() -> bool:
    return poll_stop_key()


# 在跑脚本的那个命令行窗口里，按这些键即可停止（与 F8 等价）。
CONSOLE_STOP_KEYS = ("q", "Q", "\x1b")     # q / Q / Esc


def _console_key_watcher() -> None:
    """后台读控制台按键：q / Q / Esc → 与 F8 走同一条停止路径。

    为什么需要它：F8 是全局热键（GetAsyncKeyState），但脚本常常在后台/隐藏控制台里跑，
    或者用户更习惯在命令行窗口里按键。这里用 msvcrt 读**本进程控制台**的按键，
    不抢全局键盘事件；没有控制台（stdin 被重定向、打包后无窗口）时静默退出。
    """
    global _STOP_REQUESTED
    try:
        import msvcrt
    except ImportError:            # 非 Windows
        return
    try:
        while not _STOP_REQUESTED:
            if msvcrt.kbhit():
                try:
                    ch = msvcrt.getwch()
                except Exception:
                    return
                if ch in CONSOLE_STOP_KEYS:
                    _STOP_REQUESTED = True
                    if _CONTROL is not None:
                        _CONTROL.stop("用户按控制台停止键")
                    key = {"\x1b": "Esc"}.get(ch, ch)
                    log(f"收到控制台停止键 {key!r}，正在停止（松开鼠标后退出；等价于 F8）…")
                    return
                if ch in ("p", "P") and _CONTROL is not None:
                    _CONTROL.toggle("console P")
            time.sleep(0.05)
    except Exception:
        # 没有真正的控制台时 kbhit() 会失败——这不是错误，静默退出即可
        return


def start_console_watcher() -> None:
    """启动控制台按键监听（daemon 线程，进程退出即消失）。"""
    try:
        thread = threading.Thread(target=_console_key_watcher,
                                  name="console-stop-key", daemon=True)
        thread.start()
    except Exception as exc:
        log(f"控制台按键监听未启动（不影响 F8）：{exc}")


def control_check(generation=None) -> None:
    if _CONTROL is not None:
        poll_stop_key()
        _CONTROL.check(generation)


def cooperative_sleep(seconds: float, draining: bool = False) -> None:
    """Interrupt waits, but never shorten an owned mouse hold for a pause."""
    if _CONTROL is None:
        time.sleep(seconds)
        return
    deadline = time.monotonic() + max(0.0, seconds)
    while time.monotonic() < deadline:
        if draining:
            if poll_stop_key():
                raise ControlInterrupted("用户停止")
        else:
            control_check()
        time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))


def active_time() -> float:
    return _CONTROL.active_time() if _CONTROL is not None else time.monotonic()


def control_event(**fields) -> None:
    """Separate control events: never add non-jump rows to the jump CSV."""
    try:
        DEBUG_DIR.mkdir(parents=True, exist_ok=True)
        fields["timestamp"] = time.time()
        with (DEBUG_DIR / f"control_{os.getpid()}.jsonl").open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(fields, ensure_ascii=False) + "\n")
    except OSError:
        pass


def control_ready_window(cfg: Config, initial: bool = False,
                         timeout_s: float | None = None) -> WindowInfo:
    """Wait without input, acknowledge pause, then activate/count down freshly."""
    if _CONTROL is None:
        return resolve_window(cfg)
    control = _CONTROL
    timeout = cfg.startup_wait_s if timeout_s is None else timeout_s
    wait_start = active_time()
    deadline = wait_start + timeout if timeout > 0 else None
    generation = control.generation
    countdown = None
    countdown_hwnd = None
    next_report = 0.0
    log(f"等待你手动打开微信「跳一跳」；{control.hotkey} 暂停/恢复，F8 / q / Esc 停止")
    while True:
        if poll_stop_key():
            raise ControlInterrupted(control.stop_reason)
        if control.pause_requested:
            new_pause = control.paused_at is None
            control.acknowledge_pause()
            if new_pause or time.monotonic() >= next_report:
                status_write(state="paused", alive=True, stop_reason=f"已暂停；{control.hotkey} / 控制台 P 恢复",
                             countdown_s=None, stuck_s=0)
                next_report = time.monotonic() + 1.0
            if new_pause and control.paused_at is not None:
                log(f"已暂停，鼠标已释放；可以做其他事情。{control.hotkey} / 控制台 P 恢复，F8 停止")
            control.changed.wait(0.05)
            control.changed.clear()
            countdown = None
            continue
        if generation != control.generation:
            generation = control.generation
            countdown = None
        candidates = find_windows(cfg.window_title_pattern, cfg.window_exe_hint)
        if not candidates:
            countdown = None
            if deadline is not None and active_time() >= deadline:
                raise WindowGoneError("等待用户打开「跳一跳」超时")
            if time.monotonic() >= next_report:
                status_write(state="waiting_window", alive=True, countdown_s=None,
                             game_state=None, waited_s=int(active_time() - wait_start),
                             stop_reason="等待用户手动打开小游戏", startup_wait=initial)
                next_report = time.monotonic() + 1.0
            time.sleep(0.05)
            continue
        win = candidates[0]
        if countdown is None or countdown_hwnd != win.hwnd:
            try:
                control.check(generation)
                win = ensure_window_size(win, cfg)
                if not force_foreground(win.hwnd):
                    status_write(state="waiting_window", alive=True,
                                 stop_reason="游戏窗口不能切回前台，等待重试")
                    time.sleep(0.1)
                    continue
            except ControlInterrupted:
                continue
            countdown_hwnd = win.hwnd
            countdown = time.monotonic() + cfg.start_delay_s
            log(f"游戏窗口已就绪：{cfg.start_delay_s:g} 秒后开跑（{control.hotkey} 暂停，F8 取消）")
        if foreground_hwnd() != win.hwnd:
            countdown = None
            time.sleep(0.05)
            continue
        remaining = max(0.0, countdown - time.monotonic())
        status_write(state="countdown" if initial else "resuming", alive=True,
                     countdown_s=round(remaining, 1), stop_reason="倒计时后重新识别画面",
                     window={"hwnd": win.hwnd, "pid": win.pid, "title": win.title,
                             "client": list(win.client_rect)})
        if remaining <= 0:
            try:
                control.finish_resume(generation)
                return win
            except ControlInterrupted:
                countdown = None
                continue
        time.sleep(0.05)


def control_checkpoint(cfg: Config, vision: Vision | None = None) -> bool:
    if _CONTROL is None:
        return False
    if poll_stop_key():
        raise ControlInterrupted(_CONTROL.stop_reason)
    if _CONTROL.needs_resume:
        control_ready_window(cfg)
        if vision is not None:
            vision.reset_prior()
        return True
    return False


def hud_center_hit(record) -> int | None:
    """Only the recorded HUD decision is hit truth; geometry is diagnostic."""
    if record.result == "landed":
        if "[center x" in record.note:
            return 1
        if "[miss]" in record.note:
            return 0
    return None


def accuracy_fields(records) -> dict:
    def count(rows):
        landed = sum(r.result == "landed" for r in rows)
        labels = [hud_center_hit(r) for r in rows]
        hit, miss = labels.count(1), labels.count(0)
        attempts = sum(r.result != "stopped" for r in rows)
        dead = sum(r.result in ("dead", "died") for r in rows)
        return dict(attempts=attempts, landed=landed, hits=hit, misses=miss,
                    unknown=max(0, attempts - hit - miss - dead), judged=hit + miss,
                    hit_rate=100.0 * hit / (hit + miss) if hit + miss else None,
                    landing_rate=100.0 * landed / attempts if attempts else None,
                    judgement_coverage=100.0 * (hit + miss) / attempts if attempts else None)
    result = count(records)
    result["recent100"] = count(list(records)[-100:])
    return result


def clamp(value: float, low: float, high: float) -> float:
    return max(low, min(high, value))


def measure_landing_error(
    frame: np.ndarray,
    block_mask: np.ndarray,
    piece: Piece,
    cfg: Config,
    direction: tuple,
    expected_width: float | None = None,
) -> tuple | None:
    """落地误差：棋子锚点相对脚下方块顶面中心在跳跃方向上的偏移（像素，正=超冲）。

    做法：先取棋子底边下方一点作为“顶面颜色”参考，用它过滤掉同样是暗色但颜色不同的
    棋子阴影；再在底边下方一小段行区间里找包含棋子 x 的最宽顶面连续段，该段中点即方块
    中心 x。
    """
    h, w = block_mask.shape[:2]
    x_a = float(piece.x)
    foot = int(round(piece.y))

    # 参考色：棋子脚下若干采样点里“最亮”的那个。棋子阴影是顶面的半透明变暗，
    # 永远比顶面暗，所以取最亮样本即可避开阴影，得到真正的顶面色。
    samples = []
    for dy in (4, 8, 12, 16, 20, 26):
        yy = foot + dy
        if yy >= h:
            break
        for dx in (-6, -2, 2, 6):
            xx = int(round(x_a)) + dx
            if not (0 <= xx < w) or block_mask[yy, xx] == 0:
                continue
            y0, y1 = max(0, yy - 1), min(h, yy + 2)
            x0, x1 = max(0, xx - 1), min(w, xx + 2)
            patch = frame[y0:y1, x0:x1].reshape(-1, 3).astype(np.float32)
            samples.append((float(patch.mean()), np.median(patch, axis=0)))
    if not samples:
        return None
    samples.sort(key=lambda s: s[0], reverse=True)
    reference = np.median(np.stack([s[1] for s in samples[:3]]), axis=0)

    # 期望的顶面宽度：优先用“跳跃前精确测得的那个目标方块”的宽度（就是脚下这块）。
    # 有了它就能把搜索限制在合理窗口内，并且可以把被棋子挡断的左右两段拼起来求中心。
    if expected_width and expected_width > 40:
        # 小方块（药瓶/礼盒）也要留足窗口，否则会频繁测不出误差
        half_window = max(0.62 * expected_width, 0.25 * w)
    else:
        half_window = 0.45 * w

    def search(mask: np.ndarray):
        found = None
        for dy in range(2, 34, 2):
            row = int(round(piece.y + dy))
            if row < 0 or row >= h:
                break
            cols = np.flatnonzero(mask[row])
            if cols.size == 0:
                continue
            # 只取“包含棋子 x”的那一段连续顶面（被棋子挡断时取棋子这一侧），
            # 并用期望宽度限制窗口，避免误取到远处别的方块。
            split = np.where(np.diff(cols) > 1)[0]
            for seg in np.split(cols, split + 1):
                span = float(seg[-1] - seg[0] + 1)
                if span < 20:
                    continue
                if not (seg[0] - 12 <= x_a <= seg[-1] + 12):
                    continue
                if seg[0] < x_a - half_window or seg[-1] > x_a + half_window:
                    continue
                if found is None or span > found[0]:
                    found = (span, (seg[0] + seg[-1]) / 2.0)
        return found

    best = None
    if cfg.landing_method != "block":
        frame_i = frame.astype(np.int16)
        color_diff = np.abs(frame_i - reference.reshape(1, 1, 3)).sum(axis=2)
        same_color = (color_diff <= cfg.landing_color_tol).astype(np.uint8) * 255
        color_best = search(cv2.bitwise_and(block_mask, same_color))
    else:
        color_best = None
    block_best = search(block_mask)
    # 方块掩码法经真实得分验证更可靠（颜色法会被顶面渐变/高光带偏），优先用它；
    # 只有它测不出来时才退回颜色法。
    best = block_best if block_best is not None else color_best
    if best is None:
        return None
    width, center_x = best
    ux = direction[0] / max(math.hypot(*direction), 1e-6)
    if abs(ux) < 0.5:
        return None
    error = (x_a - center_x) / ux
    # 第三个返回值：估计出来的落点方块中心 x。调用方可以据此再找"中心白点"，
    # 把误差换成真值（见主循环里的 dot 校正）。
    return error, width, center_x


def wait_landing(cap: Capturer, win: WindowInfo, vision: Vision, cfg: Config,
                 want_target: bool = True):
    """等待落地：连续若干帧画面稳定、棋子位置不动，且棋子脚下确实踩着方块。

    只看整帧平均差异会把“棋子正在坠落”误判成静止（棋子只占极小的像素比例），
    所以额外要求棋子锚点稳定，并且锚点下方能采到方块。

    want_target=False 时不做 find_target（省 16.5ms/跳）：确认轮次只关心棋子锚点与
    脚下掩码，返回的 target 仅供 --save-debug 标注，不参与任何判定。
    """
    last_small = None
    stable = 0
    frame = None
    piece = target = None
    block_mask = None
    last_anchor = None
    deadline = time.time() + cfg.settle_timeout_s
    poll_i = 0
    while time.time() < deadline:
        if poll_stop_key():
            return "stopped", frame, piece, target, block_mask
        # 窗口解析（EnumWindows + 查进程名）约 0.47ms，而这一环每跳要轮询 20~30 次。
        # 窗口在跳跃过程中基本不动，改成每 5 次解析一次；万一窗口真的移动了，
        # 抓帧区域会偏，识别失败 → 下一轮立即重新解析（自愈，不会累积错误）。
        if win is None or poll_i % 5 == 0:
            if _CONTROL is not None:
                try:
                    win = resolve_window(cfg, 0 if _CONTROL.pause_requested else cfg.window_wait_s,
                                         draining=True)
                except WindowGoneError:
                    if _CONTROL.pause_requested:
                        return "pause_unknown", None, None, None, None
                    raise
            else:
                win = resolve_window(cfg, cfg.window_wait_s)
        poll_i += 1
        if (_CONTROL is not None and _CONTROL.pause_requested
                and foreground_hwnd() != win.hwnd):
            return "pause_unknown", None, None, None, None
        frame = cap.grab(win)
        small = _small_gray(frame, cfg)
        diff = frame_diff(last_small, small)
        last_small = small
        if diff >= cfg.settle_diff_threshold:
            # 画面还在动（棋子飞行 / 镜头推移）。这一帧的识别结果按定义不会被采用
            # （settle 要求 diff < 阈值 且锚点稳定），所以直接跳过最贵的 detect()
            # ——实测 detect() = 46.8ms，而这一步原来是每 50ms 跑一次。
            stable = 0
            last_anchor = None
            time.sleep(cfg.settle_poll_ms / 1000.0)
            continue
        piece, _pm, block_mask = vision.detect_piece_only(frame)
        anchor = None
        if piece is not None:
            anchor = (piece.x, piece.y)
        anchor_stable = (
            anchor is not None
            and last_anchor is not None
            and abs(anchor[0] - last_anchor[0]) <= cfg.settle_anchor_tol_px
            and abs(anchor[1] - last_anchor[1]) <= cfg.settle_anchor_tol_px
        )
        last_anchor = anchor
        on_block = piece is not None and _foot_on_block(block_mask, piece, cfg)
        if piece is not None and diff < cfg.settle_diff_threshold and anchor_stable and on_block:
            stable += 1
        else:
            stable = 0
        if stable >= cfg.settle_frames:
            # 只在真正要返回时才补算目标（同一帧、同一掩码、同一 piece，
            # 与原先"每轮都 detect"得到的 target 完全一致）。
            if want_target and piece is not None:
                target = vision.find_target(frame, block_mask, piece, _pm)
            return "landed", frame, piece, target, block_mask
        time.sleep(cfg.settle_poll_ms / 1000.0)
    # 超时也要保证返回的 piece/block_mask 与最后这一帧同步（否则调用方的
    # "分数增长=已落地"裁决会因为 piece=None 而失效）
    if frame is not None and piece is None:
        piece, target, _pm, block_mask = vision.detect(frame)
    return "timeout", frame, piece, target, block_mask


def measure_landing_error_flow(
    before_frame: np.ndarray,
    after_frame: np.ndarray,
    target: Block,
    piece: Piece,
    cfg: Config,
    direction: tuple,
) -> tuple | None:
    """用相位相关估计镜头平移，再比较“理想落点”和“实际落点”。

    跳跃前目标方块顶面中心是 T；落地后镜头平移了 c，理想落点变成 T+c（棋子锚点比顶面
    中心高出 anchor_dy）。实际锚点与理想锚点的差在跳跃方向上的投影就是落点误差。
    """
    if before_frame is None or after_frame is None:
        return None
    g1 = cv2.cvtColor(before_frame, cv2.COLOR_BGR2GRAY).astype(np.float32)
    g2 = cv2.cvtColor(after_frame, cv2.COLOR_BGR2GRAY).astype(np.float32)
    win = cv2.createHanningWindow((g1.shape[1], g1.shape[0]), cv2.CV_32F)
    (dx, dy), response = cv2.phaseCorrelate(g1, g2, win)
    if response < cfg.flow_min_response:
        return None
    anchor_dy = cfg.piece_anchor_dy_ratio * after_frame.shape[0]
    expected_x = target.x + dx
    expected_y = target.y + dy - anchor_dy
    ex = piece.x - expected_x
    ey = piece.y - expected_y
    norm = math.hypot(*direction)
    if norm < 1e-6:
        return None
    ux, uy = direction[0] / norm, direction[1] / norm
    error = ex * ux + ey * uy
    # 用目标方块宽度做量纲参考，返回 (误差, 可信半径)
    ref_width = max(40.0, target.bbox[2])
    return error, ref_width


def _foot_on_block(block_mask: np.ndarray, piece: Piece, cfg: Config) -> bool:
    """棋子脚下是否有方块（用于排除“正在坠落”的帧）。"""
    h, w = block_mask.shape[:2]
    x = int(round(piece.x))
    hits = 0
    for dy in (6, 10, 14, 18, 24):
        y = int(round(piece.y)) + dy
        if not (0 <= y < h):
            continue
        x0, x1 = max(0, x - 4), min(w, x + 5)
        if block_mask[y, x0:x1].any():
            hits += 1
    return hits >= 2


class ScreenState:
    """界面状态：只有 PLAYING 才允许发送按键。"""

    PLAYING = "playing"
    START = "start"
    GAME_OVER = "gameover"
    UNKNOWN = "unknown"


def classify_screen(frame: np.ndarray) -> tuple:
    """返回 (状态, 按钮中心, 相似度)。只有识别到明确的界面按钮才返回 START/GAME_OVER。"""
    ui = match_ui(frame)
    if ui is None:
        return ScreenState.UNKNOWN, None, 0.0
    name, center, score = ui
    state = ScreenState.START if name == "start" else ScreenState.GAME_OVER
    return state, center, score


def is_blocking_modal(frame: np.ndarray, cfg: Config) -> bool:
    """检测“获得新皮肤/分享”这类挡住游戏的深色弹窗。

    判据用“暗像素占比”而不是平均亮度：结算页中央也有深色卡片（但周围是亮的），
    用平均亮度会误判；这类弹窗是整个中央面板都暗。
    """
    h, w = frame.shape[:2]
    y0, y1 = int(0.25 * h), int(0.65 * h)
    x0, x1 = int(0.15 * w), int(0.85 * w)
    roi = frame[y0:y1, x0:x1]
    if roi.size == 0:
        return False
    gray = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)
    dark_ratio = float((gray < cfg.modal_dark_threshold).mean())
    return dark_ratio > 0.85


def acquire_frame(cap: Capturer, cfg: Config, vision: Vision, ctrl: InputController,
                  foreground: bool, state: dict | None = None,
                  attempts: int | None = None,
                  verbose: bool = True):
    """抓帧直到得到“可玩且识别连续两帧一致”的结果。

    安全约定：只有匹配到「开始游戏 / 再玩一局」按钮时才点击；其余不稳定状态一律只等待
    重识别，绝不按兜底坐标盲点（盲点是送死的主要来源）。
    返回 (win, frame, piece, target, block_mask) 或 None。
    """
    limit = attempts or cfg.detect_retry_limit
    prev_center = None
    last_frame = None
    fail_note = "未知"
    for attempt in range(limit):
        if poll_stop_key():
            return None
        control_check()
        win = resolve_window(cfg, cfg.window_wait_s)
        if foreground and foreground_hwnd() != win.hwnd:
            force_foreground(win.hwnd)
        frame = cap.grab(win)
        last_frame = frame
        # 顺序很重要：先判开始/结算界面（它们的中央也可能是深色卡片），
        # 再判“获得新皮肤”这类深色遮挡弹窗。
        screen_state, button_center, score = classify_screen(frame)
        if screen_state in (ScreenState.START, ScreenState.GAME_OVER):
            status_write(game_state=("start_page" if screen_state == ScreenState.START
                                     else "game_over"), state="acquiring")
            if verbose:
                log(f"界面状态={screen_state}（相似度 {score:.2f}），点击继续")
            ctrl.click(win, button_center[0] / win.width, button_center[1] / win.height)
            if state is not None:
                state["fresh"] = True
            prev_center = None
            cooperative_sleep(1.4)
            continue
        if is_blocking_modal(frame, cfg):
            status_write(game_state="blocked_modal", state="acquiring")
            if verbose:
                log("检测到遮挡弹窗（深色面板），点击返回键关闭")
            ctrl.click(win, cfg.modal_dismiss_point[0], cfg.modal_dismiss_point[1])
            prev_center = None
            cooperative_sleep(1.0)
            continue

        piece, target, _piece_mask, block_mask = vision.detect(frame)
        if piece is None or target is None:
            fail_note = f"piece={'有' if piece is not None else '无'} target={'有' if target is not None else '无'}"
            if attempt % 5 == 0:
                status_write(state="acquiring", acquire_attempt=attempt + 1,
                             acquire_limit=limit, stop_reason=f"识别中（{attempt + 1}/{limit}）")
            if verbose and attempt and attempt % 10 == 0:
                log(f"识别仍失败（已重试 {attempt} 次，{fail_note}），继续等待…")
            prev_center = None
            vision.reset_prior()   # 连续失败时清掉先验，避免一直锁定在同一个错误位置
            cooperative_sleep(0.30)
            continue

        center = (target.x, target.y)
        if (prev_center is not None
                and abs(center[0] - prev_center[0]) <= cfg.target_consistency_px
                and abs(center[1] - prev_center[1]) <= cfg.target_consistency_px):
            return win, frame, piece, target, block_mask
        # 首帧只作为一致性基准，再抓一帧确认
        prev_center = center
        cooperative_sleep(cfg.acquire_poll_ms / 1000.0)
    # 重试用尽：把现场帧存下来，便于事后定位是哪种场景识别不了
    if last_frame is not None:
        try:
            DEBUG_DIR.mkdir(parents=True, exist_ok=True)
            path = DEBUG_DIR / f"fail_{time.strftime('%Y%m%d_%H%M%S')}.png"
            cv2.imwrite(str(path), last_frame)
            # 同帧复核：用全新 Vision（无位置先验）再识别一次，用来区分
            #   “这一帧本身识别不了” 与 “运行时状态（先验等）把识别带偏了”
            probe = Vision(cfg)
            fresh_piece, fresh_target, _p1, _p2 = probe.detect(last_frame)
            detail = (f"复核: piece={'无' if fresh_piece is None else (round(fresh_piece.x), int(fresh_piece.y))} "
                      f"target={'无' if fresh_target is None else (round(fresh_target.x), round(fresh_target.y))}")
            log(f"识别连续失败（{limit} 次，{fail_note}），现场帧已存：{path}")
            log(detail)
        except Exception as exc:
            log(f"保存失败帧出错：{exc}")
    return None


def ensure_playing(cap: Capturer, cfg: Config, vision: Vision, ctrl: InputController,
                   foreground: bool, verbose: bool = True,
                   state: dict | None = None,
                   allow_fallback_click: bool = True) -> WindowInfo | None:
    """确保游戏处于可玩状态；必要时点击“再玩一局 / 开始游戏”。

    state["fresh"] 会被置为 True 表示“本局是从新局开始的”（分数从 0 起算）。

    兜底点击（allow_fallback_click）只在**连续多帧都认不出棋子**时才用，而且只点
    `restart_point`（结算页的“再玩一局”按钮位置）。绝不点画面中央——那一带在游戏中
    就是一次极短跳跃，会直接把棋子送下去。
    """
    blind_attempts = 0
    for attempt in range(8):
        control_check()
        win = resolve_window(cfg)
        if foreground and foreground_hwnd() != win.hwnd:
            force_foreground(win.hwnd)
        frame = cap.grab(win)
        ui = match_ui(frame)
        if ui is not None:
            name, (cx, cy), score = ui
            if verbose:
                log(f"检测到界面按钮 {name}（相似度 {score:.2f}），点击 {cx:.0f},{cy:.0f}")
            ctrl.click(win, cx / win.width, cy / win.height)
            cooperative_sleep(1.4)
            if state is not None:
                state["fresh"] = True
            continue
        piece, target, _pm, _bm = vision.detect(frame)
        if piece is not None and target is not None:
            blind_attempts = 0
            if verbose and attempt:
                log(f"已进入可玩状态（第 {attempt + 1} 次尝试）")
            return win
        if not allow_fallback_click:
            cooperative_sleep(0.4)
            continue
        if piece is not None:
            # 棋子认出来了（说明可能还在游戏中），只等不点。
            cooperative_sleep(0.5)
            continue
        blind_attempts += 1
        if blind_attempts < 2:
            cooperative_sleep(0.5)
            continue
        if verbose:
            log(f"连续 {blind_attempts} 帧既无棋子也无目标，按“再玩一局”兜底坐标点击一次")
        ctrl.click(win, cfg.restart_point[0], cfg.restart_point[1])
        cooperative_sleep(1.3)
    return None


def harvest_score(args) -> int:
    """从 debug/score 的样本自动挖掘 0-9 数字模板。

    文件名：jNNN_sXXXX.png（NNN=跳跃序号，XXXX=当时的“预测分数”）。预测分数的绝对值
    可能整体偏移（例如从半局开始跑），但同一段连续生命里两步之间的增量是准的。因此这里
    对每一段（预测分数严格递增的连续样本）搜索一个起始分，使得：样本的字形个数等于该分数
    的位数、且同一种字形始终对应同一个数字。冲突最少的起始分就是答案。
    """
    score_dir = Path(args.score_dir) if args.score_dir else DEBUG_DIR / "score"
    files = sorted(score_dir.glob("*.png"))
    if not files:
        log(f"没有找到分数样本：{score_dir}（先跑一次 --save-debug 采集）")
        return 2

    samples: list = []
    for path in files:
        match = re.search(r"j(\d+)_s(\d+)\.png$", path.name)
        if not match:
            continue
        roi = cv2.imread(str(path))
        if roi is None:
            continue
        glyphs = segment_glyphs(roi)
        samples.append({
            "idx": int(match.group(1)),
            "pred": int(match.group(2)),
            "path": path,
            "glyphs": glyphs,
        })
    if not samples:
        log("样本文件名不符合 jNNN_sXXXX.png 规则，无法挖掘。")
        return 2

    # 字形聚类（同一数字在不同样本里应聚成一类）
    clusters: list = []
    for sample in samples:
        sample["cluster_ids"] = []
        for glyph in sample["glyphs"]:
            for index, cluster in enumerate(clusters):
                if glyph_similarity(glyph, cluster["example"]) > 0.95:
                    cluster["count"] += 1
                    sample["cluster_ids"].append(index)
                    break
            else:
                clusters.append({"example": glyph, "count": 1})
                sample["cluster_ids"].append(len(clusters) - 1)

    # 分段：预测分数严格递增的连续样本属于同一条命
    segments: list = []
    current: list = []
    for sample in samples:
        if current and (sample["pred"] <= current[-1]["pred"] or sample["idx"] <= current[-1]["idx"]):
            segments.append(current)
            current = []
        current.append(sample)
    if current:
        segments.append(current)

    def evaluate(segment: list, base: int) -> tuple:
        score = base
        previous_pred = None
        mapping: dict = {}
        conflicts = 0
        for sample in segment:
            if previous_pred is not None:
                score += sample["pred"] - previous_pred
            previous_pred = sample["pred"]
            text = str(score)
            ids = sample["cluster_ids"]
            if len(text) != len(ids):
                conflicts += 3
                continue
            for cluster_id, digit in zip(ids, text):
                known = mapping.get(cluster_id)
                if known is None:
                    mapping[cluster_id] = digit
                elif known != digit:
                    conflicts += 1
        return conflicts, mapping

    votes: dict = {}   # cluster_id -> {digit: 票数}
    for segment in segments:
        best = None
        for base in range(0, 300):
            conflicts, mapping = evaluate(segment, base)
            if best is None or conflicts < best[0]:
                best = (conflicts, base, mapping)
        if best is None:
            continue
        conflicts, base, mapping = best
        log(f"样本段（{len(segment)} 条）：起始分 {base}，冲突 {conflicts}")
        for cluster_id, digit in mapping.items():
            votes.setdefault(cluster_id, {})
            votes[cluster_id][digit] = votes[cluster_id].get(digit, 0) + 1

    TEMPLATE_DIR.mkdir(parents=True, exist_ok=True)
    written = {}
    for cluster_id, counter in votes.items():
        digit, count = max(counter.items(), key=lambda kv: kv[1])
        if sum(counter.values()) > count:   # 同一字形对应多个数字，说明不可靠
            log(f"字形簇 {cluster_id} 标签不一致 {counter}，跳过")
            continue
        if digit in written:
            continue
        cv2.imwrite(str(TEMPLATE_DIR / f"digit_{digit}.png"), clusters[cluster_id]["example"])
        written[digit] = clusters[cluster_id]["count"]
        log(f"数字模板 {digit}：样本 {clusters[cluster_id]['count']} 个")
    missing = [d for d in "0123456789" if d not in written]
    log(f"完成：使用 {len(samples)} 张样本，已生成 {len(written)} 个数字模板"
        + (f"，缺少 {''.join(missing)}（可再跑几局采集）" if missing else ""))
    return 0


def save_calib_plot(samples: list, a: float, c: float, out_path: Path) -> None:
    """把 (按压时长, 实测距离) 散点和拟合直线画成 PNG（不依赖 matplotlib）。"""
    w, h, pad = 760, 500, 52
    img = np.full((h, w, 3), 255, np.uint8)
    ts = [s[0] for s in samples]
    ls = [s[1] for s in samples]
    tmin, tmax = min(ts), max(ts)
    lmin, lmax = min(ls), max(ls)
    span_t = max(tmax - tmin, 1e-6)
    span_l = max(lmax - lmin, 1e-6)

    def to_px(t: float, dist: float) -> tuple:
        x = pad + (t - tmin) / span_t * (w - 2 * pad)
        y = h - pad - (dist - lmin) / span_l * (h - 2 * pad)
        return int(x), int(y)

    cv2.line(img, (pad, pad), (pad, h - pad), (200, 200, 200), 1)
    cv2.line(img, (pad, h - pad), (w - pad, h - pad), (200, 200, 200), 1)
    for t, dist in zip(ts, ls):
        cv2.circle(img, to_px(t, dist), 4, (180, 90, 40), -1)
    cv2.line(img, to_px(tmin, a * tmin + c), to_px(tmax, a * tmax + c), (200, 60, 0), 2)
    cv2.putText(img, f"L = {a:.4f} * t + {c:.1f}", (pad + 8, pad - 14),
                cv2.FONT_HERSHEY_SIMPLEX, 0.6, (0, 0, 0), 1, cv2.LINE_AA)
    cv2.putText(img, f"n={len(ts)}  t:{tmin:.0f}~{tmax:.0f}ms  L:{lmin:.0f}~{lmax:.0f}px",
                (pad + 8, pad + 6), cv2.FONT_HERSHEY_SIMPLEX, 0.45, (90, 90, 90), 1, cv2.LINE_AA)
    cv2.putText(img, "press time (ms) ->", (w // 2 - 60, h - 16),
                cv2.FONT_HERSHEY_SIMPLEX, 0.5, (90, 90, 90), 1, cv2.LINE_AA)
    cv2.putText(img, "jump distance (px)", (8, h // 2), cv2.FONT_HERSHEY_SIMPLEX,
                0.45, (90, 90, 90), 1, cv2.LINE_AA)
    cv2.imwrite(str(out_path), img)


def analyze_calibration(samples: list, cfg: Config, config_path: Path, tag: str,
                        persist: bool = False) -> dict | None:
    """用开环采样拟合 L = a*t + c，并把结果写进配置。"""
    if len(samples) < 8:
        log(f"标定样本只有 {len(samples)} 个，不足以拟合（至少 8 个）")
        return None
    t = np.array([s[0] for s in samples], dtype=float)
    l = np.array([s[1] for s in samples], dtype=float)
    d = np.array([s[2] for s in samples], dtype=float)
    matrix = np.vstack([t, np.ones_like(t)]).T
    sol, *_ = np.linalg.lstsq(matrix, l, rcond=None)
    a, c = float(sol[0]), float(sol[1])
    pred = matrix @ sol
    resid = l - pred
    ss_tot = float(((l - l.mean()) ** 2).sum())
    r2 = 1.0 - float((resid ** 2).sum()) / max(ss_tot, 1e-9)
    log(f"标定拟合：L = {a:.4f}*t + {c:.1f}   R2={r2:.4f}   残差std={resid.std():.1f}px   n={len(t)}")
    if a > 1e-6:
        log(f"→ 控制律：t = (d - {c:.1f}) / {a:.4f}   等效 k={1.0 / a:.4f} ms/px  截距 b={-c / a:.1f} ms")
    if resid.std() <= 12:
        log("→ 判据：残差 ≤12px，可用该模型直接追中心命中")
    elif resid.std() <= 25:
        log("→ 判据：残差 12~25px，命中率上限约 40~50%，需要精测顶面中心 + 二次项")
    else:
        log("→ 判据：残差 >25px，存在游戏侧随机/量化，按“不摔优先”交付")
    out_csv = LOG_DIR / f"calib_{tag}.csv"
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    with out_csv.open("w", newline="", encoding="utf-8-sig") as fh:
        writer = csv.writer(fh)
        writer.writerow(["hold_ms", "achieved_px", "target_px", "k_used", "residual_px"])
        for (hold, achieved, target, k_used), r in zip(samples, resid):
            writer.writerow([f"{hold:.1f}", f"{achieved:.1f}", f"{target:.1f}",
                             f"{k_used:.3f}", f"{r:.1f}"])
    plot_path = LOG_DIR / f"calib_{tag}.png"
    save_calib_plot(samples, a, c, plot_path)
    log(f"标定数据：{out_csv}")
    log(f"标定散点图：{plot_path}")
    # 只有拟合足够可信才启用两点模型：样本够多、拟合够好、且等效 k 与现有 k 不矛盾。
    # （样本少 + 按压时长区间窄时，斜率会被截距"吸收"，产出严重过拟合的参数。）
    implied_k = (1.0 / a) if a > 1e-6 else float("inf")
    credit = (
        len(t) >= 25
        and r2 >= 0.90
        and cfg.k > 0
        and 0.8 <= implied_k / cfg.k <= 1.25
        and (l.max() - l.min()) >= 150        # 实测距离要覆盖足够宽的范围
    )
    if credit:
        # 默认**不写回配置**：这条拟合的自变量来自已被证伪的落点误差测量
        # （噪声 ±28px、符号都会反），自动启用等于悄悄换掉控制模型。
        # 确认要用时显式加 --persist-calibration。
        if persist:
            cfg.transfer_a = round(a, 4)
            cfg.transfer_c = round(c, 2)
            try:
                cfg.save(config_path)
                log(f"已写入配置：transfer_a={cfg.transfer_a} transfer_c={cfg.transfer_c}")
            except Exception as exc:
                log(f"写配置失败：{exc}")
        else:
            log(f"拟合结果 a={a:.4f} c={c:.1f}（等效 k={implied_k:.3f}）——"
                f"**未写入配置**：该拟合基于已被证伪的落点误差测量，仅作参考；"
                f"确需启用请加 --persist-calibration")
    else:
        log(f"拟合可信度不足（n={len(t)}, R2={r2:.3f}, 等效k={implied_k:.2f} vs 现有k={cfg.k:.2f}, "
            f"距离跨度={l.max() - l.min():.0f}px）→ 不启用两点模型，继续用 k*d 模型")
    return {"a": a, "c": c, "r2": r2, "resid_std": float(resid.std()), "n": len(t),
            "implied_k": implied_k, "credit": credit, "mean_target": float(d.mean())}


def play(args) -> int:
    global _LAST_STOP_REASON
    cfg = Config.load(
        Path(args.config),
        {
            "k": args.k,
            "gamma": args.gamma,
            "max_jumps": args.max_jumps,
            "pause_hotkey": getattr(args, "pause_hotkey", None),
            "startup_wait_s": getattr(args, "startup_wait_s", None),
            "start_delay_s": getattr(args, "start_delay_s", None),
        },
    )
    if args.no_adaptive:
        cfg.gamma = 0.0
    if cfg.transfer_a > 0.01:
        log(f"警告：当前启用标定模型 t=(d-c)/a（a={cfg.transfer_a}, c={cfg.transfer_c}）——"
            f"该模型来自已被证伪的落点误差测量，建议把 config.json 里的 transfer_a 置 0、"
            f"继续用 k*d 模型（k={cfg.k}）")
    k0 = float(cfg.k)
    k = float(cfg.k)
    background = bool(args.background)
    foreground = not background
    status_write(
        pid=os.getpid(), state="starting", game_state=None,
        started=time.time(), max_jumps=cfg.max_jumps, jumps=0, landed=0,
        deaths=0, hits=0, misses=0, unknown=0, streak=0, best_streak=0,
        score=None, best_life_score=0, k=round(k, 4),
        csv="", config=str(args.config), daemon=bool(args.daemon),
    )

    log_dir = Path(args.log_dir) if args.log_dir else LOG_DIR
    log_dir.mkdir(parents=True, exist_ok=True)
    debug_dir = DEBUG_DIR / time.strftime("run_%Y%m%d_%H%M%S")
    if args.save_debug:
        debug_dir.mkdir(parents=True, exist_ok=True)
    csv_path = log_dir / f"run_{time.strftime('%Y%m%d_%H%M%S')}.csv"
    status_write(csv=str(csv_path))

    cap = Capturer(args.capture)
    vision = Vision(cfg)
    ctrl = InputController(background=background)
    records: list[JumpRecord] = []
    # 日志文件**延迟到窗口就绪后再建**：早期版本在这里就开文件，导致"窗口不在、
    # 启动失败"时留下一堆 103 字节的空 run_*.csv（还让 tools/report.py 默认读到空文件）。
    csv_file = None
    csv_writer = None
    csv_header = ["index", "timestamp", "k_before", "dist_px", "hold_ms",
                  "k_after", "error_px", "result", "score", "note",
                  "achieved_px", "center_hit",
                  "t_acquire_ms", "t_landing_ms", "t_score_ms", "t_loop_ms",
                  "err_src", "err_dot_px", "aim_dot", "bias_pct",
                  "mask_backend", "pause_total_s", "active_elapsed_s", "pixel_center_hit"]

    def open_csv() -> None:
        nonlocal csv_file, csv_writer
        if csv_file is not None:
            return
        csv_file = csv_path.open("w", newline="", encoding="utf-8-sig")
        csv_writer = csv.writer(csv_file)
        csv_writer.writerow(csv_header)
        csv_file.flush()

    def emit(record: JumpRecord) -> None:
        records.append(record)
        if csv_writer is None:
            return
        csv_writer.writerow([
            record.index, record.timestamp, f"{record.k_before:.4f}", f"{record.dist_px:.1f}",
            f"{record.hold_ms:.0f}", f"{record.k_after:.4f}",
            "" if record.error_px is None else f"{record.error_px:.1f}",
            record.result, "" if record.score is None else record.score, record.note,
            ("" if record.error_px is None else f"{record.dist_px + record.error_px:.1f}"),
            "" if hud_center_hit(record) is None else hud_center_hit(record),
            f"{record.t_acquire_ms:.0f}", f"{record.t_landing_ms:.0f}",
            f"{record.t_score_ms:.0f}", f"{record.t_loop_ms:.0f}",
            record.err_src,
            "" if record.err_dot_px is None else f"{record.err_dot_px:.1f}",
            record.aim_dot,
            f"{record.bias_pct:g}",
            vision._auto_backend if cfg.masks_backend == "auto" else cfg.masks_backend,
            _CONTROL.snapshot()["pause_total_s"] if _CONTROL is not None else 0,
            round(active_time() - started, 3),
            "" if record.error_px is None else int(abs(record.error_px) < cfg.center_hit_tolerance_px),
        ])
        csv_file.flush()

    deaths = 0
    consecutive_deaths = 0
    best_k = k
    best_alive = 0
    alive_in_round = 0
    err_streak = 0            # 由落点误差推算的连击（诊断用）
    score_streak = 0          # 由真实分数推算的连击（真值）
    predicted_score = 0
    prev_score: int | None = None
    score_hits = 0
    score_misses = 0
    score_unknown = 0
    best_score_streak = 0
    best_life_score = 0
    life_score = 0
    err_ema = 0.0
    skip_count = 0
    run_state = {"fresh": False}
    stall_count = 0
    blind_jumps = 0
    last_good_hold_ms = 0.0
    calib_bands: list = []
    calib_samples: list = []
    band_index = 0
    band_left = 0
    band_attempts = 0
    if args.calibrate:
        cfg.gamma = 0.0
        scales = [float(s) for s in str(args.calib_k_scales).split(",") if s.strip()]
        calib_bands = [(float(cfg.k) * s, int(args.calib_per_band)) for s in scales]
        if calib_bands:
            band_left = calib_bands[0][1]
    bias_levels: list = []
    if args.bias_sweep:
        bias_levels = [float(s) for s in str(args.bias_levels).split(",") if s.strip()]
        if not bias_levels:
            bias_levels = [0.0]
        log(f"偏置扫描已启用：档位 {bias_levels}（百分比，相对沿轴距离），"
            f"每档 {args.bias_per_level} 跳；判据用分数（像素级误差测量已证伪）")
    started = active_time()
    stop_reason = "达到最大跳跃次数"

    try:
        while True:
            win = control_ready_window(cfg, initial=True,
                                       timeout_s=getattr(args, "_next_startup_wait_s", None))
            try:
                if _CONTROL is not None:
                    ctrl.expected_generation = _CONTROL.generation
                log("目标窗口：" + win.describe())
                # control_ready_window has already sized/activated/count-downed
                # the window. Do not repeat foreground operations at startup.
                win = ensure_playing(cap, cfg, vision, ctrl, foreground, True, run_state)
                break
            except ControlInterrupted:
                if poll_stop_key():
                    raise
                vision.reset_prior()
        if win is None:
            log("无法进入可玩状态：请确认游戏窗口可见且没有被遮挡。")
            _LAST_STOP_REASON = "无法进入可玩状态"
            status_write(state="stopped", alive=False,
                         stop_reason="无法进入可玩状态")
            return 3
        log(f"开始运行：k={k:.3f} ms/px, gamma={cfg.gamma}, "
            f"最大跳跃={cfg.max_jumps}, 输入={'后台' if background else '前台'}, "
            f"识别={'capture=' + args.capture}")
        status_write(state="playing", window={"hwnd": win.hwnd, "pid": win.pid,
                                              "title": win.title, "client": list(win.client_rect)})
        # 窗口已就绪、真的要开始跑了，这时才建日志文件（启动失败不留空日志）
        open_csv()
        # 打印生效的判定阈值，避免“改了代码默认值但被 config.json 里旧值覆盖”这种坑
        log(f"生效阈值：沿轴距离 {cfg.min_jump_dist_ratio * win.width:.0f}~"
            f"{cfg.max_jump_dist_ratio * win.width:.0f}px，横向比≤{cfg.axis_max_cross_ratio}，"
            f"最小面积比={cfg.block_min_area_ratio}，最小宽度比={cfg.block_min_width_ratio}，"
            f"棋盘上色差阈={cfg.block_color_delta}，棋子模板阈={cfg.piece_template_min_score}，"
            f"重试上限={cfg.detect_retry_limit}")

        index = 0
        started = active_time()
        last_progress_ts = active_time()
        next_stall_warn = last_progress_ts + 90.0
        while index < cfg.max_jumps:
            try:
                in_flight = False
                if control_checkpoint(cfg, vision):
                    prev_score = None
                    score_streak = 0
                    stall_count = blind_jumps = skip_count = 0
                    last_progress_ts = active_time()
                    next_stall_warn = last_progress_ts + 90.0
                if _CONTROL is not None:
                    ctrl.expected_generation = _CONTROL.generation
                # 卡死看门狗：跳跃是 2~3 秒一次，超过 90 秒没有任何一跳完成，
                # 说明卡在某个等待里（窗口消失/被遮挡/识别不出来）。这里定期把
                # 现场写进日志和状态面板，避免"看起来莫名冻住"。
                if active_time() > next_stall_warn:
                    gap = int(active_time() - last_progress_ts)
                    log(f"警告：已 {gap} 秒没有完成一次跳跃（当前 {index} 跳）；"
                        f"若长时间无变化请按 F8 停止并检查窗口是否被遮挡/关闭")
                    status_write(stuck_s=gap, stop_reason=f"已 {gap} 秒无进展（Watchdog）")
                    next_stall_warn = active_time() + 90.0
                if active_time() - started > cfg.max_runtime_s:
                    stop_reason = "达到最长运行时间"
                    break
                if f8_pressed():
                    stop_reason = "按下 F8 手动停止"
                    break
                win = resolve_window(cfg, cfg.window_wait_s)
                # 每跳前守卫：用户在跑的过程中把窗口拖小了，这里当场调回去（不会在飞行途中动窗口）
                win = ensure_window_size(win, cfg)
                if foreground and foreground_hwnd() != win.hwnd:
                    if not force_foreground(win.hwnd):
                        log("窗口失去前台且无法切回，停止。")
                        stop_reason = "窗口被其他程序抢占前台"
                        break
                status_write(state="acquiring",
                             window={"hwnd": win.hwnd, "pid": win.pid, "title": win.title,
                                     "client": list(win.client_rect)})
                t_iter_start = time.perf_counter()
                aim_dot_flag = 0          # 本跳瞄点是否用了游戏中心白点（写进 CSV 的 aim_dot）
                acquired = acquire_frame(cap, cfg, vision, ctrl, foreground, run_state)
                t_acquire_ms = (time.perf_counter() - t_iter_start) * 1000.0
                if acquired is None:
                    # 先确认不是用户按了 F8：acquire_frame 里 poll_stop_key() 命中时也返回 None，
                    # 早期版本会把它当成"识别失败"继续盲跳一次——等于急停之后又补按了一下鼠标。
                    if f8_pressed():
                        stop_reason = "按下 F8 手动停止"
                        break
                    # 有界盲跳：识别彻底失败时，用“上次成功的按压时长”盲跳一次推进局面。
                    # 死亡可以自动重开，而整轮停止会丢掉后续所有进度——所以盲跳比停下更优。
                    blind_jumps += 1
                    if blind_jumps <= cfg.blind_jump_limit:
                        hold_ms = last_good_hold_ms if last_good_hold_ms > 0 else k * 270.0
                        hold_ms = clamp(hold_ms, cfg.press_duration_min_ms, cfg.press_duration_max_ms)
                        log(f"识别连续失败，执行第 {blind_jumps}/{cfg.blind_jump_limit} 次盲跳"
                            f"（按压 {hold_ms:.0f}ms，用上次成功值）")
                        if foreground:
                            px = int(win.client_rect[0] + cfg.press_point[0] * win.width)
                            py = int(win.client_rect[1] + cfg.press_point[1] * win.height)
                            if not point_is_window(win.hwnd, px, py):
                                log("按压点被遮挡，跳过盲跳")
                                time.sleep(1.5)
                                continue
                        ctrl.press(win, cfg.press_point[0], cfg.press_point[1], hold_ms / 1000.0)
                        vision.reset_prior()
                        time.sleep(0.4)
                        continue
                    stop_reason = "连续识别失败且盲跳用尽（安全策略：停止）"
                    log(stop_reason)
                    break
                win, frame, piece, target, block_mask = acquired
                blind_jumps = 0
                if target.dot is not None:
                    # 每个跳跃只打一条：本跳的瞄点被"游戏自带的中心白点"校正过。
                    aim_dot_flag = 1
                    log(f"#{index + 1} 瞄点用中心白点校正：({target.dot[0]:.0f},{target.dot[1]:.0f})"
                        f" 面积 {target.dot[2]}")

                # 跳跃方向被游戏锁死为两个等距轴向，按沿轴投影长度算按压时长
                axis, along, cross = axis_components(
                    target.x - piece.x, target.y - piece.y, cfg.axis_slope
                )
                cross_ratio = cross / max(abs(along), 1.0)
                # find_target 内部已经做了分级放宽（特殊小方块、贴邻/宽方块场景），
                # 这里只做最后一道“明显不合理才拦”的兜底校验，不要比它更严。
                min_dist = cfg.min_jump_dist_ratio * win.width * 0.4
                max_dist = cfg.max_jump_dist_ratio * win.width
                problem = None
                if along <= 0:
                    problem = f"沿轴距离非正({along:.0f})"
                elif not (min_dist <= along <= max_dist):
                    problem = f"沿轴距离异常({along:.0f}px，合理 {min_dist:.0f}~{max_dist:.0f})"
                elif cross_ratio > cfg.axis_max_cross_ratio + 0.20:
                    problem = f"偏离轴向过大(cross/along={cross_ratio:.2f})"
                if problem:
                    skip_count += 1
                    log(f"第 {index + 1} 跳校验未通过：{problem}，本次不发按键"
                        f"（{skip_count}/{cfg.detect_retry_limit}）")
                    if skip_count >= cfg.detect_retry_limit:
                        stop_reason = "识别结果持续异常（安全策略：未盲按，已停止）"
                        log(stop_reason)
                        break
                    time.sleep(0.25)
                    continue
                skip_count = 0
                direction = axis
                dist = along
                # 粘连块（顶面估计不可信）补偿实验：命中率明显偏低的那一类，按比例缩短按压。
                if (cfg.low_ratio_trim_ratio > 0 and target.bbox[2] > 0
                        and target.top_width < cfg.low_ratio_gate * target.bbox[2]):
                    trimmed = dist * (1.0 - cfg.low_ratio_trim_ratio)
                    log(f"粘连块补偿：沿轴 {dist:.0f}px → {trimmed:.0f}px"
                        f"（-{cfg.low_ratio_trim_ratio * 100:.1f}%）")
                    dist = trimmed
                if calib_bands:
                    k = calib_bands[band_index][0]
                # 偏置扫描：把"沿轴距离"乘上 (1+bias/100)。这等价于给按压时长加一个
                # 与距离成比例的受控偏置——用来测"游戏中心判定区间的中心在哪"，
                # 而这是唯一能用权威判据（分数）测出系统性偏置的办法（像素级误差
                # 测量已全部被证伪，见 README「落点误差的'真值'路线破产」）。
                bias_pct = 0.0
                if bias_levels:
                    per = max(1, int(args.bias_per_level))
                    bias_pct = bias_levels[(index // per) % len(bias_levels)]
                    dist *= (1.0 + bias_pct / 100.0)
                    if index % per == 0:
                        log(f"[扫描] 第 {index // per + 1} 段：距离偏置 {bias_pct:+g}%"
                            f"（每段 {per} 跳，档位 {args.bias_levels}）")
                k_before = k
                if cfg.transfer_a > 0.01:
                    hold_ms = (dist - cfg.transfer_c) / cfg.transfer_a
                else:
                    hold_ms = k * dist + cfg.b
                hold_ms *= (1.0 + cfg.press_bias_ratio)
                if cfg.gamma > 0:
                    trim = 1.0 + clamp(-0.3 * err_ema,
                                       -cfg.residual_trim_ratio, cfg.residual_trim_ratio)
                    hold_ms *= trim
                hold_ms = clamp(hold_ms, cfg.press_duration_min_ms, cfg.press_duration_max_ms)
                if args.save_debug:
                    cv2.imwrite(
                        str(debug_dir / f"jump_{index + 1:03d}_before.png"),
                        annotate(frame, piece, target, hold_ms, f"#{index + 1} k={k:.3f}"),
                    )
                    cv2.imwrite(str(debug_dir / f"jump_{index + 1:03d}_raw.png"), frame)

                # 安全闸：确认按压点确实落在游戏窗口上。若被别的窗口（例如微信分享面板）
                # 盖住，就只等待、绝不按键——实测盲按会点到游戏内分享按钮，弹出的微信分享面板
                # 无法用点外部的方式关闭，只能人工处理。
                if foreground:
                    px = int(win.client_rect[0] + cfg.press_point[0] * win.width)
                    py = int(win.client_rect[1] + cfg.press_point[1] * win.height)
                    if not point_is_window(win.hwnd, px, py):
                        log("按压点被其他窗口遮挡（可能是微信分享面板/弹窗），等待中，不发送按键")
                        status_write(state="blocked_modal", game_state="blocked_modal",
                                     stop_reason="按压点被其他窗口遮挡，正在等待（不发送按键）")
                        time.sleep(1.5)
                        continue
                ctrl.press(win, cfg.press_point[0], cfg.press_point[1], hold_ms / 1000.0)
                in_flight = True
                status_write(state="landing", last_hold_ms=round(hold_ms),
                             last_dist_px=round(dist, 1))
                # 跳跃会大幅改变画面，旧的位置先验必然失效，先清掉再等落地
                vision.reset_prior()
                # want_target 只在要存调试图时打开：长跑模式不需要落地方块的目标点，
                # 省掉每跳一次 find_target（16.5ms）。
                status, frame2, piece2, target2, block_mask2 = wait_landing(
                    cap, win, vision, cfg, want_target=args.save_debug)
                t_landing_ms = (time.perf_counter() - t_iter_start) * 1000.0 - t_acquire_ms - hold_ms
                score = read_score(frame2, cfg) if frame2 is not None else None
                # 落地的瞬间 HUD 往往还没刷新，直接读会读到上一跳的旧分：
                # 这会让增量变成两跳之和（表现为 +3 之类怪值），也会让卡死判据误触发。
                # 因此读到与上次相同就重新抓帧重读几次。
                # 读不出来（None）同样要重读：实测 2026-10-01 第 3973 跳落地确认超时，
                # 恰好那几帧 HUD 读数为 None，导致"分数增长"这条最终裁决拿不到证据，
                # 被误判成死亡（实际游戏一直在得分：130026→130038→130056…）。
                if (status not in ("pause_unknown", "stopped") and prev_score is not None
                        and (score is None or score == prev_score)):
                    # 第一次重读只等很短（HUD 通常 100ms 内刷新），后面才用 250ms，
                    # 这样"分数滞后"这个高频情形不会再白等 250ms。
                    for attempt_i in range(3):
                        time.sleep((cfg.score_reread_first_ms if attempt_i == 0 else 250.0) / 1000.0)
                        if _CONTROL is not None:
                            try:
                                fresh_win = resolve_window(cfg, 0, draining=True)
                            except WindowGoneError:
                                if not _CONTROL.pause_requested:
                                    raise
                                status = "pause_unknown"
                                frame2 = None
                                score = None
                                break
                            if (_CONTROL.pause_requested and foreground_hwnd() != fresh_win.hwnd):
                                status = "pause_unknown"
                                frame2 = None
                                score = None
                                break
                        else:
                            fresh_win = resolve_window(cfg, cfg.window_wait_s)
                        fresh = cap.grab(fresh_win)
                        reread = read_score(fresh, cfg)
                        if reread is None:
                            continue
                        score = reread
                        frame2 = fresh
                        if reread != prev_score:
                            # 读到与上一跳不同的分数 = HUD 确实刷新了（可能是落地得分，
                            # 也可能是新一局的低分），可以据此裁决，无需再等。
                            break
                timestamp = time.strftime("%H:%M:%S")
                t_score_ms = ((time.perf_counter() - t_iter_start) * 1000.0
                              - t_acquire_ms - hold_ms - t_landing_ms)

                if status == "stopped":
                    stop_reason = "按下 F8 手动停止"
                    emit(JumpRecord(index + 1, timestamp, k_before, dist, hold_ms, k, None,
                                    "stopped", None, "F8 急停"))
                    break

                # 落地确认超时 ≠ 死亡：浅色方块（快递箱、便利店屋顶等）与淡背景颜色接近时，
                # `_foot_on_block` 会因为方块掩码里没有脚下那块而一直判不了落地，2.6s 后超时。
                # 实测 2026-10-01 #723：判定死亡，但 HUD 分数 19215→19247(+32) 还在继续涨，
                # 说明棋子明明落在快递箱上拿了中心加分。这里用**分数增长**做最终裁决：
                # 分数涨了就是落地（只是没测到误差），分数没动才算真死。
                #
                # 2026-10-01 13:40 修：原来这里还要求 `piece2 is not None`，于是
                # "超时那一帧镜头未稳、棋子检测不到，但 0.25s 后重读的分数已经涨了"
                # 这种情形会被误判死亡 —— 实测 01:33:38 那次：分数 8066→8098(+32，满连击
                # 中心命中)，CSV 却记 dead，紧接着 #1373 又以同一条命继续跳。
                # 分数是权威信号，不该被一个辅助的棋子检测挡住。
                delayed_landing = (
                    status != "landed" and prev_score is not None
                    and score is not None and score > prev_score
                )
                if (status == "pause_unknown" or (_CONTROL is not None
                        and _CONTROL.pause_requested and status != "landed" and not delayed_landing)):
                    record = JumpRecord(index + 1, timestamp, k_before, dist, hold_ms, k, None,
                                        "unknown", score, "暂停收尾：无法可靠核对，不判死亡",
                                        t_acquire_ms, t_landing_ms, t_score_ms,
                                        (time.perf_counter() - t_iter_start) * 1000.0)
                    emit(record)
                    index += 1
                    in_flight = False
                    prev_score = None
                    score_unknown += 1
                    status_write(jumps=index, **accuracy_fields(records))
                    continue
                if status == "landed" and piece2 is not None or delayed_landing:
                    err = None
                    err_src = ""
                    err_dot = None
                    if delayed_landing:
                        log(f"#{index + 1} 落地确认超时（{status}），但分数 "
                            f"{prev_score}→{score} 说明已落地，按落地处理（不判死）")
                    elif cfg.landing_method == "flow" and piece2 is not None:
                        detail = measure_landing_error_flow(frame, frame2, target, piece2, cfg, direction)
                        if detail is not None:
                            err, run_width = detail
                            err_src = "flow"
                    elif block_mask2 is not None and piece2 is not None:
                        detail = measure_landing_error(
                            frame2, block_mask2, piece2, cfg, direction,
                            expected_width=target.top_width,
                        )
                        if detail is not None:
                            err, run_width, center_x = detail
                            err_src = "mask"
                            # 真值校正：落地方块上若有游戏自带的中心白点，直接用它当中心——
                            # 实测（2026-10-01）掩码法的中心估计在"抬高台面"类方块上会偏
                            # 20~25px，符号都能反（16 个现场帧：真值均值 +22.6px，掩码法 -3.9px）。
                            # 这是唯一能让落点误差变成可信信号的办法。
                            if cfg.center_dot_max_offset_px > 0:
                                anchor_y = piece2.y + cfg.piece_anchor_dy_ratio * frame2.shape[0]
                                dot = find_dot_near(frame2, center_x, anchor_y + 18.0,
                                                    cfg.center_dot_landing_radius_px, cfg)
                                if dot is not None:
                                    ux, uy = direction[0], direction[1]
                                    # 白点值单独记，**不覆盖** err：落地时棋子把白点挡住，
                                    # 实测这批值里约一半与游戏判分矛盾（见 JumpRecord 注释）。
                                    err_dot = (piece2.x - dot[0]) * ux + (anchor_y - dot[1]) * uy
                    # 信号 A（诊断用）：用落点误差预测连击，用于和真实分数对照
                    if err is not None and abs(err) < cfg.center_hit_tolerance_px:
                        err_streak += 1
                        predicted_score += 2 * err_streak
                    else:
                        err_streak = 0
                        predicted_score += 1

                    # 信号 B（真值）：读分已实测 19/19 正确。delta==1 → 普通落点；
                    # delta==min(2*连击数,32) → 命中中心；其它 → 不计入统计。
                    #
                    # 抗级联设计（实测过：一次 delta=0 的滞后读数会让连击计数错位，
                    # 之后所有判定全变 unknown）：
                    #   - delta<=0（HUD 未刷新）→ 重读一次，仍为 0 则跳过，且**不重置连击**
                    #   - delta 异常大（HUD 显示错乱）→ 跳过，不重置连击
                    #   - 偶数 delta 但与本跳预期不符（连击计数已错位）→ 由 delta 反推连击数重新同步
                    note_center = "score=n/a"
                    delta_this_jump = None       # 本跳的真实分差（用于卡死判据）
                    if score is not None:
                        if prev_score is None:
                            note_center = "score基准"
                        elif score >= prev_score:
                            delta = score - prev_score
                            delta_this_jump = delta
                            # 实测规律：连击加成为 min(2×连续中心数, 32)，+32 封顶
                            expect_center = min(2 * (score_streak + 1), cfg.score_bonus_cap)
                            if delta <= 0 or delta > 200:
                                score_unknown += 1
                                note_center = f"读数异常(+{delta})"
                            elif delta == 1:
                                score_streak = 0
                                score_misses += 1
                                note_center = "miss"
                            elif delta == expect_center:
                                score_streak += 1
                                score_hits += 1
                                note_center = f"center x{score_streak}"
                            elif delta >= 4 and delta % 2 == 0 and delta <= cfg.score_bonus_cap:
                                # 计数错位：由增量反推连击数（偶数增量必为命中）
                                score_streak = min(delta // 2, cfg.score_bonus_cap // 2)
                                score_hits += 1
                                note_center = f"center x{score_streak}(重同步)"
                            else:
                                score_unknown += 1
                                note_center = f"unknown(+{delta})"   # 保连击，避免级联失真
                        else:
                            score_streak = 0        # 分数变小 = 换局/重开
                            note_center = "score重置"
                        prev_score = score
                        life_score = score
                        best_life_score = max(best_life_score, score)
                        best_score_streak = max(best_score_streak, score_streak)
                    if (args.save_debug and frame2 is not None and err is not None
                            and run_state["fresh"]):
                        score_dir = DEBUG_DIR / "score"
                        score_dir.mkdir(parents=True, exist_ok=True)
                        roi = crop_score_roi(frame2, cfg)
                        if roi is not None:
                            cv2.imwrite(
                                str(score_dir / f"j{index + 1:03d}_s{predicted_score:04d}.png"), roi
                            )
                    k_after = k
                    if err is not None and cfg.gamma > 0:
                        ratio = err / max(dist, 1.0)
                        if abs(err) < 0.35 * run_width:  # 测量可信才用于调参（过大视为噪声）
                            # 指数平滑，抑制单帧测量噪声带来的抖动
                            err_ema = 0.55 * err_ema + 0.45 * ratio
                            step = clamp(-cfg.gamma * err_ema, -cfg.max_step_ratio, cfg.max_step_ratio)
                            k_after = clamp(k * (1.0 + step), k0 * cfg.min_k_ratio, k0 * cfg.max_k_ratio)
                    k = k_after
                    last_good_hold_ms = hold_ms
                    alive_in_round += 1
                    if alive_in_round > best_alive:
                        best_alive = alive_in_round
                        best_k = k
                    consecutive_deaths = 0
                    note = f"err={err:.1f}px" if err is not None else "err=n/a"
                    note += f" pred={predicted_score}"
                    note += f" score={score} [{note_center}]"
                    # 目标几何写进备注：事后用它按“方块类型/大小”关联真实命中率，
                    # 不需要保存每一帧图（落点误差测量已证明与真值几乎无关，只有分数可信）。
                    if target is not None:
                        tb = target.bbox
                        note += (f" tb={int(tb[0])},{int(tb[1])},{int(tb[2])},{int(tb[3])}"
                                 f" tw={target.top_width:.0f}")
                    # 未命中（真实分数判据判为普通落点）时留现场帧：这是唯一能解释
                    # “为什么这一跳没进中心”的证据，且频率只有百分之几，不会撑爆磁盘。
                    if note_center == "miss" and frame is not None:
                        try:
                            miss_dir = DEBUG_DIR / "miss"
                            miss_dir.mkdir(parents=True, exist_ok=True)
                            stamp = time.strftime("%Y%m%d_%H%M%S")
                            cv2.imwrite(str(miss_dir / f"miss_{stamp}_before.png"), frame)
                            if frame2 is not None:
                                cv2.imwrite(str(miss_dir / f"miss_{stamp}_after.png"), frame2)
                        except Exception:
                            pass
                    if calib_bands and err is not None:
                        calib_samples.append((hold_ms, dist + err, dist, k))
                        log(f"[标定] k={k:.3f} t={hold_ms:.0f}ms  d={dist:.0f}px  "
                            f"L={dist + err:.0f}px  err={err:+.1f}px")
                    if args.save_debug:
                        cv2.imwrite(
                            str(debug_dir / f"jump_{index + 1:03d}_after.png"),
                            annotate(frame2, piece2, target2, None, f"#{index + 1} {note}"),
                        )
                        cv2.imwrite(str(debug_dir / f"jump_{index + 1:03d}_raw_after.png"), frame2)
                    log(
                        f"#{index + 1} d={dist:.1f}px hold={hold_ms:.0f}ms {note} "
                        f"k {k_before:.3f}->{k:.3f} score={score} "
                        + ("piece=(未检出)" if piece2 is None
                           else f"piece=({piece2.x:.0f},{piece2.y:.0f})")
                    )
                    record = JumpRecord(index + 1, timestamp, k_before, dist, hold_ms, k, err,
                                        "landed", score, note,
                                        t_acquire_ms, t_landing_ms, t_score_ms,
                                        (time.perf_counter() - t_iter_start) * 1000.0,
                                        err_src, err_dot, aim_dot_flag, bias_pct)
                    # 卡死判据：**连续多跳真实分数都没涨**才算“按键没生效”。
                    # （早期用“棋子落点位置不变”判断，但镜头本来就会把棋子摆回相近位置，
                    #   会误杀正常运行——实测在 175 跳时被误判过一次。）
                    # 注意：必须用“本跳分差”，不能用 score 与 prev_score 比较——后者在本跳
                    # 已被更新为同一个值，会导致每次跳跃都判定为卡死（实测 3 跳就误停）。
                    if delta_this_jump is not None and delta_this_jump <= 0:
                        stall_count += 1
                    else:
                        stall_count = 0
                    if stall_count >= 3:
                        # 注意：这里**不能**盲点“返回键”自救——游戏里那个坐标附近是分享按钮，
                        # 实测会弹出微信分享面板，比卡死更难恢复。只在 acquire_frame 里
                        # 确认检测到深色弹窗时才点击关闭。
                        log(f"连续 {stall_count} 跳分数没有变化，判定按键未生效（游戏画面无响应），停止运行")
                        log("提示：请检查游戏窗口是否有遮挡弹窗，必要时手动关闭后重开脚本。")
                        emit(record)
                        stop_reason = "游戏画面无响应（按键未生效）"
                        break
                else:
                    deaths += 1
                    consecutive_deaths += 1
                    alive_in_round = 0
                    err_streak = 0
                    score_streak = 0
                    predicted_score = 0
                    prev_score = None
                    note = f"未落地({status})"
                    if cfg.gamma > 0:
                        if records and records[-1].error_px is not None:
                            sign = 1.0 if records[-1].error_px > 0 else -1.0
                            k = clamp(k * (1.0 - 0.03 * sign), k0 * cfg.min_k_ratio, k0 * cfg.max_k_ratio)
                            note += "，按上次误差方向微调"
                        else:
                            k = clamp(k * 1.03, k0 * cfg.min_k_ratio, k0 * cfg.max_k_ratio)
                    if consecutive_deaths >= 3:
                        k = best_k
                        consecutive_deaths = 0
                        note += "，连续死亡 3 次，回退历史最优 k"
                    if args.save_debug and frame2 is not None:
                        cv2.imwrite(str(debug_dir / f"jump_{index + 1:03d}_dead.png"), frame2)
                    # 死亡现场一律留存（不限 --save-debug）：跳跃前的目标帧 + 落地帧，
                    # 便于事后判断是“选错目标”还是“按压时长/事件丢失”导致的超差。
                    try:
                        DEBUG_DIR.mkdir(parents=True, exist_ok=True)
                        stamp = time.strftime("%Y%m%d_%H%M%S")
                        if frame is not None:
                            cv2.imwrite(str(DEBUG_DIR / f"dead_{stamp}_before.png"), frame)
                        if frame2 is not None:
                            cv2.imwrite(str(DEBUG_DIR / f"dead_{stamp}_after.png"), frame2)
                    except Exception:
                        pass
                    log(f"#{index + 1} 死亡（{status}），累计死亡 {deaths}，k={k:.3f} {note}")
                    status_write(state="dead", game_state="game_over", deaths=deaths,
                                 best_life_score=best_life_score, last_result="dead")
                    record = JumpRecord(index + 1, timestamp, k_before, dist, hold_ms, k, None,
                                        "dead", score, note,
                                        t_acquire_ms, t_landing_ms, t_score_ms,
                                        (time.perf_counter() - t_iter_start) * 1000.0)
                    if _CONTROL is None or not _CONTROL.pause_requested:
                        win = ensure_playing(cap, cfg, vision, ctrl, foreground, True, run_state)
                    if win is None:
                        log("死后无法重新开始，停止。")
                        emit(record)
                        stop_reason = "无法自动重新开始"
                        break

                emit(record)
                index += 1
                in_flight = False
                last_progress_ts = active_time()
                next_stall_warn = last_progress_ts + 90.0
                status_write(
                    state="playing", game_state="playing",
                    jumps=index, deaths=deaths,
                    streak=score_streak, best_streak=best_score_streak,
                    score=score, life_score=life_score, best_life_score=best_life_score,
                    k=round(k, 4), last_note=record.note, last_result=record.result,
                    last_index=index, last_time=record.timestamp,
                    elapsed_text=time.strftime("%H:%M:%S", time.gmtime(active_time() - started)),
                    round=index, csv=str(csv_path), **accuracy_fields(records),
                )
                if calib_bands:
                    band_attempts += 1
                    if record.result == "landed":
                        band_left -= 1
                    exhausted = band_left <= 0
                    too_many = band_attempts >= calib_bands[band_index][1] * 3
                    if exhausted or too_many:
                        if too_many and not exhausted:
                            log(f"该档尝试 {band_attempts} 次仍只采到 "
                                f"{calib_bands[band_index][1] - band_left} 个有效样本，跳过剩余")
                        band_index += 1
                        if band_index >= len(calib_bands):
                            stop_reason = "标定采样完成"
                            break
                        band_left = calib_bands[band_index][1]
                        band_attempts = 0
                        log(f"标定进入下一档 k={calib_bands[band_index][0]:.3f}"
                            f"（{band_left} 跳）")
            except ControlInterrupted as exc:
                if poll_stop_key():
                    stop_reason = "用户停止（F8 / q / Esc）"
                    break
                if in_flight:
                    emit(JumpRecord(index + 1, time.strftime("%H:%M:%S"), k_before,
                                    dist, hold_ms, k, None, "unknown", None,
                                    "控制中断：当前跳无法核对，不判死亡"))
                    index += 1
                    score_unknown += 1
                    prev_score = None
                    status_write(jumps=index, **accuracy_fields(records))
                vision.reset_prior()
                log(f"暂停/安全中断：{exc}；重新识别后继续")
                time.sleep(0.05)
                continue
    except ControlInterrupted:
        stop_reason = "用户停止（F8 / q / Esc）"
    except KeyboardInterrupt:
        stop_reason = "用户中断（Ctrl+C）"
    finally:
        cap.close()
        if csv_file is not None:
            try:
                csv_file.close()
            except Exception:
                pass
    if csv_file is not None and not records:
        # 拿到窗口但一跳都没记就结束（例如"识别结果持续异常"安全停止）：
        # 只留一个表头的 CSV 没有任何价值，还会让 tools/report.py 默认读到空文件。
        try:
            csv_path.unlink(missing_ok=True)
            log("本轮 0 跳，已删除仅含表头的空日志")
        except Exception:
            pass
    landed = sum(1 for r in records if r.result == "landed")
    log(f"结束：{stop_reason}；共 {len(records)} 跳，落地 {landed}，死亡 {deaths}，"
        f"最终 k={k:.3f}，最优 k={best_k:.3f}（最长存活 {best_alive}）")
    if csv_file is not None and records:
        log(f"日志：{csv_path}")
    if calib_bands:
        analyze_calibration(calib_samples, cfg, Path(args.config),
                            time.strftime("%Y%m%d_%H%M%S"),
                            persist=bool(args.persist_calibration))
    if records:
        hits = sum(1 for r in records
                   if r.error_px is not None and abs(r.error_px) < cfg.center_hit_tolerance_px)
        measured = sum(1 for r in records if r.error_px is not None)
        if measured:
            log(f"命中统计（落点误差判据，仅供参考）：{hits}/{measured} = "
                f"{hits / measured * 100:.1f}%（|err| < {cfg.center_hit_tolerance_px:.0f}px）")
        judged = score_hits + score_misses
        if judged:
            log(f"命中统计（真实分数判据，权威）：{score_hits}/{judged} = "
                f"{score_hits / judged * 100:.1f}%"
                f"（另有 {score_unknown} 跳因礼盒等额外加分无法判定，{score_misses} 次普通落点）")
    if args.save_debug:
        log(f"调试图：{debug_dir}")

    if args.persist_best_k:
        try:
            cfg.k = round(best_k, 4)
            cfg.save(Path(args.config))
            log(f"已把最优 k={best_k:.4f} 写回 {args.config}")
        except Exception as exc:
            log(f"写回 k 失败：{exc}")
    _LAST_STOP_REASON = stop_reason
    status_write(state="stopped", alive=False, stop_reason=stop_reason,
                 jumps=len(records), deaths=deaths,
                 best_streak=best_score_streak, best_life_score=best_life_score,
                 elapsed_text=time.strftime("%H:%M:%S", time.gmtime(active_time() - started)),
                 **accuracy_fields(records))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="跳一跳 屏幕识别自动脚本（自适应版）",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--config", default=str(DEFAULT_CONFIG_PATH), help="配置文件路径")
    parser.add_argument("--pause-hotkey", default=None, help="暂停/恢复热键，默认 Alt+P")
    parser.add_argument("--startup-wait-s", type=float, default=None,
                        help="等待用户打开小游戏的秒数，0=无限等待")
    parser.add_argument("--start-delay-s", type=float, default=None,
                        help="启动/恢复前倒计时秒数，默认 3")
    parser.add_argument("--backend", choices=["auto", "gpu", "cuda", "opencl", "simd", "cpu"],
                        default=None, help="本次运行的掩码后端（不改配置文件）")
    parser.add_argument("--no-cuda", action="store_true", help="禁用 CUDA，仍可用 OpenCL/CPU")
    parser.add_argument("--no-opencl", action="store_true", help="禁用 OpenCL，仍可用 CUDA/CPU")
    parser.add_argument("--opencl-device", default="", help="按名称/厂商子串选择 OpenCL GPU")
    parser.add_argument("--probe", action="store_true", help="只抓一帧保存，验证窗口与截图")
    parser.add_argument("--selftest", action="store_true",
                        help="部署自检：依赖/模板/GPU 后端/掩码一致性（不碰游戏窗口）")
    parser.add_argument("--cupydebug", action="store_true",
                        help=argparse.SUPPRESS)
    parser.add_argument("--no-activate", action="store_true", help="probe 时不切前台")
    parser.add_argument("--out-dir", default="", help="probe 输出目录")
    parser.add_argument("--samples", type=int, default=0, help="连续抓取 N 帧保存到 debug/samples")
    parser.add_argument("--sample-interval", type=float, default=0.35, help="抓样本间隔秒")
    parser.add_argument("--click-xy", default="", help="调试：单击一次，格式 fx,fy（0~1 相对客户区）")
    parser.add_argument("--image", default="", help="离线识别：对图片/通配符跑识别并输出标注图")
    parser.add_argument("--jump-test", action="store_true", help="联调：跳一次并连拍观察")
    parser.add_argument("--hold-ms", type=float, default=0.0,
                        help="联调：指定按压毫秒数（0=按识别距离自动算）")
    parser.add_argument("--burst", type=int, default=8, help="联调连拍帧数")
    parser.add_argument("--burst-interval", type=float, default=0.25, help="联调连拍间隔秒")
    parser.add_argument("--capture", choices=["screen", "printwindow"], default="screen")
    parser.add_argument("--background", action="store_true", help="后台 PostMessage 输入")
    parser.add_argument("--dry-run", action="store_true", help="只识别不发输入")
    parser.add_argument("--max-jumps", type=int, default=None, help="最大跳跃次数")
    parser.add_argument("--k", type=float, default=None, help="按压系数 ms/px")
    parser.add_argument("--gamma", type=float, default=None, help="自适应增益")
    parser.add_argument("--no-adaptive", action="store_true", help="关闭自适应修正")
    parser.add_argument("--save-debug", action="store_true", help="保存每跳标注图")
    parser.add_argument("--log-dir", default="", help="日志目录")
    parser.add_argument("--persist-best-k", action="store_true",
                        help="结束时把历史最优 k 写回配置文件")
    parser.add_argument("--persist-calibration", action="store_true",
                        help="把 --calibrate 的拟合结果写回配置（默认不写：该测量已被证伪）")
    parser.add_argument("--harvest-score", action="store_true",
                        help="从 debug/score 的样本挖掘 0-9 数字模板")
    parser.add_argument("--score-dir", default="", help="数字模板挖掘的样本目录")
    parser.add_argument("--calibrate", action="store_true",
                        help="开环标定：固定多档 k 采样，拟合 按压时长→跳跃距离 曲线")
    parser.add_argument("--calib-per-band", type=int, default=20, help="标定每档跳数")
    parser.add_argument("--calib-k-scales", default="0.9,1.0,1.1",
                        help="标定用 k 档位（相对当前 k 的倍数，逗号分隔）")
    parser.add_argument("--bias-sweep", action="store_true",
                        help="偏置扫描标定：按 --bias-levels 逐档给沿轴距离加受控偏置，"
                             "用分数判据找最优偏置（像素级误差测量已被证伪）")
    parser.add_argument("--bias-levels", default="0,2,-2,4,-4,6,-6",
                        help="偏置档位（百分比，相对沿轴距离），逗号分隔")
    parser.add_argument("--bias-per-level", type=int, default=100,
                        help="偏置扫描每档连续跳数（默认 100）")
    parser.add_argument("--daemon", action="store_true",
                        help="长跑模式：本轮结束后自动等「跳一跳」窗口回来继续下一轮"
                             "（按 F8 退出整个 daemon）")
    parser.add_argument("--daemon-wait-s", type=float, default=0.0,
                        help="daemon 等待窗口重新出现的最长秒数（0 = 一直等，按 F8 退出）")
    parser.add_argument("--daemon-max-rounds", type=int, default=0,
                        help="daemon 最多跑几轮（0 = 不限）")
    parser.add_argument("--no-dashboard", action="store_true",
                        help="不弹出实时状态面板窗口（默认会弹：PID/状态/分数/实时命中率）")
    return parser


def opencl_device_check(selector: str) -> int:
    """内部驱动预热入口，明确禁止输入，不初始化 CUDA、不定位游戏窗口。"""
    setup_stdio()
    os.environ["JUMPBOT_NO_INPUT"] = "1"
    os.environ["JUMPBOT_DISABLE_CUDA"] = "1"
    os.environ["JUMPBOT_OPENCL_PROBE"] = "1"
    cfg = Config(opencl_device=selector, cuda_enabled=False)
    vision = Vision(cfg)
    frame = np.random.default_rng(42).integers(0, 256, (1050, 553, 3), dtype=np.uint8)
    result = vision._masks_opencl(frame)
    if result is None:
        return 3
    expected = vision._masks_simd(frame)
    return 0 if all(np.array_equal(a, b) for a, b in zip(expected, result)) else 4


def main(argv=None) -> int:
    global _CONTROL, _STOP_REQUESTED
    listener = None
    # 打包后单文件 exe 复用：python 面板以 `jumpbot.exe --panel ...` 启动
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    if len(raw_argv) == 2 and raw_argv[0] == "--_opencl-check":
        return opencl_device_check(raw_argv[1])
    # 打包后 sys.executable 就是本 exe：CuPy / cuda.pathfinder 等库会派生
    # `python -m <module>`（偶尔 `-c`）去做动态库探测，这里让 exe 表现得像 python，
    # 否则子进程会因"无法识别的参数"直接失败，导致 GPU 后端在打包版里起不来。
    if raw_argv and raw_argv[0] in ("-m", "-c") and len(raw_argv) >= 2:
        import runpy
        if raw_argv[0] == "-m":
            sys.argv = [raw_argv[1]] + raw_argv[2:]
            runpy.run_module(raw_argv[1], run_name="__main__")
        else:
            sys.argv = ["-c"] + raw_argv[2:]
            exec(compile(raw_argv[1], "<string>", "exec"), {"__name__": "__main__"})
        return 0
    if "--panel" in raw_argv:
        idx = raw_argv.index("--panel")
        return run_panel(raw_argv[idx + 1:])
    setup_stdio()
    make_dpi_aware()
    boost_timer_resolution()
    ensure_user_config()
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.backend:
        os.environ["JUMPBOT_MASKS_BACKEND"] = args.backend
    if args.no_cuda:
        os.environ["JUMPBOT_DISABLE_CUDA"] = "1"
    if args.no_opencl:
        os.environ["JUMPBOT_DISABLE_OPENCL"] = "1"
    if args.opencl_device:
        os.environ["JUMPBOT_OPENCL_DEVICE"] = args.opencl_device
    _cfg_for_tune = None
    try:
        _cfg_for_tune = Config.load(Path(args.config))
    except Exception:
        pass
    tune_cpu(threads=getattr(_cfg_for_tune, "cv2_threads", 4), priority="above")
    # 命令行窗口里按键即可停止（q / Q / Esc），与 F8 等价；无控制台时自动跳过
    _STOP_REQUESTED = False
    try:
        if args.selftest:
            return selftest(args)
        if args.cupydebug:
            return cupydebug(args)
        if args.image:
            return run_image(args)
        if args.harvest_score:
            return harvest_score(args)
        if args.jump_test:
            return jump_once(args)
        if args.dry_run:
            return dry_run(args)
        if args.probe or args.samples or args.click_xy:
            return probe(args)
        live_cfg = Config.load(Path(args.config), {
            "pause_hotkey": args.pause_hotkey,
            "startup_wait_s": args.startup_wait_s,
            "start_delay_s": args.start_delay_s,
        })
        _CONTROL = RunControl(live_cfg.pause_hotkey, notify=control_event)
        listener = WindowsHotkeyListener(_CONTROL, poll_stop_key)
        if listener.start():
            log(f"全局暂停/恢复：{live_cfg.pause_hotkey}；控制台 P 也可切换；停止键保持不变")
        else:
            log(f"暂停热键未启用：{_CONTROL.hotkey_error}。保持暂停，不发输入；"
                "可在本控制台按 P 继续，或修改 --pause-hotkey 后重启。")
        start_console_watcher()
        if not args.no_dashboard:
            spawn_dashboard()      # 实时状态面板（独立控制台窗口，失败不影响运行）
        if args.daemon:
            return run_daemon(args)
        return play(args)
    except WindowGoneError as exc:
        log(str(exc))
        return 3
    except ControlInterrupted as exc:
        log(f"控制操作已取消：{exc}")
        status_write(state="stopped", alive=False, stop_reason=str(exc))
        return 0
    except ValueError as exc:
        log(f"配置无效：{exc}")
        return 2
    except KeyboardInterrupt:
        # Ctrl+C：press() 里的 finally 已经把鼠标放开，这里只负责干净地报一句再退出
        log("收到 Ctrl+C，已停止（鼠标已释放）。下次可用 F8 或控制台按 q 优雅停止。")
        return 130
    finally:
        _STOP_REQUESTED = True
        if _CONTROL is not None:
            _CONTROL.stop("进程退出")
        if listener is not None:
            listener.close()
        _CONTROL = None
        restore_timer_resolution()


def run_daemon(args) -> int:
    """长跑模式：一轮结束后自动等窗口回来，再开下一轮。

    只负责"窗口回来了就继续跑"，**不做**自动重开微信小程序——那需要点击微信的
    搜索框/最近使用列表，一旦点错会弹出分享面板等不可恢复的状态，代价远大于收益。
    退出条件：用户按 F8（急停并且不再续跑）或出现不可恢复错误。
    """
    # 3754 行要读 play() 写下的全局停止原因；3749 行的赋值若没有 global，
    # Python 会把 _LAST_STOP_REASON 当成 run_daemon 的局部变量，导致
    # "一轮正常跑完"时在读取处抛 UnboundLocalError（实测 2026-10-01 500 跳一轮后
    # daemon 直接崩溃退出，日志里没有 [daemon] 本轮结束 那行）。
    global _LAST_STOP_REASON
    rounds = 0
    while True:
        try:
            rc = play(args)
        except WindowGoneError as exc:
            # 窗口在回合中途消失时，play() 内部会抛 WindowGoneError：对 daemon 来说
            # 这不是错误，而是"等它回来再继续"的信号。
            log(f"[daemon] 本轮因窗口消失中断：{exc}")
            _LAST_STOP_REASON = "窗口消失（等待重开）"
            rc = 3
        if rc == 0:
            rounds += 1            # 只把"真正跑完的轮次"计入 --daemon-max-rounds
        reason = _LAST_STOP_REASON
        log(f"[daemon] 本轮结束：{reason or '窗口不在'}（rc={rc}，已完成 {rounds} 轮）")
        if poll_stop_key() or ("F8" in reason) or ("中断" in reason) or ("用户停止" in reason):
            log("[daemon] 用户主动停止，daemon 一并退出")
            return rc
        if args.daemon_max_rounds and rounds >= args.daemon_max_rounds:
            log(f"[daemon] 已跑满 {rounds} 轮，退出")
            return rc
        if rc not in (0, 3):
            log("[daemon] 不可恢复错误，退出")
            return rc
        # The next play owns its wait/pause/countdown gate; do not duplicate it
        # here (otherwise daemon waits could not acknowledge a pause).
        args._next_startup_wait_s = args.daemon_wait_s
        log("[daemon] 下一轮等待由统一暂停/启动控制接管；窗口不在时只等用户打开")


if __name__ == "__main__":
    raise SystemExit(main())
