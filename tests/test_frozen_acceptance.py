"""Exercise the standalone runner with a temporary text-replay EXE (no input APIs)."""
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

from tools.run_frozen_acceptance import RUNNER, round_command


MOCK = r'''
using System;
using System.IO;
using System.Text;
class Worker {
    static int Main(string[] args) {
        Console.OutputEncoding = new UTF8Encoding(false);
        var stdin = new StreamReader(Console.OpenStandardInput(), new UTF8Encoding(false, true), false);
        if (Environment.GetEnvironmentVariable("JUMPBOT_FIXTURE_HOLD") == "1") {
            stdin.ReadLine();
            return 0;
        }
        File.AppendAllText("launches", "launch\n");
        var logs = args[Array.IndexOf(args, "--log-dir") + 1];
        Directory.CreateDirectory(logs);
        if (File.Exists("rows.csv")) File.Copy("rows.csv", Path.Combine(logs, "run_fixture.csv"));
        File.WriteAllLines(Path.Combine(logs, "args"), args);
        foreach (var line in File.ReadAllLines("transcript")) {
            if (line == "!stdin") {
                var received = stdin.ReadLine();
                File.AppendAllText("received", (received ?? "EOF") + "\n");
            } else if (line.StartsWith("!stderr ")) Console.Error.WriteLine(line.Substring(8));
            else Console.WriteLine(line);
        }
        return File.Exists("exit-code") ? 7 : 0;
    }
}
'''


def snapshot(state='stopped', n=100, landed=98, hits=76, misses=4, reason='达到最大跳跃次数', paused=False):
    return '@jumpbot ' + json.dumps(dict(
        state=state, stop_reason=reason, paused=paused, jumps=n, attempts=n, deaths=n-landed,
        landed=landed, hits=hits, misses=misses, judged=hits+misses,
        unknown=landed-hits-misses, landing_rate=100, hit_rate=100), ensure_ascii=False)


def rows(n=100, landed=98, hits=76, misses=4):
    values = [('landed', '1')] * hits + [('landed', '0')] * misses
    values += [('landed', '')] * (landed-hits-misses) + [('dead', '')] * (n-landed)
    return 'index,result,center_hit\n' + ''.join(
        f'{index},{result},{hit}\n' for index, (result, hit) in enumerate(values, 1))


