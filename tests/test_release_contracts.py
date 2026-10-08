"""No-input checks for the cleaned-up HUD and dashboard contracts."""
from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ['JUMPBOT_NO_INPUT'] = '1'
import jumpbot as J  # noqa: E402
from tools import dashboard as D  # noqa: E402


class ReleaseContractTests(unittest.TestCase):
    def test_hud_hit_truth_does_not_use_geometry(self):
        center = SimpleNamespace(result='landed', note='[center x2]', error_px=45)
        miss = SimpleNamespace(result='landed', note='[miss]', error_px=0)
        unknown = SimpleNamespace(result='landed', note='[score基准]', error_px=0)
        self.assertEqual(J.hud_center_hit(center), 1)
        self.assertEqual(J.hud_center_hit(miss), 0)
        self.assertIsNone(J.hud_center_hit(unknown))

    def test_unknown_and_death_are_separate_counts(self):
        rows = [SimpleNamespace(result='landed', note='[center x1]'),
                SimpleNamespace(result='unknown', note='暂停画面不可核对'),
                SimpleNamespace(result='dead', note='')]
        s = J.accuracy_fields(rows)
        self.assertEqual(s['unknown'], 1)
        self.assertEqual(s['attempts'], 3)
        self.assertEqual(s['landed'], 1)

    def test_dashboard_shows_stop_and_exits_without_process_scan(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'status.json'
            path.write_text(json.dumps(dict(state='stopped', alive=False, pid=123,
                                            updated_at=time.time(), jumps=0)), encoding='utf8')
            output = io.StringIO()
            with patch.object(sys, 'argv', ['panel', '--status', str(path), '--no-color']), \
                    patch.object(D, 'newest_csv', return_value=None), \
                    patch.object(D, 'lower_priority'), patch.object(D, 'log_exit'), \
                    patch.object(D.time, 'sleep'), \
                    patch.object(D, 'bot_process', side_effect=AssertionError('PID scan')), \
                    redirect_stdout(output):
                self.assertEqual(D.main(), 0)
            self.assertIn('脚本已停止', output.getvalue())


if __name__ == '__main__':
    unittest.main()
