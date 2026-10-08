"""Pause/startup tests: fake windows/input, never interact with a game."""
from contextlib import ExitStack
import os
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ["JUMPBOT_NO_INPUT"] = "1"
from run_control import RunControl, ControlInterrupted, WindowsHotkeyListener, parse_hotkey  # noqa: E402


class Clock:
    def __init__(self):
        self.value = 100.0

    def now(self):
        return self.value

    def sleep(self, seconds):
        self.value += max(0, seconds)


class ControlTests(unittest.TestCase):
    def test_hotkey_contract(self):
        self.assertEqual(parse_hotkey('Alt+P'), (0x4001, 80))
        self.assertEqual(parse_hotkey('Ctrl+Shift+8'), (0x4006, 56))
        for key in ['F8', 'Alt+F8', 'Alt+Q', 'Ctrl+C', 'Win+P', 'P', 'Alt+Esc', 'Alt+']:
            with self.subTest(key=key), self.assertRaises(ValueError):
                parse_hotkey(key)

    def test_pause_does_not_shorten_owned_input(self):
        c = RunControl()
        with c.input_operation(c.generation):
            c.toggle()
            c.acknowledge_pause()
            self.assertIsNone(c.paused_at)
            self.assertEqual(c.inputs, 1)
        c.acknowledge_pause()
        self.assertIsNotNone(c.paused_at)
        with self.assertRaises(ControlInterrupted):
            with c.input_operation():
                self.fail('input while paused')

    def test_pause_clock_excludes_long_pause_and_resume_preparation(self):
        clock = Clock()
        c = RunControl(clock=clock.now)
        start = c.active_time()
        clock.sleep(2)
        c.toggle()
        c.acknowledge_pause()
        clock.sleep(180)
        self.assertEqual(c.active_time() - start, 2)
        c.toggle()
        clock.sleep(3)
        c.finish_resume(c.generation)
        self.assertEqual(c.active_time() - start, 2)
        self.assertEqual(c.snapshot()['pause_total_s'], 183)

    def test_stop_is_latched_and_has_priority(self):
        c = RunControl()
        c.toggle()
        c.stop('F8')
        generation = c.generation
        for _ in range(50):
            c.toggle()
        self.assertEqual(c.generation, generation)
        self.assertTrue(c.stopped)
        with self.assertRaises(ControlInterrupted):
            c.finish_resume(generation)

    def test_old_generation_and_fast_toggle_cannot_press(self):
        c = RunControl()
        old = c.generation
        c.toggle()
        c.toggle()
        for generation in [old, c.generation]:
            with self.assertRaises(ControlInterrupted):
                with c.input_operation(generation):
                    self.fail('old frame reused')
        c.finish_resume(c.generation)
        with c.input_operation(c.generation):
            pass

    def test_hotkey_failure_defaults_to_no_input(self):
        c = RunControl()
        c.fail_hotkey('already registered')
        self.assertTrue(c.pause_requested)
        with self.assertRaises(ControlInterrupted):
            with c.input_operation():
                self.fail('silent launch')
        c.toggle('console P')
        c.finish_resume(c.generation)
        with c.input_operation():
            pass

    def test_input_exception_releases_lease(self):
        c = RunControl()
        with self.assertRaises(OSError):
            with c.input_operation():
                raise OSError('input failed')
        self.assertEqual(c.inputs, 0)

    def test_diagnostics_failure_does_not_break_controls(self):
        c = RunControl(notify=lambda **_: 1 / 0)
        c.toggle()
        c.acknowledge_pause()
        c.stop()
        self.assertTrue(c.stopped)

    def test_native_dispatch_serializes_concurrent_pause_commit(self):
        c = RunControl()
        started, done = threading.Event(), threading.Event()
        def request():
            started.set()
            c.toggle()
            done.set()
        with c.input_operation(c.generation):
            with c.input_dispatch(c.generation):
                thread = threading.Thread(target=request)
                thread.start()
                self.assertTrue(started.wait(1))
                self.assertFalse(done.wait(.02))
                self.assertFalse(c.pause_requested)
            thread.join(1)
            self.assertTrue(done.is_set())
            self.assertTrue(c.pause_requested)
        c.acknowledge_pause()
        with self.assertRaises(ControlInterrupted):
            with c.input_dispatch():
                self.fail('dispatch while paused')


