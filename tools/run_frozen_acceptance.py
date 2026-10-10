"""Opt-in finite acceptance of a manifest-verified frozen executable.

Uses live_acceptance inside the executable's existing -c diagnostic entry.
No independent mouse/key sender; F8 cancels, and safety stops never restart.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def save(path, data):
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding='utf8')
    tmp.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute-live', action='store_true')
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--jumps', type=int, default=500)
    parser.add_argument('--pause-every', type=int, default=50)
    parser.add_argument('--backend', choices=['auto', 'simd', 'gpu', 'opencl'], default='auto')
    args = parser.parse_args()
    if not args.execute_live or os.environ.get('JUMPBOT_NO_INPUT') == '1':
        parser.error('requires --execute-live and NO_INPUT must not be 1')
    if min(args.jumps, args.pause_every) < 1:
        parser.error('positive jump counts required')
    args.exe = args.exe.resolve(strict=True)
    manifest = json.loads((args.exe.parent / 'source-version.json').read_text(encoding='utf8'))
    exe_hash = hashlib.sha256(args.exe.read_bytes()).hexdigest()
    if manifest['exe_sha256'] != exe_hash:
        raise ValueError('frozen executable hash mismatch')
    for name, wanted in manifest['source_sha256'].items():
        if hashlib.sha256((ROOT / name).read_bytes()).hexdigest() != wanted:
            raise ValueError(f'source changed since packaging: {name}')
    sys.path.insert(0, str(ROOT))
    import jumpbot as J
    # Read-only check against another input owner. Panel and CUDA dependency
    # helper processes are not themselves input owners.
    check = subprocess.run(['powershell', '-NoProfile', '-Command',
                            "Get-CimInstance Win32_Process | Where-Object { "
                            "($_.Name -eq 'jumpbot.exe' -and $_.CommandLine -notmatch ' --panel| -m ') "
                            "-or ($_.Name -eq 'python.exe' -and $_.CommandLine -match "
                            "'jumpbot\\.py|live_acceptance\\.py' -and $_.CommandLine -notmatch "
                            "'--selftest|--dry-run|--image|--probe') } | "
                            "Select-Object -ExpandProperty ProcessId"],
                           capture_output=True, text=True, check=True)
    if check.stdout.strip():
        raise RuntimeError('another jumpbot input owner exists: ' + check.stdout.strip())
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    state_path = args.out / 'test-status.json'
    stop_path = args.out / 'stop.request'
    status_path = args.exe.parent / 'debug/status.json'
    state = dict(state='starting', requested_jumps=args.jumps, backend=args.backend,
                 frozen_exe=str(args.exe), frozen_exe_sha256=exe_hash,
                 source_commit=manifest.get('commit'), stop_reason='')
    code = f'''
import runpy, sys, threading, time
from pathlib import Path
sys.stdout.reconfigure(encoding='utf8', errors='replace')
sys.stderr.reconfigure(encoding='utf8', errors='replace')
J = sys.modules['__main__']
assert J._is_frozen()
sys.modules['jumpbot'] = J
stop_path = Path({str(stop_path)!r})
def stop_watch():
    while True:
        if stop_path.exists():
            J._STOP_REQUESTED = True
            if J._CONTROL is not None:
                J._CONTROL.stop('验收用户停止；不续下一轮')
            return
        time.sleep(.05)
threading.Thread(target=stop_watch, daemon=True).start()
sys.argv = ['live_acceptance.py', '--execute-live', '--jumps', {str(args.jumps)!r},
            '--backend', {args.backend!r}, '--pause-every', {str(args.pause_every)!r},
            '--out', {str(args.out)!r}]
runpy.run_path({str(ROOT / 'tools/live_acceptance.py')!r}, run_name='__main__')
'''
    child = None
    try:
        with (args.out / 'console.log').open('w', encoding='utf8') as console:
            child = subprocess.Popen([str(args.exe), '-c', code], cwd=str(ROOT),
                                     stdout=console, stderr=subprocess.STDOUT)
            state.update(state='running', child_pid=child.pid)
            save(state_path, state)
            print(json.dumps(state, ensure_ascii=False), flush=True)
            next_update = time.monotonic()
            while child.poll() is None:
                if J.poll_stop_key():
                    stop_path.touch(exist_ok=True)
                if time.monotonic() >= next_update:
                    try:
                        status = json.loads(status_path.read_text(encoding='utf8'))
                        if status.get('pid') == child.pid:
                            state['progress'] = {key: status.get(key) for key in (
                                'state', 'jumps', 'landed', 'deaths', 'hits', 'misses',
                                'unknown', 'mask_backend', 'pause_count', 'stop_reason',
                                'piece_source', 'skin_auto_enabled', 'piece_shape_score')}
                            save(state_path, state)
                            print(json.dumps(state['progress'], ensure_ascii=False), flush=True)
                    except (OSError, ValueError):
                        pass
                    next_update = time.monotonic() + 20
                time.sleep(.1)
        state.update(state='stopped', return_code=child.returncode)
        evidence_path = args.out / 'run-evidence.json'
        if evidence_path.exists():
            evidence = json.loads(evidence_path.read_text(encoding='utf8'))
            state['stop_reason'] = evidence.get('final_status', {}).get('stop_reason', '')
            sys.path.insert(0, str(ROOT / 'tools'))
            from summarize_acceptance import summarize
            try:
                summary = summarize(args.out)
                state['round_pass'] = summary['round_pass']
                state['stats'] = summary['stats']
                state['state'] = 'complete' if summary['round_pass'] else 'stopped'
            except ValueError as exc:
                state['summary_error'] = str(exc)
        save(state_path, state)
        print(json.dumps(state, ensure_ascii=False), flush=True)
        return 0 if state.get('round_pass') else 1
    except BaseException:
        # Stop cooperatively so an outstanding hold gets its normal release.
        stop_path.touch(exist_ok=True)
        if child is not None and child.poll() is None:
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                print('cooperative stop pending; do not start another instance', flush=True)
        raise


if __name__ == '__main__':
    raise SystemExit(main())
