"""Offline HUD labels must not count manual gameplay across pauses."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
from _scorelog import iter_labels  # noqa: E402
from report import analyze, wilson  # noqa: E402
from summarize_acceptance import unrecorded_restarts  # noqa: E402


class ReportTests(unittest.TestCase):
    def test_recorded_hud_labels_survive_pause_manual_score_increase(self):
        rows = [dict(result='landed', score='2', note='[score基准]', mask_backend='simd'),
                dict(result='landed', score='3', note='[miss]', mask_backend='simd'),
                dict(result='landed', score='35', note='[score基准]', mask_backend='simd'),
                dict(result='landed', score='37', note='[center x1]', mask_backend='simd'),
                dict(result='unknown', score='', note='暂停无法核对', mask_backend='simd'),
                dict(result='dead', score='', note='', mask_backend='simd')]
        s = analyze(rows)
        self.assertEqual(s['score_hits'], 1)
        self.assertEqual(s['score_misses'], 1)
        self.assertEqual(s['score_unknown'], 3)
        self.assertEqual(s['attempts'], 6)
        self.assertEqual(s['unresolved'], 1)
        self.assertEqual(s['land_rate'], 66.67)
        self.assertEqual(s['judgement_coverage'], 33.33)

    def test_legacy_death_or_unknown_cannot_bridge_score_baseline(self):
        rows = [dict(result='landed', score='1'), dict(result='dead', score=''),
                dict(result='landed', score='3'), dict(result='landed', score='5')]
        self.assertEqual([x[1] for x in iter_labels(rows)], [None, None, None, 1])

    def test_wilson_empty_and_full(self):
        self.assertIsNone(wilson(0, 0))
        self.assertEqual(wilson(500, 500), [99.24, 100])

    def test_silent_restart_cannot_pass_safety_audit(self):
        rows = [dict(index='160', result='landed', score='27225', note='[读数异常(+0)]'),
                dict(index='161', result='landed', score='2', note='[score重置]')]
        self.assertEqual(unrecorded_restarts(rows, [161]), [161])
        rows[0]['result'] = 'dead'
        self.assertEqual(unrecorded_restarts(rows, [161]), [])

    def test_manual_reset_after_pause_baseline_is_not_inferred_death(self):
        rows = [dict(index='50', result='landed', score='100', note='[center x1]'),
                dict(index='51', result='landed', score='2', note='[score基准]')]
        self.assertEqual(unrecorded_restarts(rows, [51]), [])


if __name__ == '__main__':
    unittest.main()