@unittest.skipUnless(os.name == 'nt', 'Windows PowerShell worker protocol')
class FrozenAcceptanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix='jumpbot mock ')
        cls.addClassCleanup(cls.tmp.cleanup)
        cls.root = Path(cls.tmp.name)
        src = cls.root / 'Worker.cs'
        src.write_text(MOCK, encoding='utf8')
        csc = Path(os.environ['SystemRoot']) / 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
        cls.fixture = cls.root / 'fixture.exe'
        subprocess.run([str(csc), '/nologo', '/target:exe', '/out:' + str(cls.fixture), str(src)],
                       check=True, capture_output=True, timeout=30)

    def setUp(self):
        self.app = Path(tempfile.mkdtemp(dir=self.root, prefix='模拟 '))
        self.exe = self.app / 'jumpbot.exe'
        shutil.copyfile(self.fixture, self.exe)
        build = self.app / 'build-source.json'
        build.write_text(json.dumps(dict(commit='fixture', files_sha256={
            'jumpbot.exe': hashlib.sha256(self.exe.read_bytes()).hexdigest()})), encoding='utf8')
        (self.app / 'source-version.json').write_text(json.dumps(dict(commit='fixture',
            build_manifest_sha256=hashlib.sha256(build.read_bytes()).hexdigest())), encoding='utf8')
        self.out = self.app / 'round [1]'
        self.cmd = round_command(self.exe, self.out, 100, 'simd', 50)
        self.env = os.environ.copy()
        self.env.pop('JUMPBOT_NO_INPUT', None)  # Only the compiled text fixture is addressable.

    def replay(self, lines, csv=rows(), cmd=None):
        (self.app / 'transcript').write_text('\n'.join(lines), encoding='utf8')
        if csv is not None:
            (self.app / 'rows.csv').write_text(csv, encoding='utf8')
        return subprocess.run(cmd or self.cmd, cwd=self.app, env=self.env, capture_output=True, timeout=20)

    def state(self):
        return json.loads((self.out / 'test-status.json').read_bytes())

    def test_threshold_equality_with_unjudged_landings_and_sibling_exe(self):
        panel = self.app / 'jumpbot-desktop.exe'
        panel.write_bytes(b'never executable')
        proc = self.replay(['!stderr @jumpbot garbage', 'worker log', snapshot()],
                           cmd=round_command(panel, self.out, 100, 'simd'))
        self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
        args = (self.out / 'logs/args').read_text(encoding='utf8').splitlines()
        self.assertEqual(args[0:5], ['--worker', '--max-jumps', '100', '--backend', 'simd'])
        self.assertEqual(args[args.index('--log-dir')+1], str(self.out / 'logs'))
        self.assertTrue({'--no-cuda', '--no-opencl'} <= set(args))
        self.assertNotIn('--daemon', args)
        self.assertTrue(self.state()['round_pass'])
        self.assertEqual(self.state()['stats']['judged'], 80)
        self.assertEqual(json.loads((self.out / 'run-evidence.json').read_bytes())['final_status']['landed'], 98)
        self.assertEqual(json.loads((self.out / 'summary.json').read_bytes()), self.state())

    def test_incomplete_bad_or_unverifiable_counts_never_pass(self):
        cases = [
            ([snapshot(landed=97)], rows(landed=97)),
            ([snapshot(hits=75, misses=5)], rows(hits=75, misses=5)),
            ([snapshot(hits=0, misses=0)], rows(hits=0, misses=0)),
            ([snapshot(n=99)], rows(n=99)),
            ([snapshot()], rows().replace('100,dead,', '99,dead,')),
            ([snapshot()], rows(hits=75, misses=5)),
            ([snapshot()], None),
            ([snapshot(state='playing')], rows()),
            ([snapshot(reason='用户停止')], rows()),
            ([snapshot(reason='达到最长运行时间')], rows()),
            ([snapshot(paused=True)], rows()),
            (['!stderr ' + snapshot()], rows()),
            ([snapshot(), '@jumpbot {'], rows()),
        ]
        for i, (lines, csv) in enumerate(cases):
            with self.subTest(case=i):
                self.out = self.app / f'round {i}'
                (self.app / 'rows.csv').unlink(missing_ok=True)
                proc = self.replay(lines, csv, cmd=round_command(self.exe, self.out, 100, 'simd'))
                self.assertNotEqual(proc.returncode, 0)
                self.assertFalse(self.state()['round_pass'])
                self.assertTrue(self.state()['stop_reason'])

    def test_coincident_operator_pause_and_stop_during_silent_stdout(self):
        checkpoint = snapshot('playing', n=50, landed=50, hits=50, misses=0, reason='')
        paused = snapshot('paused', n=50, landed=50, hits=50, misses=0, reason='')
        # The operator pauses before the pending checkpoint request reaches the child.
        (self.app / 'transcript').write_text('\n'.join(
            [checkpoint, paused, '!stdin', '!stdin', snapshot(reason='用户停止')]), encoding='utf8')
        with subprocess.Popen(self.cmd, cwd=self.app, env=self.env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE) as proc:
            try:
                deadline = time.monotonic() + 12
                received = self.app / 'received'
                while not received.exists() and time.monotonic() < deadline and proc.poll() is None:
                    time.sleep(.05)
                self.assertTrue(received.exists(), 'worker never received checkpoint pause')
                self.assertEqual(received.read_text().splitlines(), ['pause'])
                (self.out / 'stop.request').touch()
                proc.communicate(timeout=8)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.communicate(timeout=5)
        self.assertEqual(received.read_text().splitlines(), ['pause', 'stop'])
        self.assertNotEqual(proc.returncode, 0)
        self.assertFalse(self.state()['round_pass'])

    def test_suite_stops_after_child_exit_without_samples(self):
        suite = [sys.executable, '-B', str(RUNNER.with_name('run_skin_acceptance.py')),
                 '--execute-live', '--exe', str(self.exe), '--out', str(self.out)]
        (self.app / 'exit-code').write_text('7')
        proc = self.replay([snapshot(reason='用户停止')], csv=None, cmd=suite)
        self.assertNotEqual(proc.returncode, 0)
        result = json.loads((self.out / 'suite-status.json').read_bytes())
        self.assertEqual(result['state'], 'stopped')
        self.assertEqual(result['cumulative_attempts'], 0)
        self.assertEqual(len(result['rounds']), 1)
        self.assertEqual(result['rounds'][0]['evidence']['return_code'], 7)
        self.assertEqual((self.app / 'launches').read_text().splitlines(), ['launch'])

    def test_offline_jobs_can_coexist_but_possible_input_owners_block_launch(self):
        python_mock = self.app / 'python.exe'
        shutil.copyfile(self.fixture, python_mock)
        with ExitStack() as processes:
            def hold(executable, *args):
                # This is always a private copy of the text fixture. EOF releases it.
                child = processes.enter_context(subprocess.Popen(
                    [str(executable), *args], cwd=self.app, stdin=subprocess.PIPE,
                    env={**self.env, 'JUMPBOT_FIXTURE_HOLD': '1'}))
                processes.callback(child.stdin.close)
                return child.pid

            hold(self.exe, '--config', 'config with spaces.json', '--backend', 'simd',
                 '--bench-repeat', '3', '--image', 'frame 1.png', '--image', 'frame 2.png')
            hold(self.exe, '--_opencl-check', 'list')
            passed = self.replay([snapshot()])
            self.assertEqual(passed.returncode, 0, passed.stderr + passed.stdout)

            owners = [
                hold(self.exe, '--worker', '--max-jumps', '0'),
                hold(self.exe, '--probe', '--click-xy', '0.5,0.5'),
                hold(self.exe, '--config', 'directory --image frame/config.json'),
                hold(self.exe, '--_opencl-check', 'list', '--worker'),
                hold(self.exe, '-m', 'jumpbot'),
                hold(python_mock, 'jumpbot.py', '--probe', '--click-xy', '0.5,0.5'),
            ]
            dest = self.app / 'blocked'
            blocked = self.replay([snapshot()], cmd=round_command(self.exe, dest, 100, 'simd'))
            self.assertNotEqual(blocked.returncode, 0)
            # PowerShell may wrap the displayed exception between PIDs.
            error = b''.join(blocked.stderr.split())
            for pid in owners:
                self.assertIn(str(pid).encode(), error)
            self.assertFalse(dest.exists())
            self.assertEqual((self.app / 'launches').read_text().splitlines(), ['launch'])

    def test_preflight_and_legacy_cli(self):
        standalone = self.cmd.copy()
        standalone.remove('-ExecuteLive')
        no_input = {**self.env, 'JUMPBOT_NO_INPUT': '1'}
        for cmd, env in [(standalone, self.env), (self.cmd, no_input)]:
            self.assertNotEqual(subprocess.run(cmd, env=env, capture_output=True, timeout=10).returncode, 0)
        sentinel = self.app / 'stop.request'
        sentinel.touch()
        stopped = self.replay([], cmd=round_command(self.exe, self.app / 'stopped', 100, 'simd',
                                                     suite_stop_file=sentinel))
        self.assertNotEqual(stopped.returncode, 0)
        self.assertFalse((self.app / 'launches').exists())
        old_cli = [sys.executable, '-B', str(RUNNER.with_suffix('.py')), '--execute-live',
                   '--exe', str(self.exe), '--out', str(self.out), '--jumps', '100', '--backend', 'simd']
        self.assertEqual(self.replay([snapshot()], cmd=old_cli).returncode, 0)
        self.assertNotEqual(self.replay([snapshot()], cmd=old_cli).returncode, 0)  # Existing run.
        (self.app / 'source-version.json').write_text('{}', encoding='utf8')
        self.out = self.app / 'new'
        self.assertNotEqual(self.replay([snapshot()], cmd=round_command(
            self.exe, self.out, 100, 'simd')).returncode, 0)
        self.assertEqual((self.app / 'launches').read_text().splitlines(), ['launch'])


if __name__ == '__main__':
    unittest.main()
