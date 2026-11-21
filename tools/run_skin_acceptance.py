"""Opt-in sequential frozen acceptance: 500/1000/2000, never a daemon.

The normal application remains the only input sender. A failed round, F8,
Ctrl+C or a stop.request file prevents every later round. Only an operator resumes a paused worker.
"""
from __future__ import annotations

import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import time

if __package__:
    from .run_frozen_acceptance import round_command
else:
    from run_frozen_acceptance import round_command
ROUNDS = ((500, 'simd'), (1000, 'auto'), (2000, 'gpu'))


def save(path, data):
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding='utf8')
    tmp.replace(path)


def ready_for_next_round(state, stop_requested):
    return (not stop_requested and state.get('state') == 'complete'
            and state.get('return_code') == 0 and state.get('round_pass') is True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--execute-live', action='store_true')
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if not args.execute_live or os.environ.get('JUMPBOT_NO_INPUT') == '1':
        parser.error('requires --execute-live and NO_INPUT must not be 1')
    exe = args.exe.resolve(strict=True)

    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    stop_path = args.out / 'stop.request'
    status_path = args.out / 'suite-status.json'
    suite = dict(state='running', rounds=[], cumulative_attempts=0,
                 requested_skin='gold', all_rounds_pass=False, stop_reason='')
    active = None
    child = None
    try:
        for jumps, backend in ROUNDS:
            if stop_path.exists() or (ctypes.windll.user32.GetAsyncKeyState(0x77) & 0x8000):
                suite['stop_reason'] = 'user stop before next round'
                break
            active = args.out / f'round-{jumps}-{backend}'
            suite.update(current_round=jumps, current_backend=backend)
            save(status_path, suite)
            command = round_command(exe, active, jumps, backend, suite_stop_file=stop_path)
            with (args.out / f'round-{jumps}-runner.log').open('w', encoding='utf8') as log:
                child = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
                while child.poll() is None:
                    if stop_path.exists() or (ctypes.windll.user32.GetAsyncKeyState(0x77) & 0x8000):
                        stop_path.touch(exist_ok=True)
                    time.sleep(.1)
            state_file = active / 'test-status.json'
            state = json.loads(state_file.read_text(encoding='utf8')) if state_file.exists() else {}
            suite['rounds'].append(dict(jumps=jumps, backend=backend, directory=str(active),
                                        process_return_code=child.returncode, evidence=state))
            suite['cumulative_attempts'] += (state.get('stats') or {}).get('attempts', 0)
            save(status_path, suite)
            if child.returncode != 0 or not ready_for_next_round(state, stop_path.exists()):
                suite['stop_reason'] = state.get('stop_reason') or 'round did not pass; no escalation'
                break
        suite['all_rounds_pass'] = (len(suite['rounds']) == len(ROUNDS)
                                   and not stop_path.exists()
                                   and all(ready_for_next_round(r['evidence'], False)
                                           for r in suite['rounds']))
        suite['state'] = 'complete' if suite['all_rounds_pass'] else 'stopped'
        save(status_path, suite)
        print(json.dumps(suite, ensure_ascii=False), flush=True)
        return 0 if suite['all_rounds_pass'] else 1
    except BaseException:
        stop_path.touch(exist_ok=True)
        if active is not None and active.exists():
            (active / 'stop.request').touch(exist_ok=True)
        if child is not None and child.poll() is None:
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                print('cooperative stop pending; do not start another instance', flush=True)
        suite.update(state='stopped', stop_reason='coordinator interrupted')
        save(status_path, suite)
        raise


if __name__ == '__main__':
    raise SystemExit(main())
