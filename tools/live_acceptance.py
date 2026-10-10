"""Opt-in live acceptance through the application's normal closed-loop runner.

Never opens WeChat. Native messages target only our own hotkey listener; they
are programmatic hotkey-path checks, not evidence of physical-key auto-repeat
or another application's compatibility. Human pause requests are never resumed
by the test driver. All live input remains in jumpbot.InputController.
"""
from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import sys
import threading
import time
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import jumpbot as J  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute-live', action='store_true', help='explicitly allow real game input')
    parser.add_argument('--jumps', type=int, default=500)
    parser.add_argument('--backend', choices=['simd', 'auto', 'gpu', 'opencl'], default='simd')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--pause-every', type=int, default=50)
    args = parser.parse_args()
    if not args.execute_live or os.environ.get('JUMPBOT_NO_INPUT') == '1':
        parser.error('live input requires --execute-live and JUMPBOT_NO_INPUT must not be 1')
    if args.jumps < 1 or args.pause_every < 1:
        parser.error('positive jump counts required')
    args.out.mkdir(parents=True, exist_ok=True)
    hashes = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
              for name in ['jumpbot.py', 'run_control.py', 'skin_vision.py',
                           'config.json', 'tools/live_acceptance.py']}
    audit_lock = threading.Lock()
    finished = threading.Event()
    counts = dict(mouse_events=0, down=0, up=0, foreground_calls=0, capture_calls=0,
                  paused_violations=0, automated_pause_cycles=0)
    latest = {}
    captured_control = []
    audit_file = (args.out / 'input-audit.jsonl').open('w', encoding='utf8')

    def audit(kind, **fields):
        with audit_lock:
            audit_file.write(json.dumps(dict(kind=kind, monotonic=time.monotonic(), **fields),
                                        ensure_ascii=False) + '\n')
            audit_file.flush()

    def guard(kind):
        control = J._CONTROL
        if control is not None and control.paused_at is not None and control.pause_requested:
            counts['paused_violations'] += 1
            audit('SAFETY_VIOLATION', operation=kind, control=control.snapshot())
            control.stop('验收发现已暂停期间有操作请求')
            raise J.ControlInterrupted('paused acceptance guard')

    original_mouse = J._send_mouse
    original_foreground = J.force_foreground
    original_grab = J.Capturer.grab
    original_status = J.status_write
    listener_type = J.WindowsHotkeyListener
    listener = []

    def mouse(flags, *values):
        guard('mouse')
        counts['mouse_events'] += 1
        counts['down'] += bool(flags & J.MOUSEEVENTF_LEFTDOWN)
        counts['up'] += bool(flags & J.MOUSEEVENTF_LEFTUP)
        audit('mouse', flags=flags, coordinates=values,
              control=J._CONTROL.snapshot() if J._CONTROL else {})
        return original_mouse(flags, *values)

    def foreground(*values, **options):
        guard('foreground')
        counts['foreground_calls'] += 1
        audit('foreground', hwnd=values[0])
        return original_foreground(*values, **options)

    def grab(self, win):
        guard('capture')
        counts['capture_calls'] += 1
        return original_grab(self, win)

    def status(**fields):
        original_status(**fields)
        latest.update(J._STATUS)

    def make_listener(control, poll_stop):
        instance = listener_type(control, poll_stop)
        listener.append(instance)
        captured_control.append(control)
        return instance

    user = ctypes.WinDLL('user32', use_last_error=True)
    user.PostThreadMessageW.argtypes = [wintypes.DWORD, wintypes.UINT,
                                       wintypes.WPARAM, wintypes.LPARAM]
    user.PostThreadMessageW.restype = wintypes.BOOL

    def request():
        if not listener or not listener[0].thread.is_alive():
            return False
        return bool(user.PostThreadMessageW(listener[0].thread.native_id, 0x0312, 0x4A50, 0))

    def drive():
        next_pause = args.pause_every
        while not finished.wait(0.05):
            if not captured_control:
                continue
            control = captured_control[0]
            if control.stopped:
                return
            # A hard bound: these counts already make the requested round's
            # 98%/95% gate mathematically unreachable, even with perfect rest.
            if (latest.get('deaths', 0) > args.jumps * 0.02 + 1e-9 or
                    latest.get('misses', 0) > args.jumps * 0.05 + 1e-9):
                audit('gate_unreachable', status=dict(latest))
                control.stop('验收门槛已不可达，停止本轮并保留证据')
                return
            if (latest.get('jumps', 0) < next_pause or control.pause_requested or
                    control.needs_resume or not control.hotkey_registered):
                continue
            generation = control.generation + 1
            if not request():
                audit('test_request_failed', error=ctypes.get_last_error())
                continue
            next_pause += args.pause_every
            deadline = time.monotonic() + 8
            while not finished.wait(0.05) and time.monotonic() < deadline:
                if control.stopped or control.generation != generation:
                    break
                if control.paused_at is not None:
                    audit('paused_confirmed', control=control.snapshot())
                    duration = 95 if counts['automated_pause_cycles'] == 0 else 3
                    end = time.monotonic() + duration
                    while not finished.wait(0.05) and time.monotonic() < end:
                        if control.stopped or control.generation != generation:
                            break
                    if (not finished.is_set() and not control.stopped and
                            control.pause_requested and control.generation == generation):
                        request()
                        counts['automated_pause_cycles'] += 1
                        audit('programmatic_resume_requested', duration_s=duration)
                    break

    worker = threading.Thread(target=drive, name='live-acceptance', daemon=True)
    argv = ['--max-jumps', str(args.jumps), '--backend', args.backend,
            '--save-debug', '--log-dir', str(args.out / 'logs')]
    if args.backend == 'simd':
        argv += ['--no-cuda', '--no-opencl']
    rc = None
    error = None
    try:
        with patch.object(J, '_send_mouse', mouse), patch.object(J, 'force_foreground', foreground), \
                patch.object(J.Capturer, 'grab', grab), patch.object(J, 'status_write', status), \
                patch.object(J, 'WindowsHotkeyListener', make_listener):
            worker.start()
            rc = J.main(argv)
    except BaseException as exc:
        error = repr(exc)
        raise
    finally:
        finished.set()
        worker.join(2)
        audit_file.close()
        report = dict(requested_jumps=args.jumps, source_sha256=hashes,
                      backend=args.backend, return_code=rc, exception=error,
                      counters=counts, final_status=latest,
                      physical_hotkey_repeat_verified=False,
                      controlled_window_switch_verified=False)
        (args.out / 'run-evidence.json').write_text(
            json.dumps(report, ensure_ascii=False, indent=2), encoding='utf8')
    return rc or 0


if __name__ == '__main__':
    raise SystemExit(main())
