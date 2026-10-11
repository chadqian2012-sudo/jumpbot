"""No-input skin recognition: real gold fixture and labelled synthetic colours."""
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
from skin_vision import (find_skin_piece, _has_waist, _material_bodies,
                         _nested_silhouette, SkinMatch)  # noqa: E402


class SkinTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-play.png'))
        assert cls.frame is not None
        cls.cfg = dataclasses.replace(J.Config(), masks_backend='simd',
                                      cuda_enabled=False, opencl_enabled=False)
        hsv = cv2.cvtColor(cls.frame, cv2.COLOR_BGR2HSV)
        cls.gold = cv2.inRange(hsv, (12, 55, 65), (40, 255, 255)) > 0
        bounds = np.zeros(cls.gold.shape, dtype=bool)
        bounds[532:652, 148:199] = True
        cls.gold &= bounds

    def detect(self, frame, cfg=None):
        return J.Vision(cfg or self.cfg).detect(frame)

    def test_real_gold_has_character_target_and_platform(self):
        p, t, pm, bm = self.detect(self.frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertEqual(p.source, 'skin-shape')
        self.assertAlmostEqual(p.x, 173, delta=2)
        self.assertAlmostEqual(p.y, 647, delta=2)
        self.assertAlmostEqual(t.x, 404, delta=3)
        self.assertAlmostEqual(t.y, 501, delta=3)
        self.assertTrue(J._foot_on_block(bm, p, self.cfg))
        self.assertTrue(pm[550:640, 160:185].any())

    def test_synthetic_hues_do_not_move_the_foot_or_target(self):
        for hue in (0, 20, 55, 85, 120, 150, 175):
            with self.subTest(hue=hue):
                hsv = cv2.cvtColor(self.frame, cv2.COLOR_BGR2HSV)
                hsv[:, :, 0][self.gold] = hue
                hsv[:, :, 1][self.gold] = 190
                frame = cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR)
                p, t, _pm, _bm = self.detect(frame)
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 173, delta=2)
                self.assertAlmostEqual(p.y, 647, delta=2)
                self.assertAlmostEqual(t.x, 404, delta=3)
                self.assertAlmostEqual(t.y, 501, delta=3)

    def test_black_and_grey_skin_variants(self):
        original = cv2.cvtColor(self.frame, cv2.COLOR_BGR2HSV)
        for value in (70, 140):
            with self.subTest(value=value):
                hsv = original.copy()
                hsv[:, :, 1][self.gold] = 0
                hsv[:, :, 2][self.gold] = np.clip(
                    original[:, :, 2][self.gold].astype(float) * value / 225, 0, 255).astype(np.uint8)
                p, t, _pm, _bm = self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 173, delta=2)
                self.assertAlmostEqual(p.y, 647, delta=2)

    def test_weak_purple_decoration_cannot_disable_auto_skin(self):
        frame = self.frame.copy()
        cv2.rectangle(frame, (300, 580), (332, 660), (95, 40, 65), -1)
        p, t, pm, _bm = self.detect(frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertEqual(p.source, 'skin-shape')
        self.assertAlmostEqual(p.x, 173, delta=2)
        self.assertAlmostEqual(p.y, 647, delta=2)
        self.assertTrue(pm[540:560, 165:180].any(), 'head must not be left as a target')

    def test_gold_glitter_on_red_platform_recovers_a_complete_body(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-red-platform.png'))
        self.assertIsNotNone(frame)
        p, t, pm, bm = self.detect(frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertEqual(p.source, 'skin-shape')
        self.assertAlmostEqual(p.x, 350, delta=2)
        self.assertAlmostEqual(p.y, 613, delta=2)
        self.assertAlmostEqual(t.x, 228, delta=3)
        self.assertAlmostEqual(t.y, 529, delta=3)
        self.assertTrue(J._foot_on_block(bm, p, self.cfg))
        self.assertTrue(pm[508:530, 340:360].any())

    def test_glitter_adaptation_retains_backend_and_scale_agreement(self):
        original = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-red-platform.png'))
        for scale in (.75, 1, 1.25):
            with self.subTest(scale=scale):
                frame = cv2.resize(original, None, fx=scale, fy=scale)
                ps, ts, *_ = self.detect(frame)
                pc, tc, *_ = self.detect(frame, dataclasses.replace(self.cfg, masks_backend='cpu'))
                self.assertIsNotNone(ps)
                self.assertIsNotNone(pc)
                self.assertIsNotNone(ts)
                self.assertIsNotNone(tc)
                self.assertEqual((ps.x, ps.y, ts.x, ts.y), (pc.x, pc.y, tc.x, tc.y))
                self.assertAlmostEqual(ps.x / scale, 350, delta=3)
                self.assertAlmostEqual(ps.y / scale, 613, delta=3)

    def test_edge_proposal_cannot_hide_gold_material_on_green_platform(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-green-platform.png'))
        p, t, _pm, bm = self.detect(frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertEqual(p.source, 'skin-shape')
        self.assertAlmostEqual(p.x, 407, delta=2)
        self.assertAlmostEqual(p.y, 657, delta=2)
        self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_lost_gold_silhouette_cannot_turn_a_cup_into_a_character(self):
        for name in ('skin-gold-cardboard', 'skin-gold-falling'):
            with self.subTest(name=name):
                frame = cv2.imread(str(ROOT / 'tests/fixtures' / (name + '.png')))
                p, _t, _pm, _bm = self.detect(frame)
                if p is not None:
                    self.assertEqual(p.source, 'skin-shape')
                    self.assertAlmostEqual(p.x, 391, delta=3)
                if name == 'skin-gold-falling':
                    self.assertIsNone(p)

    def test_head_sampled_glitter_distinguishes_gold_from_cardboard(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-cardboard.png'))
        p, t, _pm, bm = self.detect(frame)
        self.assertIsNotNone(p)
        self.assertIsNotNone(t)
        self.assertEqual(p.source, 'skin-shape')
        self.assertAlmostEqual(p.x, 391, delta=3)
        self.assertAlmostEqual(p.y, 643, delta=3)
        self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_broken_gold_head_arc_is_recovered_without_relaxing_geometry(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-white-book.png'))
        for backend in ('cpu', 'simd'):
            with self.subTest(backend=backend):
                p, t, _pm, bm = self.detect(frame, dataclasses.replace(self.cfg, masks_backend=backend))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertEqual(p.source, 'skin-shape')
                self.assertAlmostEqual(p.x, 150, delta=3)
                self.assertAlmostEqual(p.y, 659, delta=3)
                self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_broken_head_reconstruction_is_not_a_gold_hue_rule(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-white-book.png'))
        original = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        material = cv2.inRange(original, (12, 55, 65), (40, 255, 255)) > 0
        bounds = np.zeros(material.shape, dtype=bool)
        bounds[546:663, 125:178] = True
        material &= bounds
        for hue in (0, 20, 55, 85, 120, 150, 175):
            with self.subTest(hue=hue):
                hsv = original.copy()
                hsv[:, :, 0][material] = hue
                hsv[:, :, 1][material] = 190
                p, t, _pm, _bm = self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 150, delta=3)
                self.assertAlmostEqual(p.y, 659, delta=3)

    def test_gold_start_texture_fragment_is_not_a_second_person(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-start-texture.png'))
        self.assertIsNotNone(frame)
        self.assertIsNone(J.match_ui(frame), 'a live frame is not a restart button')
        for backend in ('cpu', 'simd'):
            with self.subTest(backend=backend):
                p, t, _pm, bm = self.detect(frame, dataclasses.replace(self.cfg, masks_backend=backend))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertEqual(p.source, 'skin-shape')
                self.assertAlmostEqual(p.x, 173, delta=2)
                self.assertAlmostEqual(p.y, 645, delta=2)
                self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_gold_round_platform_boundary_holes_get_bounded_repair(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-round-platform.png'))
        self.assertIsNotNone(frame)
        for backend in ('cpu', 'simd'):
            with self.subTest(backend=backend):
                p, t, _pm, bm = self.detect(frame, dataclasses.replace(self.cfg, masks_backend=backend))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertEqual(p.source, 'skin-shape')
                self.assertAlmostEqual(p.x, 232, delta=2)
                self.assertAlmostEqual(p.y, 611, delta=2)
                self.assertAlmostEqual(t.x, 346, delta=3)
                self.assertAlmostEqual(t.y, 533, delta=3)
                self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_round_platform_repair_keeps_hue_and_scale_agreement(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-round-platform.png'))
        original = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        material = cv2.inRange(original, (12, 55, 65), (40, 255, 255)) > 0
        bounds = np.zeros(material.shape, dtype=bool)
        bounds[497:616, 208:258] = True
        material &= bounds
        for hue in (0, 20, 55, 85, 120, 150, 175):
            with self.subTest(hue=hue):
                hsv = original.copy()
                hsv[:, :, 0][material] = hue
                hsv[:, :, 1][material] = 190
                p, t, _pm, _bm = self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 232, delta=2)
                self.assertAlmostEqual(p.y, 611, delta=2)
        for scale in (.75, 1.25):
            with self.subTest(scale=scale):
                p, t, _pm, _bm = self.detect(cv2.resize(frame, None, fx=scale, fy=scale))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x / scale, 232, delta=3)
                self.assertAlmostEqual(p.y / scale, 611, delta=3)

    def test_glitter_neighborhood_separates_gold_from_flat_yellow_platform(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-yellow-platform.png'))
        self.assertIsNotNone(frame)
        for backend in ('cpu', 'simd'):
            with self.subTest(backend=backend):
                p, t, _pm, bm = self.detect(frame, dataclasses.replace(self.cfg, masks_backend=backend))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertEqual(p.source, 'skin-shape')
                self.assertAlmostEqual(p.x, 356, delta=2)
                self.assertAlmostEqual(p.y, 618, delta=2)
                self.assertAlmostEqual(t.x, 224, delta=3)
                self.assertAlmostEqual(t.y, 529, delta=3)
                self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_yellow_platform_neighborhood_stays_hue_independent(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-yellow-platform.png'))
        original = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        material = cv2.inRange(original, (12, 55, 65), (40, 255, 255)) > 0
        # A rectangular HSV selection also paints the yellow platform under
        # the foot. Use the separately ground-truth-checked full silhouette so
        # this tests a skin recolor, not a changed/cropped platform fragment.
        vision = J.Vision(self.cfg)
        self.assertIsNotNone(vision.detect(frame)[0])
        skin = vision._skin_match
        self.assertIsNotNone(skin)
        x, y, w, h = skin.bbox
        bounds = np.zeros(material.shape, dtype=bool)
        bounds[y:y + h, x:x + w] = skin.mask > 0
        material &= bounds
        for hue in (0, 20, 55, 85, 120, 150, 175):
            with self.subTest(hue=hue):
                hsv = original.copy()
                hsv[:, :, 0][material] = hue
                hsv[:, :, 1][material] = 190
                p, t, _pm, _bm = self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 356, delta=3)
                self.assertAlmostEqual(p.y, 618, delta=3)

    def test_textured_flat_material_cannot_be_cropped_into_a_body(self):
        lab = np.full((200, 120, 3), (197, 127, 168), np.uint8)
        noise = np.random.default_rng(154).integers(-8, 9, size=lab.shape[:2])
        lab[:, :, 0] = (lab[:, :, 0].astype(np.int16) + noise).astype(np.uint8)
        head = cv2.ellipse2Poly((60, 40), (15, 15), 0, 0, 360, 5).reshape(-1, 1, 2)
        self.assertEqual(_material_bodies(lab, head, 45, 25, 31, 31), [])
        # A textured cylinder still has no sustained waist.
        frame = np.full((600, 400, 3), 210, np.uint8)
        cv2.circle(frame, (200, 350), 15, (60, 90, 180), -1)
        cv2.rectangle(frame, (185, 370), (215, 430), (60, 90, 180), -1)
        selected = frame[:, :, 0] == 60
        values = frame.astype(np.int16)
        values[selected] += np.random.default_rng(86).integers(-8, 9, size=(selected.sum(), 1))
        self.assertIsNone(find_skin_piece(np.clip(values, 0, 255).astype(np.uint8)))

    def test_shaded_gold_fine_texture_on_tan_background_is_complete(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-tan-background.png'))
        self.assertIsNotNone(frame)
        for backend in ('cpu', 'simd'):
            with self.subTest(backend=backend):
                p, t, _pm, bm = self.detect(frame, dataclasses.replace(self.cfg, masks_backend=backend))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertEqual(p.source, 'skin-shape')
                self.assertAlmostEqual(p.x, 175, delta=2)
                self.assertAlmostEqual(p.y, 646, delta=2)
                self.assertAlmostEqual(t.x, 404, delta=3)
                self.assertAlmostEqual(t.y, 501, delta=3)
                self.assertTrue(J._foot_on_block(bm, p, self.cfg))

    def test_fine_texture_adaptation_on_tan_is_not_a_gold_hue_rule(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-tan-background.png'))
        original = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        vision = J.Vision(self.cfg)
        self.assertIsNotNone(vision.detect(frame)[0])
        skin = vision._skin_match
        self.assertIsNotNone(skin)
        x, y, w, h = skin.bbox
        selected = np.zeros(frame.shape[:2], dtype=bool)
        selected[y:y + h, x:x + w] = skin.mask > 0
        selected &= cv2.inRange(original, (12, 55, 65), (40, 255, 255)) > 0
        for hue in (0, 20, 55, 85, 120, 150, 175):
            with self.subTest(hue=hue):
                hsv = original.copy()
                hsv[:, :, 0][selected] = hue
                hsv[:, :, 1][selected] = 190
                p, t, _pm, _bm = self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 175, delta=3)
                self.assertAlmostEqual(p.y, 646, delta=3)

    def test_nested_outline_coalescing_requires_same_head_and_coverage(self):
        full = SkinMatch(20, 95, (0, 0, 40, 95), .90, np.full((95, 40), 255, np.uint8))
        short = SkinMatch(20, 65, (5, 0, 30, 65), .99, np.full((65, 30), 255, np.uint8))
        head = (5, 0, 30, 30)
        self.assertTrue(_nested_silhouette(full, head, short, head))
        self.assertTrue(_nested_silhouette(short, head, full, head))
        self.assertFalse(_nested_silhouette(full, head, short, (15, 0, 30, 30)))
        self.assertFalse(_nested_silhouette(full, head,
            dataclasses.replace(short, bbox=(25, 0, 30, 65)), head))
        hollow = dataclasses.replace(full, mask=full.mask.copy())
        hollow.mask[35:65, 5:35] = 0
        self.assertFalse(_nested_silhouette(hollow, head, short, head))

    def test_start_texture_coalescing_is_hue_independent_and_keeps_ambiguity(self):
        frame = cv2.imread(str(ROOT / 'tests/fixtures/skin-gold-start-texture.png'))
        original = cv2.cvtColor(frame, cv2.COLOR_BGR2HSV)
        material = cv2.inRange(original, (12, 55, 65), (40, 255, 255)) > 0
        bounds = np.zeros(material.shape, dtype=bool)
        bounds[530:650, 145:202] = True
        material &= bounds
        for hue in (0, 20, 55, 85, 120, 150, 175):
            with self.subTest(hue=hue):
                hsv = original.copy()
                hsv[:, :, 0][material] = hue
                hsv[:, :, 1][material] = 190
                p, t, _pm, _bm = self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 173, delta=2)
                self.assertAlmostEqual(p.y, 645, delta=2)
        duplicate = frame.copy()
        duplicate[530:650, 295:352] = frame[530:650, 145:202]
        self.assertIsNone(find_skin_piece(duplicate))

    def test_one_vision_adapts_to_colour_changes_without_a_stale_profile(self):
        vision = J.Vision(self.cfg)
        original = cv2.cvtColor(self.frame, cv2.COLOR_BGR2HSV)
        for hue in (20, 0, 179, 55, 85, 120, 150, 20):
            with self.subTest(hue=hue):
                hsv = original.copy()
                hsv[:, :, 0][self.gold] = hue
                hsv[:, :, 1][self.gold] = 190
                p, t, _pm, _bm = vision.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x, 173, delta=2)
                self.assertAlmostEqual(p.y, 647, delta=2)
                self.assertAlmostEqual(t.x, 404, delta=3)
                self.assertAlmostEqual(t.y, 501, delta=3)
        vision.reset_prior()
        self.assertIsNone(vision.piece)
        self.assertIsNone(vision.detect(np.full_like(self.frame, 210))[0])

    def test_old_configuration_enables_auto_skin_by_default(self):
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'config.json'
            path.write_text(json.dumps({'k': 2.61, 'gamma': 0}), encoding='utf8')
            cfg = J.Config.load(path)
            self.assertTrue(cfg.piece_skin_shape_enabled)
            self.assertEqual(cfg.k, 2.61)
            self.assertEqual(cfg.gamma, 0)

    def test_low_contrast_white_on_white_surface_fails_closed(self):
        # Both shaded and flat white lose the complete body outline at the
        # platform boundary. They are intentionally unsupported, not positives
        # made to pass by reducing the geometry/input-safety gates.
        original = cv2.cvtColor(self.frame, cv2.COLOR_BGR2HSV)
        for shaded in (False, True):
            with self.subTest(shaded=shaded):
                hsv = original.copy()
                hsv[:, :, 1][self.gold] = 0
                hsv[:, :, 2][self.gold] = (np.clip(
                    original[:, :, 2][self.gold].astype(float) * 240 / 225,
                    0, 255).astype(np.uint8) if shaded else 240)
                self.assertIsNone(self.detect(cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR))[0])

    def test_foot_is_scale_equivariant(self):
        for scale in (.75, 1.25):
            with self.subTest(scale=scale):
                frame = cv2.resize(self.frame, None, fx=scale, fy=scale)
                p, t, _pm, _bm = self.detect(frame)
                self.assertIsNotNone(p)
                self.assertIsNotNone(t)
                self.assertAlmostEqual(p.x / scale, 173, delta=3)
                self.assertAlmostEqual(p.y / scale, 647, delta=3)

    def test_no_supported_platform_no_shape_match(self):
        self.assertIsNone(find_skin_piece(self.frame, np.zeros(self.frame.shape[:2], np.uint8)))
        self.assertIsNone(find_skin_piece(self.frame, np.full(self.frame.shape[:2], 255, np.uint8)))

    def test_material_profile_cannot_crop_same_colour_platform_into_a_body(self):
        lab = np.full((200, 120, 3), (197, 127, 168), np.uint8)
        head = cv2.ellipse2Poly((60, 40), (15, 15), 0, 0, 360, 5).reshape(-1, 1, 2)
        self.assertEqual(_material_bodies(lab, head, 45, 25, 31, 31), [])

    def test_a_noisy_cylinder_is_not_a_sustained_waist(self):
        widths = np.full(72, 30)
        widths[30] = 5
        self.assertFalse(_has_waist(widths))
        self.assertFalse(_has_waist(np.full(72, 30)))

    def test_blank_noise_and_round_bottle_do_not_match(self):
        blank = np.full((600, 400, 3), 210, np.uint8)
        self.assertIsNone(find_skin_piece(blank))
        self.assertIsNone(find_skin_piece(np.random.default_rng(123).integers(
            0, 256, size=blank.shape, dtype=np.uint8)))
        bottle = blank.copy()
        cv2.circle(bottle, (200, 350), 15, (60, 90, 180), -1)
        cv2.rectangle(bottle, (185, 370), (215, 430), (60, 90, 180), -1)
        self.assertIsNone(find_skin_piece(bottle))

    def test_two_valid_people_are_ambiguous(self):
        frame = self.frame.copy()
        frame[530:655, 298:349] = self.frame[530:655, 148:199]
        self.assertIsNone(find_skin_piece(frame))

    def test_raw_cpu_simd_masks_unchanged_and_detection_agrees(self):
        simd = J.Vision(self.cfg)
        cpu = J.Vision(dataclasses.replace(self.cfg, masks_backend='cpu'))
        for a, b in zip(simd.masks(self.frame), cpu.masks(self.frame)):
            self.assertTrue(np.array_equal(a, b))
        ps, ts, _a, _b = simd.detect(self.frame)
        pc, tc, _a, _b = cpu.detect(self.frame)
        self.assertEqual((ps.x, ps.y, ts.x, ts.y), (pc.x, pc.y, tc.x, tc.y))


if __name__ == '__main__':
    unittest.main()
