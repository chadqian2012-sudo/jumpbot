"""Cooperative pause/stop state and a Windows hotkey listener.

This module is independent of vision and never emits mouse input.  The owner
thread acknowledges pauses only after an outstanding input operation finishes.
"""
from __future__ import annotations

from contextlib import contextmanager
import math
import threading
import time


class ControlInterrupted(RuntimeError):
    """An unstarted operation was cancelled; not a recognition/game failure."""


def parse_hotkey(text: str) -> tuple[int, int]:
    parts = str(text).upper().replace(" ", "").split("+")
    modifiers = {"ALT": 1, "CTRL": 2, "CONTROL": 2, "SHIFT": 4}
    if len(parts) < 2 or not all(p in modifiers for p in parts[:-1]):
        raise ValueError("暂停热键应为 Alt+P 等修饰键+字母/数字组合")
    key = parts[-1]
    if len(key) != 1 or not ("A" <= key <= "Z" or "0" <= key <= "9"):
        raise ValueError("暂停热键末键只支持字母/数字，不使用 F8/Esc 等停止键")
    mask = 0
    for modifier in parts[:-1]:
        mask |= modifiers[modifier]
    if key == "Q" or (key == "C" and mask & 2):
        raise ValueError("暂停热键不能与 q / Ctrl+C 停止键冲突")
    return mask | 0x4000, ord(key)  # MOD_NOREPEAT


def validate_timing(value: float, name: str) -> float:
    result = float(value)
    if not math.isfinite(result) or result < 0:
        raise ValueError(f"{name} 必须是有限的非负秒数")
    return result


class RunControl:
    def __init__(self, hotkey="Alt+P", clock=time.monotonic, notify=None):
        parse_hotkey(hotkey)
        self.hotkey = hotkey
        self.clock = clock
        self.notify = notify or (lambda **fields: None)
        self.lock = threading.RLock()
        self.changed = threading.Event()
        self.stopped = False
        self.stop_reason = ""
        self.pause_requested = False
        self.paused_at = None
        self.paused_total = 0.0
        self.needs_resume = False
        self.generation = 0
        self.inputs = 0
        self.hotkey_registered = False
        self.hotkey_error = ""
        self.pause_count = 0

    def _event(self, event, **fields):
        self.changed.set()
        try:
            self.notify(event=event, monotonic=self.clock(), **fields)
        except Exception:
            pass  # diagnostic output cannot break input-release/control safety

    def toggle(self, source="hotkey"):
        with self.lock:
            if self.stopped:
                return
            self.pause_requested = not self.pause_requested
            self.needs_resume = True
            self.generation += 1
            self._event("pause_requested" if self.pause_requested else "resume_requested",
                        source=source, generation=self.generation)

    def stop(self, reason="用户停止"):
        with self.lock:
            if not self.stopped:
                self.stopped = True
                self.stop_reason = reason
                self.generation += 1
                self._event("stop_requested", reason=reason)

    def fail_hotkey(self, error):
        with self.lock:
            self.hotkey_error = str(error)
            self.pause_requested = True
            self.needs_resume = True
            self.generation += 1
            self._event("hotkey_unavailable", error=str(error))

    def acknowledge_pause(self):
        with self.lock:
            if self.pause_requested and self.inputs == 0 and self.paused_at is None:
                self.paused_at = self.clock()
                self.pause_count += 1
                self._event("paused", pause_count=self.pause_count)

    def finish_resume(self, generation):
        with self.lock:
            self.check(generation)
            if self.paused_at is not None:
                self.paused_total += self.clock() - self.paused_at
                self.paused_at = None
            self.needs_resume = False
            self._event("ready", generation=generation)

    def active_time(self):
        with self.lock:
            now = self.clock()
            return now - self.paused_total - (now - self.paused_at
                                             if self.paused_at is not None else 0)

    def check(self, generation=None):
        with self.lock:
            if self.stopped or self.pause_requested or (
                    generation is not None and generation != self.generation):
                raise ControlInterrupted(self.stop_reason if self.stopped else "暂停/恢复请求")

    @contextmanager
    def input_operation(self, generation=None):
        with self.lock:
            self.check(generation)
            if self.needs_resume or self.paused_at is not None:
                raise ControlInterrupted("恢复尚未完成")
            self.inputs += 1
        try:
            yield
        finally:
            with self.lock:
                self.inputs -= 1

    @contextmanager
    def input_dispatch(self, generation=None):
        """Linearize admission and native DOWN against pause/stop requests.

        Hold this only for the native dispatch, never for the mouse hold or
        screenshot/recognition. Once dispatched, UP must remain unconditional.
        """
        with self.lock:
            self.check(generation)
            if self.needs_resume or self.paused_at is not None:
                raise ControlInterrupted("恢复尚未完成")
            yield

    def snapshot(self):
        with self.lock:
            paused_s = self.paused_total
            if self.paused_at is not None:
                paused_s += self.clock() - self.paused_at
            return dict(paused=self.pause_requested, pause_total_s=round(paused_s, 3),
                        pause_count=self.pause_count, pause_hotkey=self.hotkey,
                        hotkey_registered=self.hotkey_registered,
                        hotkey_error=self.hotkey_error,
                        control_generation=self.generation,
                        input_active=bool(self.inputs))


class WindowsHotkeyListener:
    """Message pump owns registration/unregistration on the same thread."""
    def __init__(self, control: RunControl, poll_stop):
        self.control = control
        self.poll_stop = poll_stop
        self.closed = threading.Event()
        self.ready = threading.Event()
        self.thread = threading.Thread(target=self._run, name="pause-hotkey", daemon=True)

    def start(self):
        self.thread.start()
        if not self.ready.wait(3):
            self.closed.set()
            self.control.fail_hotkey("注册线程未就绪")
        return self.control.hotkey_registered

    def close(self):
        self.closed.set()
        if self.thread.is_alive():
            self.thread.join(timeout=2)

    def _run(self):
        import ctypes
        from ctypes import wintypes
        registered = False
        hotkey_id = 0x4A50
        try:
            user = ctypes.WinDLL("user32", use_last_error=True)
            user.RegisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int,
                                           wintypes.UINT, wintypes.UINT]
            user.RegisterHotKey.restype = wintypes.BOOL
            user.UnregisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int]
            user.UnregisterHotKey.restype = wintypes.BOOL
            user.PeekMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND,
                                         wintypes.UINT, wintypes.UINT, wintypes.UINT]
            user.PeekMessageW.restype = wintypes.BOOL
            modifiers, key = parse_hotkey(self.control.hotkey)
            registered = bool(user.RegisterHotKey(None, hotkey_id, modifiers, key))
            if not registered:
                raise OSError(ctypes.get_last_error(), "暂停热键已被占用或无法注册")
            self.control.hotkey_registered = True
            self.ready.set()
            message = wintypes.MSG()
            while not self.closed.is_set() and not self.control.stopped:
                self.poll_stop()
                while user.PeekMessageW(ctypes.byref(message), None, 0, 0, 1):
                    if message.message == 0x0312 and message.wParam == hotkey_id:
                        self.control.toggle(self.control.hotkey)
                self.closed.wait(0.02)
        except Exception as exc:
            self.control.fail_hotkey(exc)
        finally:
            if registered:
                user.UnregisterHotKey(None, hotkey_id)
            self.control.hotkey_registered = False
            self.ready.set()
