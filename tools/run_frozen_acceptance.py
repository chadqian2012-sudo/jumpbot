"""Developer CLI for the standalone Windows PowerShell bounded runner."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess

RUNNER = Path(__file__).with_suffix('.ps1')


def round_command(exe, out, jumps, backend, pause_every=50, *, suite_stop_file=None):
    powershell = Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
    command = [str(powershell), '-NoLogo', '-NoProfile', '-NonInteractive',
               '-ExecutionPolicy', 'Bypass', '-File', str(RUNNER), '-ExecuteLive',
               '-Exe', str(exe), '-Out', str(out), '-Jumps', str(jumps),
               '-Backend', backend, '-PauseEvery', str(pause_every)]
    if suite_stop_file is not None:
        command.extend(['-SuiteStopFile', str(suite_stop_file)])
    return command


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--execute-live', action='store_true')
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--jumps', type=int, default=500)
    parser.add_argument('--pause-every', type=int, default=50,
                        help='request a pause after this many jumps; resume manually')
    parser.add_argument('--backend', choices=['auto', 'simd', 'gpu', 'opencl'], default='auto')
    args = parser.parse_args()
    if not args.execute_live or os.environ.get('JUMPBOT_NO_INPUT') == '1':
        parser.error('requires --execute-live and JUMPBOT_NO_INPUT must not be 1')
    if min(args.jumps, args.pause_every) < 1:
        parser.error('positive jump counts required')
    return subprocess.call(round_command(args.exe, args.out, args.jumps, args.backend, args.pause_every))


if __name__ == '__main__':
    raise SystemExit(main())
