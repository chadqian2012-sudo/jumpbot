"""Real stuck frame and dot-gate regressions; no capture or input."""
import dataclasses
import os
from pathlib import Path
import sys
import unittest

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ['JUMPBOT_NO_INPUT'] = '1'
import jumpbot as J  # noqa: E402


class TargetDotTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.frame = cv2.imread(str(ROOT / 'tests/fixtures/purple-cylinder-stall.png'))
        assert cls.frame is not None
        cls.cfg = dataclasses.replace(J.Config.load(ROOT / 'config.json'), masks_backend='simd',
                                      cuda_enabled=False, opencl_enabled=False)

    def test_cup_rim_must_not_replace_adjacent_target(self):
        p, t, _pm, _bm = J.Vision(self.cfg).detect(self.frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertAlmostEqual(p.x, 264, delta=2)
        self.assertAlmostEqual(p.y, 593, delta=2)
        self.assertAlmostEqual(t.x, 313, delta=3)
        self.assertAlmostEqual(t.y, 553, delta=6)
        self.assertIsNone(t.dot)
        _axis, along, cross = J.axis_components(t.x - p.x, t.y - p.y, self.cfg.axis_slope)
        self.assertGreater(along, 60)
        self.assertLess(cross / along, .6)

    def test_original_highlight_is_the_reproduced_invalid_dot(self):
        dot = J.find_dot_near(self.frame, 313, 548, 55, self.cfg)
        self.assertIsNotNone(dot)
        self.assertAlmostEqual(dot[0], 278, delta=2)
        self.assertAlmostEqual(dot[1], 588, delta=2)

    def test_valid_correction_is_preserved(self):
        frame = self.frame.copy()
        cv2.circle(frame, (313, 555), 15, (160, 160, 160), -1)
        cv2.circle(frame, (313, 555), 5, (255, 255, 255), -1)
        _p, t, _pm, _bm = J.Vision(self.cfg).detect(frame)
        self.assertIsNotNone(t)
        self.assertIsNotNone(t.dot)
        self.assertAlmostEqual(t.x, 313, delta=2)
        self.assertAlmostEqual(t.y, 555, delta=2)

    def test_rejected_nearest_dot_does_not_hide_valid_farther_dot(self):
        frame = np.full((200, 200, 3), 120, np.uint8)
        cv2.circle(frame, (100, 105), 4, (255, 255, 255), -1)
        cv2.circle(frame, (100, 75), 4, (255, 255, 255), -1)
        dot = J.find_dot_near(frame, 100, 100, 50, self.cfg, accept=lambda x, y: y < 90)
        self.assertIsNotNone(dot)
        self.assertAlmostEqual(dot[1], 75, delta=1)

    def test_cup_label_below_top_face_is_not_a_target_dot(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/purple-cup-before-death.png'))
        p, t, _pm, _bm = J.Vision(self.cfg).detect(frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertIsNone(t.dot)
        self.assertLess(t.y, 545)  # label highlight at y=571 must be rejected
        self.assertGreater(J.press_distance(p, t, self.cfg)[0], 205)

    def test_horizontal_falling_character_cannot_confirm_a_landing(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/purple-horizontal-fall.png'))
        p, _pm, _bm = J.Vision(self.cfg).detect_piece_only(frame)
        self.assertIsNone(p)


if __name__ == '__main__':
    unittest.main()
