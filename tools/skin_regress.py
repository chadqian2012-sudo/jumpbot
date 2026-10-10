"""No-input A/B history regression for a skin-recognition change.

Read-only datasets; reports go only to --out. A new positive is reported for
manual inspection, never silently counted as a recognition improvement.
"""
from __future__ import annotations

import argparse
import dataclasses
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import time

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ['JUMPBOT_NO_INPUT'] = '1'
import jumpbot as J  # noqa: E402
from tools import regress  # noqa: E402


def describe(result):
    piece, target, _pm, _bm = result
    return {'piece': None if piece is None else [piece.x, piece.y, list(piece.bbox), piece.area],
            'target': None if target is None else [target.x, target.y, list(target.bbox),
                                                   target.top_width, target.dot]}


def load_baseline(directory):
    # Each side must use its own skin implementation. Merely loading a second
    # jumpbot.py leaves its import bound to the already-loaded candidate module
    # and can hide regressions in precisely the algorithm being reviewed.
    skin_spec = importlib.util.spec_from_file_location('skin_baseline_impl', directory / 'skin_vision.py')
    old_skin = importlib.util.module_from_spec(skin_spec)
    sys.modules[skin_spec.name] = old_skin
    skin_spec.loader.exec_module(old_skin)
    current_skin = sys.modules.get('skin_vision')
    try:
        sys.modules['skin_vision'] = old_skin
        spec = importlib.util.spec_from_file_location('skin_baseline', directory / 'jumpbot.py')
        old = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = old
        spec.loader.exec_module(old)
    finally:
        if current_skin is None:
            sys.modules.pop('skin_vision', None)
        else:
            sys.modules['skin_vision'] = current_skin
    return old


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--dataset', type=Path, action='append', required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    old = load_baseline(args.baseline)
    J.log = old.log = lambda _: None
    cfg = dataclasses.replace(J.Config.load(ROOT / 'config.json'), masks_backend='simd',
                              cuda_enabled=False, opencl_enabled=False)
    cv2.setNumThreads(J.effective_cv2_threads(cfg.cv2_threads))
    old_cfg = old.Config.load(args.baseline / 'config.json')
    old_cfg.masks_backend, old_cfg.cuda_enabled, old_cfg.opencl_enabled = 'simd', False, False
    paths = sorted({p for dataset in args.dataset for p in dataset.rglob('*.png')
                    if regress.is_game_frame(p)})
    changed, lost, added, mask_changes = [], [], [], []
    elapsed_old, elapsed_new = [], []
    totals = {'frames': 0, 'baseline_positive': 0, 'candidate_positive': 0}
    for index, path in enumerate(paths):
        frame = cv2.imread(str(path))
        if frame is None:
            continue
        a, b = old.Vision(old_cfg), J.Vision(cfg)
        t = time.perf_counter()
        before = a.detect(frame)
        elapsed_old.append((time.perf_counter() - t) * 1000)
        t = time.perf_counter()
        after = b.detect(frame)
        elapsed_new.append((time.perf_counter() - t) * 1000)
        totals['frames'] += 1
        totals['baseline_positive'] += before[1] is not None
        totals['candidate_positive'] += after[1] is not None
        ad, bd = describe(before), describe(after)
        entry = {'path': str(path), 'before': ad, 'after': bd}
        if ad != bd:
            changed.append(entry)
        if before[1] is not None and after[1] is None:
            lost.append(entry)
        if before[1] is None and after[1] is not None:
            added.append(entry)
        if any(not np.array_equal(x, y) for x, y in zip(a.masks(frame), b.masks(frame))):
            mask_changes.append(str(path))
        if index % 100 == 0:
            print(f'processed {index}/{len(paths)}', flush=True)
    regress.PROJECT_DIR = args.dataset[0].parent
    scores = regress.check_score_reader(cfg)
    screens = regress.check_screen_classify(cfg)
    summary = dict(totals, changed=changed, lost=lost, added=added,
                   raw_mask_changes=mask_changes, score_reader=list(scores),
                   screen_classifier=list(screens),
                   median_ms={'baseline': float(np.median(elapsed_old)),
                              'candidate': float(np.median(elapsed_new))},
                   source_sha256={name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                                  for name in ('jumpbot.py', 'skin_vision.py', 'config.json')})
    args.out.parent.mkdir(parents=True, exist_ok=True)
    def numpy_scalar(value):
        if isinstance(value, np.generic):
            return value.item()
        raise TypeError(f'not JSON serializable: {type(value).__name__}')

    args.out.write_text(json.dumps(summary, indent=2, ensure_ascii=False,
                                   default=numpy_scalar), encoding='utf8')
    print(json.dumps({key: value for key, value in summary.items()
                      if key not in ('changed', 'lost', 'added', 'raw_mask_changes')},
                     ensure_ascii=False))
    print(f'changed={len(changed)}, lost={len(lost)}, added={len(added)}, raw_mask_changes={len(mask_changes)}')
    return int(bool(lost or mask_changes or scores[0] != scores[1] or screens[0] != screens[1]))


if __name__ == '__main__':
    raise SystemExit(main())
