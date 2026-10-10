"""The A/B oracle must not accidentally share the candidate skin module."""
from pathlib import Path
import sys
import tempfile
import unittest

from tools.skin_regress import load_baseline


class BaselineIsolationTests(unittest.TestCase):
    def test_baseline_imports_its_own_skin_module_and_restores_candidate(self):
        candidate = sys.modules['skin_vision']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'skin_vision.py').write_text("MARKER = 'baseline'\n", encoding='utf8')
            (root / 'jumpbot.py').write_text('from skin_vision import MARKER\n', encoding='utf8')
            old = load_baseline(root)
            self.assertEqual(old.MARKER, 'baseline')
        self.assertIs(sys.modules['skin_vision'], candidate)

    def test_failed_baseline_import_still_restores_candidate(self):
        candidate = sys.modules['skin_vision']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'skin_vision.py').write_text("MARKER = 'baseline'\n", encoding='utf8')
            (root / 'jumpbot.py').write_text("raise RuntimeError('fixture failure')\n", encoding='utf8')
            with self.assertRaises(RuntimeError):
                load_baseline(root)
        self.assertIs(sys.modules['skin_vision'], candidate)
