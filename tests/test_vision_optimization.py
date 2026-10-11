"""Exact numeric equivalence and non-mutation checks; no game input."""
from pathlib import Path
import runpy
import sys
import unittest
from unittest.mock import patch
import warnings

import numpy as np
import skin_vision as S

ROOT = Path(__file__).resolve().parents[1]


def legacy_waist(rows):
    height = len(rows)
    neck = float(np.median(rows[max(1, height // 8):max(2, height // 3)]))
    base = float(np.median(rows[height // 2:max(height // 2 + 1, height * 7 // 8)]))
    if neck >= 3 and base >= neck * 1.15:
        return True
    band = max(3, height // 10)
    stop = min(height // 2, height - band)
    widths = [float(np.median(rows[y:y + band])) for y in range(height // 4, stop + 1)]
    collar = float(np.median(rows[max(1, height // 12):max(2, height // 6)]))
    lower = [float(np.median(rows[y:y + band]))
             for y in range(height // 2, max(height // 2, height * 7 // 8 - band) + 1)]
    waist, sustained = min(widths, default=0), max(lower, default=0)
    return ((neck >= 3 and sustained >= neck * 1.15)
            or (waist >= 3 and waist < collar and sustained >= waist * 1.15))


class OptimizationTests(unittest.TestCase):
    def test_vectorized_waist_is_exact_and_does_not_change_rows(self):
        rng = np.random.default_rng(20261011)
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', RuntimeWarning)
            for height in list(range(1, 24)) + [31, 60, 79, 100, 107, 127, 256]:
                for _ in range(12):
                    rows = rng.integers(0, 65, size=height, dtype=np.int64)
                    before = rows.copy()
                    self.assertEqual(S._has_waist(rows), legacy_waist(rows), height)
                    np.testing.assert_array_equal(rows, before)

    def test_vectorized_windows_reduce_python_median_dispatches(self):
        rows = np.full(100, 10, dtype=np.int64)
        with patch.object(S.np, 'median', wraps=np.median) as medians:
            self.assertFalse(S._has_waist(rows))
            self.assertEqual(medians.call_count, 4)

    def test_material_delta_reuse_matches_old_masks_exactly(self):
        rng = np.random.default_rng(14)
        for dtype in (np.float32, np.float64):
            pixels = rng.integers(0, 256, (37, 42, 3), dtype=np.uint8)
            colour = np.array([125.5, 135.0, 132.5], dtype=dtype)
            values = pixels.astype(np.float32)
            for tolerance in (12, 16, 20):
                expected = (((np.linalg.norm(values - colour, axis=2) <= 35)
                             & (np.linalg.norm(values[:, :, 1:] - colour[1:], axis=2)
                                <= tolerance)).astype(np.uint8) * 255)
                np.testing.assert_array_equal(S._material_plane(pixels, colour, tolerance), expected)

    def test_frozen_panel_runtime_paths_use_executable_directory(self):
        fake_exe = str(ROOT / 'build/fake-delivery/jumpbot.exe')
        with patch.object(sys, 'frozen', True, create=True), patch.object(sys, 'executable', fake_exe):
            panel = runpy.run_path(str(ROOT / 'tools/dashboard.py'), run_name='panel_path_test')
        self.assertEqual(panel['PROJECT_DIR'], Path(fake_exe).parent)
        self.assertEqual(panel['DEFAULT_STATUS'], Path(fake_exe).parent / 'debug/status.json')


if __name__ == '__main__':
    unittest.main()
