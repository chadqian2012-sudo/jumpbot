"""Panel-only cache tests: no process scan, capture, or game input."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from tools import dashboard as D


class StatusReaderTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'status.json'
        self.reader = D.StatusReader(self.path)
        self.clock = 10.0
        self.wall = 1000.0
        for name, callback in [('monotonic', lambda: self.clock),
                               ('time', lambda: self.wall)]:
            context = patch.object(D.time, name, side_effect=callback)
            context.start()
            self.addCleanup(context.stop)

    def write(self, **changes):
        data = dict(updated_at=self.wall, state='playing', jumps=168, alive=True)
        data.update(changes)
        temporary = self.path.with_suffix('.tmp')
        temporary.write_text(json.dumps(data), encoding='utf8')
        temporary.replace(self.path)
        return data

    def test_unchanged_status_parses_once_for_eight_panel_frames(self):
        data = self.write()
        with patch.object(D.json, 'load', wraps=json.load) as parse:
            for _ in range(8):
                self.assertEqual(self.reader.read(), data)
                self.clock += .25
            self.assertEqual(parse.call_count, 1)
            self.assertEqual(self.reader.read(), data)
            self.assertEqual(parse.call_count, 2)

    def test_equal_size_and_mtime_atomic_replacement_is_seen_next_poll(self):
        self.write(state='playing')
        self.reader.read()
        previous = self.path.stat()
        replacement = self.write(state='stopped', alive=True)
        self.assertEqual(self.path.stat().st_size, previous.st_size)
        os.utime(self.path, ns=(previous.st_atime_ns, previous.st_mtime_ns))
        self.assertEqual(self.reader.read(), replacement)

    def test_cache_hit_still_expires_at_120_seconds(self):
        self.write(updated_at=881.0)
        self.assertIsNotNone(self.reader.read())
        self.wall += 1
        self.assertIsNone(self.reader.read())

    def test_missing_corrupt_and_non_object_status_never_reuse_old_data(self):
        for raw in (None, '{', '[]', 'null', '{}', '{"updated_at":null}',
                    '{"updated_at":"NaN"}', '{"updated_at":"Infinity"}', b'\xff'):
            with self.subTest(raw=raw):
                self.write()
                self.assertIsNotNone(self.reader.read())
                if raw is None:
                    self.path.unlink()
                elif isinstance(raw, bytes):
                    self.path.write_bytes(raw)
                else:
                    self.path.write_text(raw, encoding='utf8')
                self.assertIsNone(self.reader.read())
                self.assertEqual(self.reader.read(), None)
                restored = self.write(state='paused')
                self.assertEqual(self.reader.read(), restored)

    def test_huge_timestamp_falls_back_instead_of_crashing(self):
        self.write(updated_at=10 ** 1000)
        self.assertIsNone(self.reader.read())

    def test_coarse_metadata_is_revalidated_within_two_seconds(self):
        self.write()
        with patch.object(self.reader, '_signature', return_value=('coarse',)):
            self.reader.read()
            self.write(state='paused')
            self.clock += 1.75
            self.assertEqual(self.reader.read()['state'], 'playing')
            self.clock += .25
            self.assertEqual(self.reader.read()['state'], 'paused')

    def test_read_failure_retries_next_frame_without_old_snapshot(self):
        self.write()
        self.reader.read()
        self.clock += 2
        with patch.object(Path, 'open', side_effect=PermissionError):
            self.assertIsNone(self.reader.read())
        self.assertIsNotNone(self.reader.read())

    def test_in_place_write_during_read_is_not_cached(self):
        self.write()
        original = json.load

        def concurrent_write(stream):
            data = original(stream)
            self.path.write_text(json.dumps(dict(data, jumps=10000)), encoding='utf8')
            return data

        with patch.object(D.json, 'load', side_effect=concurrent_write):
            self.assertIsNone(self.reader.read())
        self.assertEqual(self.reader.read()['jumps'], 10000)

    def test_replacement_between_stat_and_open_binds_actual_open_file(self):
        self.write()
        original = Path.open
        replacement = None

        def replace_then_open(path, *args, **kwargs):
            nonlocal replacement
            with patch.object(Path, 'open', original):
                replacement = self.write(state='paused')
                return original(path, *args, **kwargs)

        with patch.object(Path, 'open', replace_then_open):
            self.assertEqual(self.reader.read()['state'], 'paused')
        self.assertEqual(self.reader.read(), replacement)

    def test_clock_rollback_forces_a_read(self):
        self.write()
        with patch.object(D.json, 'load', wraps=json.load) as parse:
            self.reader.read()
            self.clock -= 1
            self.reader.read()
            self.assertEqual(parse.call_count, 2)


class ProcessCacheTests(unittest.TestCase):
    def test_empty_missing_and_failed_scan_are_cached_for_ten_seconds(self):
        for result in ('', '[]', 'not JSON', subprocess.TimeoutExpired('fake', 8)):
            with self.subTest(result=result):
                clock = [20.0]
                kwargs = ({'side_effect': result} if isinstance(result, Exception)
                          else {'return_value': subprocess.CompletedProcess([], 0, result)})
                with patch.object(D.os, 'name', 'nt'), \
                        patch.object(D, '_PID_CACHE', (-float('inf'), None, '')), \
                        patch.object(D.time, 'monotonic', side_effect=lambda: clock[0]), \
                        patch.object(D.subprocess, 'run', **kwargs) as scan:
                    for _ in range(40):
                        self.assertEqual(D.bot_process(), (None, ''))
                        clock[0] += .25
                    self.assertEqual(scan.call_count, 1)
                    D.bot_process()
                    self.assertEqual(scan.call_count, 2)


if __name__ == '__main__':
    unittest.main()
