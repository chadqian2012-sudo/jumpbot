"""Clock/display and zero-limit regressions; never use real game input."""
from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ['JUMPBOT_NO_INPUT'] = '1'
import jumpbot as J  # noqa: E402
from run_control import RunControl  # noqa: E402
from tools import dashboard as D  # noqa: E402


class DashboardUnlimitedTests(unittest.TestCase):
    def sample(self, **overrides):
        data = dict(state='playing', alive=True, active_elapsed_s=336.0,
                    updated_monotonic_s=100.0, elapsed_running=True,
                    elapsed_text='00:05:36', jumps=168, max_jumps=0)
        data.update(overrides)
        return data

    def test_clock_ticks_between_jumps_without_inventing_progress(self):
        status = self.sample()
        with patch.object(D.time, 'monotonic', return_value=101.25):
            text = D.render(status, None, 123, False)
        self.assertIn('00:05:37', text)
        self.assertIn('本轮跳数: 168（不限）', text)
        self.assertNotIn('/5000', text)
        self.assertEqual(status['jumps'], 168)
        self.assertEqual(status['active_elapsed_s'], 336)

    def test_pause_resume_preparation_and_stop_do_not_tick(self):
        for state in ('paused', 'resuming', 'stopped'):
            with self.subTest(state=state), patch.object(D.time, 'monotonic', return_value=280):
                self.assertEqual(D.elapsed_text(self.sample(state=state)), '00:05:36')
        with patch.object(D.time, 'monotonic', return_value=280):
            self.assertEqual(D.elapsed_text(self.sample(elapsed_running=False)), '00:05:36')
            self.assertEqual(D.elapsed_text(self.sample(alive=False)), '00:05:36')

    def test_old_status_and_invalid_checkpoints_remain_compatible(self):
        self.assertEqual(D.elapsed_text(dict(elapsed_text='00:05:36', started=1)), '00:05:36')
        for fields in (dict(active_elapsed_s=float('nan')), dict(active_elapsed_s=-1),
                       dict(updated_monotonic_s=float('inf')), dict(updated_monotonic_s=None)):
            with self.subTest(fields=fields):
                self.assertEqual(D.elapsed_text(self.sample(**fields)), '00:05:36')

    def test_stale_or_future_monotonic_checkpoint_is_bounded(self):
        with patch.object(D.time, 'monotonic', return_value=300):
            self.assertEqual(D.elapsed_text(self.sample()), '00:05:51')
        with patch.object(D.time, 'monotonic', return_value=99):
            self.assertEqual(D.elapsed_text(self.sample()), '00:05:36')

    def test_long_duration_does_not_wrap_at_24_hours(self):
        self.assertEqual(D.elapsed_text(self.sample(active_elapsed_s=90061, elapsed_running=False)),
                         '25:01:01')

    def test_bounded_test_progress_is_still_available(self):
        self.assertEqual(D.jump_progress(self.sample(max_jumps=500)), '本轮进度: 168/500')
        self.assertEqual(D.jump_progress(dict(jumps=168)), '本轮跳数: 168')

    def test_panel_default_cadence_and_no_redundant_csv_or_pid_scan(self):
        now = [100.0]
        sleeps = []
        def sleep(seconds):
            sleeps.append(seconds)
            now[0] += seconds
            if len(sleeps) == 6:
                raise KeyboardInterrupt
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'status.json'
            path.write_text(json.dumps(self.sample(updated_at=time.time())), encoding='utf8')
            output = io.StringIO()
            with patch.object(sys, 'argv', ['panel', '--status', str(path), '--no-color']), \
                    patch.object(D.time, 'monotonic', side_effect=lambda: now[0]), \
                    patch.object(D.time, 'sleep', side_effect=sleep), \
                    patch.object(D, 'newest_csv', side_effect=AssertionError('CSV scan')), \
                    patch.object(D, 'bot_process', side_effect=AssertionError('PID scan')), \
                    patch.object(D, 'lower_priority'), patch.object(D, 'log_exit'), \
                    redirect_stdout(output):
                self.assertEqual(D.main(), 0)
            self.assertEqual(sleeps, [.25] * 6)
            self.assertIn('00:05:37', output.getvalue())

    def test_status_clock_uses_active_time_and_freezes_a_long_pause(self):
        now = [100.0]
        control = RunControl(clock=lambda: now[0])
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(J, '_CONTROL', control), patch.object(J, '_STATUS', {}), \
                patch.object(J, '_ROUND_STARTED_ACTIVE', 100.0), \
                patch.object(J, 'STATUS_PATH', Path(directory) / 'status.json'), \
                patch.object(J.time, 'monotonic', side_effect=lambda: now[0]):
            now[0] += 2
            J.status_write(state='playing', alive=True, jumps=168)
            self.assertEqual(J._STATUS['active_elapsed_s'], 2)
            now[0] += 1
            self.assertEqual(D.elapsed_text(J._STATUS), '00:00:03')
            control.toggle()
            control.acknowledge_pause()
            J.status_write()
            now[0] += 180
            J.status_write()
            self.assertEqual(J._STATUS['active_elapsed_s'], 3)
            self.assertFalse(J._STATUS['elapsed_running'])
            self.assertEqual(D.elapsed_text(J._STATUS), '00:00:03')
            control.toggle()
            now[0] += 3
            J.status_write()
            self.assertEqual(D.elapsed_text(J._STATUS), '00:00:03')
            control.finish_resume(control.generation)
            J.status_write(state='playing')
            now[0] += 1
            self.assertEqual(D.elapsed_text(J._STATUS), '00:00:04')
            self.assertEqual(J._STATUS['jumps'], 168)
            J.status_write(state='stopped', alive=False)
            now[0] += 10
            self.assertEqual(D.elapsed_text(J._STATUS), '00:00:04')

    def test_startup_wait_does_not_start_the_round_clock(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(J, '_CONTROL', None), \
                patch.object(J, '_STATUS', {}), patch.object(J, '_ROUND_STARTED_ACTIVE', None), \
                patch.object(J, 'STATUS_PATH', Path(directory) / 'status.json'):
            J.status_write(state='waiting_window', alive=True)
            self.assertEqual(J._STATUS['active_elapsed_s'], 0)
            self.assertFalse(J._STATUS['elapsed_running'])

    def test_unlimited_budget_continues_beyond_5000(self):
        for completed in (0, 4999, 5000, 5001, 1_000_000):
            self.assertTrue(J.jump_budget_available(completed, 0))
        self.assertTrue(J.jump_budget_available(499, 500))
        self.assertFalse(J.jump_budget_available(500, 500))

    def test_defaults_and_cli_override_old_config_without_writing_it(self):
        self.assertEqual(J.Config().max_jumps, 0)
        self.assertEqual(J.Config.load(ROOT / 'config.json').max_jumps, 0)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            original = json.dumps(dict(max_jumps=5000, k=2.61, gamma=0))
            path.write_text(original, encoding='utf8')
            args = J.build_parser().parse_args(['--config', str(path), '--max-jumps', '0'])
            self.assertEqual(J.Config.load(path, dict(max_jumps=args.max_jumps)).max_jumps, 0)
            self.assertEqual(path.read_text(encoding='utf8'), original)
            for invalid in (-1, 1.5, True, '500'):
                with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                    J.Config.load(path, dict(max_jumps=invalid))

    def test_daily_launchers_have_no_positive_jump_limit(self):
        for name in ('tools/build_app.ps1', 'tools/installer/jumpbot.iss'):
            with self.subTest(name=name):
                text = (ROOT / name).read_text(encoding='utf-8-sig')
                self.assertIn('--max-jumps 0 --daemon', text)
                self.assertNotIn('--max-jumps 5000', text)


if __name__ == '__main__':
    unittest.main()
