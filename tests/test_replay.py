"""A replay must not evaluate colored annotations as actual game pixels."""
import os
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ['JUMPBOT_NO_INPUT'] = '1'
from tools.replay_test import pick_triples  # noqa: E402


class ReplayFrameTests(unittest.TestCase):
    def test_replay_only_uses_clean_raw_pairs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for suffix in ('before', 'after', 'raw', 'raw_after'):
                (root / f'jump_001_{suffix}.png').touch()
            triples = pick_triples(root)
            self.assertEqual(triples, [(root / 'jump_001_raw.png',
                                        root / 'jump_001_raw.png', root / 'jump_001_raw_after.png')])

    def test_annotated_or_incomplete_pairs_cannot_fill_missing_raw_frame(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for suffix in ('before', 'after', 'raw'):
                (root / f'jump_001_{suffix}.png').touch()
            self.assertEqual(pick_triples(root), [])


if __name__ == '__main__':
    unittest.main()
