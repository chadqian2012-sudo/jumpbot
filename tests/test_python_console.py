"""Exercise P on both private Windows consoles. Never focus a window or inject desktop input."""
import ctypes
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


def write_console_key(pid, key, repeats):
    from ctypes import wintypes
    k = ctypes.WinDLL('kernel32', use_last_error=True)
    k.FreeConsole()
    if not k.AttachConsole(int(pid)):
        raise ctypes.WinError(ctypes.get_last_error())
    k.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                              ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    k.CreateFileW.restype = wintypes.HANDLE
    h = k.CreateFileW('CONIN$', 0x40000000, 3, None, 3, 0, None)
    if h == wintypes.HANDLE(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    vk = ord(key.upper())
    records = [(True, 1)]
    if int(repeats) > 1:
        records.append((True, int(repeats) - 1))
    records.append((False, 1))
    events = b''.join(struct.pack('<H2xiHHHHI', 1, down, count, vk, 0, ord(key), 0)
                      for down, count in records)
    written = wintypes.DWORD()
    if not k.WriteConsoleInputW(wintypes.HANDLE(h), events, len(records), ctypes.byref(written)):
        raise ctypes.WinError(ctypes.get_last_error())
    assert written.value == len(records)


def worker(folder):
    sys.path.insert(0, str(ROOT))
    import jumpbot as j
    from unittest.mock import patch
    folder = Path(folder)
    j.DEBUG_DIR = j.LOG_DIR = folder
    j.STATUS_PATH = folder / 'status.json'
    real_popen = subprocess.Popen

    def hidden_panel(args, **kwargs):
        si = kwargs.setdefault('startupinfo', subprocess.STARTUPINFO())
        si.dwFlags |= subprocess.STARTF_USESHOWWINDOW
        si.wShowWindow = 0
        child = real_popen(args, **kwargs)
        (folder / 'panel.pid').write_text(str(child.pid))
        return child

    with patch.object(j, 'find_windows', return_value=[]), patch.object(subprocess, 'Popen', hidden_panel):
        return j.main(['--config', str(folder / 'config.json'), '--startup-wait-s', '30',
                         '--pause-hotkey', 'Ctrl+Shift+9'])


@unittest.skipUnless(sys.platform == 'win32', 'Windows console input buffers')
class ConsoleTests(unittest.TestCase):
    def test_pause_and_resume_in_both_consoles(self):
        hidden = subprocess.STARTUPINFO()
        hidden.dwFlags |= subprocess.STARTF_USESHOWWINDOW
        hidden.wShowWindow = 0
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            with subprocess.Popen([sys.executable, __file__, 'worker', tmp], cwd=ROOT,
                                  env={**os.environ, 'JUMPBOT_NO_INPUT': '1', 'PYTHONUTF8': '1'},
                                  creationflags=subprocess.CREATE_NEW_CONSOLE, startupinfo=hidden) as child:
                def state(expected, generation=None):
                    deadline = time.monotonic() + 10
                    latest = {}
                    while time.monotonic() < deadline and child.poll() is None:
                        try:
                            latest = json.loads((folder / 'status.json').read_text(encoding='utf8'))
                        except OSError:
                            pass
                        if latest.get('state') == expected and (
                                generation is None or latest.get('control_generation') == generation):
                            return
                        time.sleep(.05)
                    self.fail(f'expected {expected}, generation {generation}; got {latest}, exit={child.poll()}')

                def key(pid, char, count=1):
                    subprocess.run([sys.executable, __file__, 'key', str(pid), char, str(count)],
                                   creationflags=subprocess.CREATE_NO_WINDOW, check=True, timeout=10)

                try:
                    state('waiting_window')
                    panel = int((folder / 'panel.pid').read_text())
                    import _winapi
                    panel_handle = _winapi.OpenProcess(0x00100000, False, panel)
                    self.addCleanup(_winapi.CloseHandle, panel_handle)
                    key(child.pid, 'p')
                    state('paused', 1)
                    key(child.pid, 'P')
                    state('waiting_window', 2)
                    key(panel, 'p', 4)
                    state('paused', 3)
                    key(panel, 'P')
                    state('waiting_window', 4)
                    key(child.pid, 'q')
                    rc = child.wait(timeout=18)
                    self.assertEqual(rc, 0, (folder / 'worker.log').read_text(encoding='utf8'))
                    self.assertEqual(_winapi.WaitForSingleObject(panel_handle, 25000), 0,
                                     'dashboard did not exit after its owner stopped')
                    self.assertEqual(json.loads((folder / 'status.json').read_text(
                        encoding='utf8'))['jumps'], 0)
                finally:
                    if child.poll() is None:
                        key(child.pid, 'q')
                        child.wait(timeout=18)


if __name__ == '__main__':
    if sys.argv[1:2] == ['key']:
        write_console_key(*sys.argv[2:])
    elif sys.argv[1:2] == ['worker']:
        import traceback
        with (Path(sys.argv[2]) / 'worker.log').open('w', encoding='utf8') as output:
            sys.stderr = output
            try:
                raise SystemExit(worker(sys.argv[2]))
            except Exception:
                traceback.print_exc()
                raise
    else:
        unittest.main()
