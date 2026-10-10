"""Verify a completed live acceptance run; never emits input or scans windows."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

from _scorelog import parse_csv
from report import analyze

ROOT = Path(__file__).resolve().parents[1]


def unrecorded_restarts(rows, reset_indices):
    """Score drops must follow a recorded death or an explicit resume baseline.

    Do not infer a death from manual gameplay across pauses; instead prevent a
    silent automatic restart from passing the 'no missed deaths' safety gate.
    """
    reset_set = set(reset_indices)
    bad = []
    for i, row in enumerate(rows):
        index = int(row['index']) if row.get('index') else None
        if index not in reset_set or '[score基准]' in row.get('note', ''):
            continue
        previous = rows[max(0, i - 2):i]
        if not any(item.get('result') in ('dead', 'died') for item in previous):
            bad.append(index)
    return bad


def summarize(directory: Path) -> dict:
    evidence = json.loads((directory / 'run-evidence.json').read_text(encoding='utf8'))
    candidates = sorted((directory / 'logs').glob('run_*.csv'))
    if len(candidates) != 1:
        raise ValueError('one run CSV required per evidence directory')
    rows = parse_csv(candidates[0])
    stats = analyze(rows)
    audit = [json.loads(line) for line in (directory / 'input-audit.jsonl').read_text(
        encoding='utf8').splitlines()]
    down = up = 0
    owned = False
    balance_errors = []
    paused_since = None
    paused_operations = []
    durations = []
    for event in audit:
        kind = event['kind']
        if kind == 'paused_confirmed':
            paused_since = event['monotonic']
        elif kind == 'programmatic_resume_requested' and paused_since is not None:
            durations.append(event['monotonic'] - paused_since)
            paused_since = None
        elif kind in ('mouse', 'foreground'):
            # Only the control's acknowledgement establishes "已暂停";
            # native hotkey requests during a hold do not shorten that hold.
            if kind == 'mouse':
                control = event.get('control', {})
                if control.get('paused') and control.get('pause_total_s', 0) > 0 and not control.get('input_active'):
                    paused_operations.append(event)
                flags = event['flags']
                if flags & 2:
                    down += 1
                    if owned:
                        balance_errors.append('double down')
                    owned = True
                if flags & 4:
                    up += 1
                    if not owned:
                        balance_errors.append('up without down')
                    owned = False
            elif paused_since is not None:
                paused_operations.append(event)
    if owned:
        balance_errors.append('held at process exit')
    counters = evidence['counters']
    unchanged = all(hashlib.sha256((ROOT / name).read_bytes()).hexdigest() == value
                    for name, value in evidence['source_sha256'].items())
    final = evidence['final_status']
    missed_restarts = unrecorded_restarts(rows, stats['reset_indices'])
    statistic_agreement = all(final.get(key) == stats[mapped] for key, mapped in
                              [('attempts', 'attempts'), ('landed', 'landed'),
                               ('hits', 'score_hits'), ('misses', 'score_misses')])
    rate_gate = (stats['land_rate'] is not None and stats['land_rate'] >= 98 and
                 stats['hit_rate'] is not None and stats['hit_rate'] >= 95)
    completed = stats['attempts'] >= evidence['requested_jumps']
    safety_gate = (not balance_errors and not paused_operations and
                   not counters['paused_violations'] and evidence['exception'] is None and
                   evidence['return_code'] == 0 and not stats['false_death_indices']
                   and not missed_restarts)
    result = dict(requested_jumps=evidence['requested_jumps'], stats=stats,
                  recent100=analyze(rows[-100:]), counters=counters,
                  down=down, up=up, balance_errors=balance_errors,
                  paused_operations=paused_operations, programmed_pause_durations_s=durations,
                  source_unchanged=unchanged, statistic_agreement=statistic_agreement,
                  round_complete=completed, rate_gate=rate_gate, safety_gate=safety_gate,
                  round_pass=completed and rate_gate and safety_gate and unchanged and statistic_agreement,
                  physical_hotkey_repeat_verified=evidence['physical_hotkey_repeat_verified'],
                  controlled_window_switch_verified=evidence['controlled_window_switch_verified'],
                  publication_ready=False, unrecorded_restart_indices=missed_restarts,
                  source_sha256=evidence['source_sha256'])
    (directory / 'summary.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf8')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    result = summarize(args.directory)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result['round_pass'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