@unittest.skipUnless(sys.platform == 'win32', 'Windows integration')
class ApplicationTests(unittest.TestCase):
    def setUp(self):
        import jumpbot as J
        self.j = J
        self.temp = tempfile.TemporaryDirectory(prefix='jumpbot-pause-tests-')
        self.addCleanup(self.temp.cleanup)
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.clock = Clock()
        self.control = RunControl(clock=self.clock.now)
        self.stack.enter_context(patch.object(J, '_CONTROL', self.control))
        self.stack.enter_context(patch.object(J, '_STOP_REQUESTED', False))
        self.stack.enter_context(patch.object(J.user32, 'GetAsyncKeyState', lambda _: 0))
        self.stack.enter_context(patch.object(J, 'DEBUG_DIR', Path(self.temp.name)))
        self.stack.enter_context(patch.object(J, 'STATUS_PATH', Path(self.temp.name) / 'status.json'))
        self.stack.enter_context(patch.object(J, '_STATUS', {}))
        self.stack.enter_context(patch.object(J, 'log', lambda _: None))
        self.stack.enter_context(patch('jumpbot.time.monotonic', self.clock.now))
        self.stack.enter_context(patch('jumpbot.time.sleep', self.clock.sleep))
        self.win = SimpleNamespace(hwnd=123, pid=456, title='fake', width=553,
                                   height=1050, client_rect=(0, 0, 553, 1050))
        self.cfg = J.Config(window_size_policy='off', start_delay_s=3)
        self.stack.enter_context(patch.object(J, 'foreground_hwnd', lambda: self.win.hwnd))

    def fake_event(self, callback):
        self.control.changed = SimpleNamespace(set=lambda: None, clear=lambda: None,
                                               wait=lambda _: callback())

    def test_startup_waits_for_manual_window_and_countdown(self):
        J = self.j
        calls = []
        def windows(*_):
            return [self.win] if self.clock.value >= 101 else []
        self.stack.enter_context(patch.object(J, 'find_windows', windows))
        self.stack.enter_context(patch.object(J, 'force_foreground', lambda h: calls.append(h) or True))
        result = J.control_ready_window(self.cfg, initial=True)
        self.assertEqual(result.hwnd, 123)
        self.assertGreaterEqual(self.clock.value, 104)
        self.assertEqual(calls, [123])
        self.assertFalse(list(Path(self.temp.name).glob('run_*.csv')))

    def test_wait_timeout_has_no_activation(self):
        J = self.j
        self.stack.enter_context(patch.object(J, 'find_windows', lambda *_: []))
        self.stack.enter_context(patch.object(J, 'force_foreground', side_effect=AssertionError('activation')))
        with self.assertRaises(J.WindowGoneError):
            J.control_ready_window(self.cfg, timeout_s=.2)

    def test_pause_wait_has_no_window_scan_or_activation(self):
        J = self.j
        self.control.toggle()
        self.stack.enter_context(patch.object(J, 'find_windows', side_effect=AssertionError('scan while paused')))
        self.stack.enter_context(patch.object(J, 'force_foreground', side_effect=AssertionError('activation')))
        def stop_after_long_pause():
            self.clock.sleep(180)
            self.assertEqual(self.control.active_time(), 100)
            self.control.stop('Esc')
        self.fake_event(stop_after_long_pause)
        with self.assertRaises(ControlInterrupted):
            J.control_ready_window(self.cfg, initial=True)
        self.assertEqual(J._STATUS['state'], 'paused')

    def test_resume_reacquires_window_and_discards_prior(self):
        J = self.j
        calls = []
        self.control.toggle()
        self.fake_event(lambda: (self.clock.sleep(120), self.control.toggle()))
        self.stack.enter_context(patch.object(J, 'find_windows', lambda *_: [self.win]))
        self.stack.enter_context(patch.object(J, 'force_foreground', lambda h: calls.append(h) or True))
        vision = SimpleNamespace(reset_prior=lambda: calls.append('reset'))
        self.assertTrue(J.control_checkpoint(self.cfg, vision))
        self.assertEqual(calls, [123, 'reset'])
        self.assertAlmostEqual(self.control.snapshot()['pause_total_s'], 123, places=2)
        self.assertFalse(self.control.needs_resume)

    def test_pause_before_down_does_not_emit_down(self):
        J = self.j
        events = []
        ctrl = J.InputController()
        ctrl.expected_generation = self.control.generation
        self.stack.enter_context(patch.dict(os.environ, {'JUMPBOT_NO_INPUT': '0'}))
        self.stack.enter_context(patch.object(J, 'point_is_window', lambda *_: True))
        self.stack.enter_context(patch.object(J, '_send_mouse', lambda flags, *xy: events.append(flags)))
        self.stack.enter_context(patch('jumpbot.time.sleep', lambda _: self.control.toggle()))
        with self.assertRaises(ControlInterrupted):
            ctrl.press(self.win, .5, .72, .6)
        self.assertEqual(events, [0])  # permitted initial move, no DOWN
        self.assertEqual(self.control.inputs, 0)

    def test_pause_during_hold_finishes_release_once(self):
        J = self.j
        events = []
        ctrl = J.InputController()
        ctrl.expected_generation = self.control.generation
        self.stack.enter_context(patch.dict(os.environ, {'JUMPBOT_NO_INPUT': '0'}))
        self.stack.enter_context(patch.object(J, 'point_is_window', lambda *_: True))
        self.stack.enter_context(patch.object(J, '_send_mouse', lambda flags, *xy: events.append(flags)))
        self.stack.enter_context(patch.object(J, 'hold_precise', lambda _: self.control.toggle()))
        ctrl.press(self.win, .5, .72, .6)
        self.assertEqual(events, [0, J.MOUSEEVENTF_LEFTDOWN, J.MOUSEEVENTF_LEFTUP])
        self.control.acknowledge_pause()
        with self.assertRaises(ControlInterrupted):
            ctrl.press(self.win, .5, .72, .6)
        self.assertEqual(len(events), 3)

    def test_pause_after_final_target_check_does_not_dispatch_down(self):
        J = self.j
        events, checks = [], []
        def point(*_):
            checks.append(1)
            if len(checks) == 2:
                self.control.toggle('last-target-check race')
            return True
        self.stack.enter_context(patch.dict(os.environ, {'JUMPBOT_NO_INPUT': '0'}))
        self.stack.enter_context(patch.object(J, 'point_is_window', point))
        self.stack.enter_context(patch.object(J, '_send_mouse', lambda flags, *xy: events.append(flags)))
        ctrl = J.InputController()
        ctrl.expected_generation = self.control.generation
        with self.assertRaises(ControlInterrupted):
            ctrl.press(self.win, .5, .72, .6)
        self.assertEqual(events, [0])
        self.assertEqual(self.control.inputs, 0)

    def test_background_move_pause_does_not_dispatch_down(self):
        J = self.j
        events = []
        def post(_hwnd, message, *_):
            events.append(message)
            if message == J.WM_MOUSEMOVE:
                self.control.toggle('background dispatch race')
            return True
        self.stack.enter_context(patch.dict(os.environ, {'JUMPBOT_NO_INPUT': '0'}))
        self.stack.enter_context(patch.object(J.user32, 'PostMessageW', post))
        ctrl = J.InputController(background=True)
        ctrl.expected_generation = self.control.generation
        with self.assertRaises(ControlInterrupted):
            ctrl.press(self.win, .5, .72, .6)
        self.assertEqual(events, [J.WM_MOUSEMOVE])
        self.assertEqual(self.control.inputs, 0)

    def test_hold_exception_still_releases(self):
        J = self.j
        events = []
        self.stack.enter_context(patch.dict(os.environ, {'JUMPBOT_NO_INPUT': '0'}))
        self.stack.enter_context(patch.object(J, 'point_is_window', lambda *_: True))
        self.stack.enter_context(patch.object(J, '_send_mouse', lambda flags, *xy: events.append(flags)))
        self.stack.enter_context(patch.object(J, 'hold_precise', side_effect=KeyboardInterrupt))
        with self.assertRaises(KeyboardInterrupt):
            J.InputController().press(self.win, .5, .72, .6)
        self.assertEqual(events[-1], J.MOUSEEVENTF_LEFTUP)
        self.assertEqual(self.control.inputs, 0)

    def test_down_dispatch_exception_still_attempts_release(self):
        J = self.j
        events = []
        def send(flags, *xy):
            events.append(flags)
            if flags == J.MOUSEEVENTF_LEFTDOWN:
                raise KeyboardInterrupt
        self.stack.enter_context(patch.dict(os.environ, {'JUMPBOT_NO_INPUT': '0'}))
        self.stack.enter_context(patch.object(J, 'point_is_window', lambda *_: True))
        self.stack.enter_context(patch.object(J, '_send_mouse', send))
        with self.assertRaises(KeyboardInterrupt):
            J.InputController().press(self.win, .5, .72, .6)
        self.assertEqual(events, [0, J.MOUSEEVENTF_LEFTDOWN, J.MOUSEEVENTF_LEFTUP])
        self.assertEqual(self.control.inputs, 0)

    def test_pause_landing_loss_is_unknown_not_death(self):
        J = self.j
        self.control.toggle()
        self.stack.enter_context(patch.object(J, 'find_windows', lambda *_: []))
        cap = SimpleNamespace(grab=lambda _: self.fail('capture without window'))
        result = J.wait_landing(cap, self.win, SimpleNamespace(), self.cfg)
        self.assertEqual(result, ('pause_unknown', None, None, None, None))

    def test_pause_blocks_foreground_size_and_acquisition(self):
        J = self.j
        self.control.toggle()
        self.control.acknowledge_pause()
        for action in [lambda: J.force_foreground(123),
                       lambda: J.ensure_window_size(self.win, self.cfg),
                       lambda: J.acquire_frame(None, self.cfg, None, None, False),
                       lambda: J.ensure_playing(None, self.cfg, None, None, False)]:
            with self.assertRaises(ControlInterrupted):
                action()

    def test_recent_stats_do_not_treat_unknown_as_landed(self):
        J = self.j
        rows = [SimpleNamespace(result='landed', note='[center x1]') for _ in range(99)]
        rows += [SimpleNamespace(result='landed', note='[miss]'),
                 SimpleNamespace(result='unknown', note='暂停收尾')]
        result = J.accuracy_fields(rows)
        self.assertEqual(result['attempts'], 101)
        self.assertEqual(result['landed'], 100)
        self.assertEqual(result['recent100']['hits'], 98)
        self.assertEqual(result['unknown'], 1)

    def simulate_play(self, count, pause_every=0, unknown_every=0, pause_restart=False):
        """Entire play caller, synthetic scoreboard, fake input/window/capture."""
        import numpy as np
        J = self.j
        frame = np.zeros((1050, 553, 3), dtype=np.uint8)
        self.win.describe = lambda: 'fake window'
        counters = dict(presses=0, score=0, reads=0, captures=0, activations=0)
        piece = SimpleNamespace(x=200., y=800., bbox=(180, 700, 40, 100))
        target = SimpleNamespace(x=400., y=684.52, bbox=(320, 630, 160, 80),
                                 top_width=140., dot=None)
        cfg = J.Config(k=2.61, gamma=0, max_jumps=count, max_runtime_s=100000,
                       window_size_policy='off', start_delay_s=0,
                       masks_backend='simd', center_dot_max_offset_px=0)
        class FakeVision:
            _auto_backend = 'simd'
            def __init__(self, _):
                pass
            def reset_prior(self):
                pass
            def detect(self, _):
                return piece, target, None, None
        owner = self
        class FakeCapture:
            def __init__(self, _):
                pass
            def grab(self, _):
                owner.assertFalse(owner.control.pause_requested and owner.control.paused_at is not None)
                counters['captures'] += 1
                return frame
            def close(self):
                pass
        class FakeInput:
            expected_generation = None
            def __init__(self, **_):
                pass
            def press(self, win, fx, fy, hold_s):
                with owner.control.input_operation(self.expected_generation):
                    counters['presses'] += 1
                    counters['score'] += min(2 * counters['presses'], 32)
                    owner.clock.sleep(hold_s)
                    if pause_every and counters['presses'] % pause_every == 0:
                        owner.control.toggle('test pause')
            def click(self, *_):
                raise AssertionError('unexpected start/restart click')
        def resume():
            owner.clock.sleep(180)
            owner.control.toggle('test resume')
        def landing(*_, **__):
            if pause_restart and counters['presses'] == 2:
                return 'fixture_dead', None, None, None, None
            if unknown_every and counters['presses'] % unknown_every == 0:
                return 'pause_unknown', None, None, None, None
            return 'landed', frame, piece, None, None
        config_path = Path(self.temp.name) / 'config.json'
        config_path.write_text(json.dumps({'k': 2.61, 'gamma': 0}), encoding='utf8')
        self.fake_event(resume)
        original_playing = J.ensure_playing
        def playing(*values, **options):
            if pause_restart and counters['presses'] == 2 and not counters.get('restart_pause'):
                import csv
                path = next(Path(self.temp.name).glob('run_*.csv'))
                with path.open(encoding='utf-8-sig', newline='') as stream:
                    rows = list(csv.DictReader(stream))
                owner.assertEqual(rows[-1]['result'], 'dead')
                counters['restart_pause'] = True
                owner.control.toggle('pause during restart')
                raise ControlInterrupted('restart cancelled')
            return original_playing(*values, **options)
        with ExitStack() as stack:
            stack.enter_context(patch.object(J.Config, 'load', lambda *_: cfg))
            stack.enter_context(patch.object(J, 'find_windows', lambda *_: [self.win]))
            stack.enter_context(patch.object(J, 'force_foreground', lambda _: True))
            stack.enter_context(patch.object(J, 'point_is_window', lambda *_: True))
            stack.enter_context(patch.object(J, 'Capturer', FakeCapture))
            stack.enter_context(patch.object(J, 'Vision', FakeVision))
            stack.enter_context(patch.object(J, 'InputController', FakeInput))
            stack.enter_context(patch.object(J, 'classify_screen', lambda _: ('playing', None, 1)))
            stack.enter_context(patch.object(J, 'is_blocking_modal', lambda *_: False))
            stack.enter_context(patch.object(J, 'match_ui', lambda *_: None))
            stack.enter_context(patch.object(J, 'wait_landing', landing))
            stack.enter_context(patch.object(J, 'ensure_playing', playing))
            stack.enter_context(patch.object(J, 'read_score', lambda *_:
                                             None if pause_restart and counters['presses'] == 2
                                             else counters['score']))
            stack.enter_context(patch.object(J, 'LOG_DIR', Path(self.temp.name)))
            args = J.build_parser().parse_args(['--config', str(config_path), '--max-jumps', str(count)])
            self.assertEqual(J.play(args), 0)
        self.assertEqual(counters['presses'], count)
        self.assertEqual(J._STATUS['attempts'], count)
        self.assertEqual(J._STATUS['deaths'], 1 if pause_restart else 0)
        self.assertEqual(J._STATUS['jumps'], count)
        self.assertEqual(J._LAST_STOP_REASON, '达到最大跳跃次数')
        return J._STATUS.copy(), counters

    def test_known_death_is_committed_before_pause_cancels_restart(self):
        status, counters = self.simulate_play(3, pause_restart=True)
        self.assertTrue(counters['restart_pause'])
        self.assertEqual(status['deaths'], 1)
        self.assertEqual(status['landed'], 2)
        import csv
        with next(Path(self.temp.name).glob('run_*.csv')).open(encoding='utf-8-sig', newline='') as stream:
            self.assertEqual([r['result'] for r in csv.DictReader(stream)], ['landed', 'dead', 'landed'])

    def test_play_pause_integration_does_not_add_deaths_or_blind_jumps(self):
        status, _ = self.simulate_play(30, pause_every=3)
        self.assertEqual(status['landed'], 30)
        self.assertEqual(status['pause_count'], 9)
        self.assertGreaterEqual(status['pause_total_s'], 9 * 180)

    def test_play_unknown_pause_landing_is_recorded_without_restart(self):
        status, _ = self.simulate_play(30, pause_every=3, unknown_every=3)
        self.assertEqual(status['landed'], 20)
        self.assertEqual(status['attempts'], 30)
        self.assertGreaterEqual(status['unknown'], 10)

    def test_three_long_non_live_rounds(self):
        # Numeric scoreboard is synthetic; this is NOT measured game accuracy.
        for count in (500, 1000, 2000):
            with self.subTest(round_size=count):
                self.control = RunControl(clock=self.clock.now)
                self.j._CONTROL = self.control
                status, _ = self.simulate_play(count, pause_every=50)
                self.assertEqual(status['landed'], count)
                self.assertEqual(status['pause_count'], count // 50 - 1)


@unittest.skipUnless(sys.platform == 'win32', 'native Windows hotkey ABI')
class NativeHotkeyTests(unittest.TestCase):
    def test_message_delivery_and_stop_priority(self):
        import ctypes
        import time
        from ctypes import wintypes
        control = RunControl('Alt+Shift+9')
        listener = WindowsHotkeyListener(control, lambda: None)
        user = ctypes.WinDLL('user32', use_last_error=True)
        user.PostThreadMessageW.argtypes = [wintypes.DWORD, wintypes.UINT,
                                          wintypes.WPARAM, wintypes.LPARAM]
        user.PostThreadMessageW.restype = wintypes.BOOL
        try:
            self.assertTrue(listener.start(), control.hotkey_error)
            self.assertTrue(user.PostThreadMessageW(listener.thread.native_id, 0x0312, 0x4A50, 0))
            deadline = time.monotonic() + 2
            while not control.pause_requested and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(control.pause_requested)
            control.stop('F8 test')
            self.assertEqual(control.stop_reason, 'F8 test')
            control.toggle()
            self.assertTrue(control.stopped)
        finally:
            listener.close()

    def test_registration_conflict_and_cleanup(self):
        # These listeners never instantiate an InputController or send keys.
        first = RunControl('Alt+Shift+9')
        second = RunControl('Alt+Shift+9')
        a = WindowsHotkeyListener(first, lambda: None)
        b = WindowsHotkeyListener(second, lambda: None)
        try:
            self.assertTrue(a.start(), first.hotkey_error)
            self.assertFalse(b.start())
            self.assertTrue(second.pause_requested)
        finally:
            b.close()
            a.close()
        self.assertFalse(a.thread.is_alive())
        third = RunControl('Alt+Shift+9')
        c = WindowsHotkeyListener(third, lambda: None)
        try:
            self.assertTrue(c.start(), third.hotkey_error)
        finally:
            c.close()


if __name__ == '__main__':
    unittest.main()
