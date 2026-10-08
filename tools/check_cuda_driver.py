"""Non-game CUDA driver/kernel compatibility and buffer-reuse stress test."""
from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
os.environ['JUMPBOT_NO_INPUT'] = '1'
import jumpbot as J  # noqa: E402
import numpy as np  # noqa: E402


def main():
    sys.stdout.reconfigure(encoding='utf8', errors='replace')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, default=ROOT / 'build/cuda-driver-check.json')
    parser.add_argument('--rounds', type=int, nargs='+', default=[500, 1000, 2000])
    args = parser.parse_args()
    if any(n < 1 for n in args.rounds):
        parser.error('rounds must be positive')
    cfg = J.Config.load(ROOT / 'config.json')
    J.tune_cpu(J.effective_cv2_threads(cfg.cv2_threads), 'below')
    vision = J.Vision(dataclasses.replace(cfg, masks_backend='gpu', cuda_enabled=True))
    cpu = J.Vision(dataclasses.replace(cfg, masks_backend='simd'))
    frame = np.random.default_rng(61742).integers(0, 256, (1050, 553, 3), dtype=np.uint8)
    if not vision._init_gpu():
        raise RuntimeError('CUDA unavailable: a CPU fallback is not a passing CUDA test')
    cp = vision._cp
    runtime = cp.cuda.runtime
    smi = subprocess.run(['nvidia-smi', '--query-gpu=name,driver_version', '--format=csv,noheader'],
                         capture_output=True, text=True, timeout=15, check=True,
                         creationflags=0x08000000 if os.name == 'nt' else 0)
    expected = cpu._masks_simd(frame)
    for _ in range(10):
        vision._masks_gpu(frame)
    cp.cuda.get_current_stream().synchronize()
    before = cp.get_default_memory_pool().used_bytes()
    results = []
    for count in args.rounds:
        started = time.perf_counter()
        for index in range(count):
            result = vision._masks_gpu(frame)
            if result is None or any(not np.array_equal(a, b) for a, b in zip(expected, result)):
                raise AssertionError(f'CUDA output mismatch at {count}/{index}')
        cp.cuda.get_current_stream().synchronize()
        elapsed = time.perf_counter() - started
        used = cp.get_default_memory_pool().used_bytes()
        if used != before:
            raise AssertionError(f'Active GPU memory changed: {before} -> {used}')
        results.append(dict(iterations=count, elapsed_s=elapsed,
                            mean_ms=elapsed * 1000 / count, active_gpu_bytes=used,
                            pixel_differences=0))
        print(f'{count} CUDA frames: output diff=0, active memory={used}, mean={elapsed*1000/count:.3f}ms', flush=True)
    report = dict(game_input=False, game_accuracy_measured=False, nvidia_smi=smi.stdout.strip(),
                  driver_api=runtime.driverGetVersion(), cuda_runtime=runtime.runtimeGetVersion(),
                  cupy=cp.__version__, rounds=results, memory_before=before,
                  source_sha256=hashlib.sha256((ROOT / 'jumpbot.py').read_bytes()).hexdigest(),
                  control_sha256=hashlib.sha256((ROOT / 'run_control.py').read_bytes()).hexdigest())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf8')
    print(f'Report: {args.out}')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
