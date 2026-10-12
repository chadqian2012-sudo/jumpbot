"""Streaming CSV must preserve historical quoting, labels and statistics."""
import csv
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from tools import dashboard as D
import _scorelog as S


class StreamingCSVTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'run_sample.csv'

    def test_bom_quotes_newlines_short_extra_and_duplicate_columns(self):
        content = '\ufeffscore,note,note\r\n2,unused,"tb=1,2,3,4 tw=5\r\n[center x1]"\r\n\r\n3\r\n4,a,b,ignored\r\n'
        self.path.write_text(content, encoding='utf8', newline='')
        expected = [{'score': '2', 'note': 'tb=1,2,3,4 tw=5\r\n[center x1]'},
                    {'score': '3', 'note': ''}, {'score': '4', 'note': 'b'}]
        self.assertEqual(list(S.iter_csv(self.path)), expected)
        self.assertEqual(S.parse_csv(self.path), expected)
        self.assertIsInstance(S.parse_csv(self.path), list)

    def test_empty_and_header_only_files_remain_empty(self):
        for text in ('', '\ufeff', 'score,result\n'):
            self.path.write_text(text, encoding='utf8')
            self.assertEqual(list(S.iter_csv(self.path)), [])
            self.assertEqual(S.parse_csv(self.path), [])

    def test_iterator_opens_lazily_and_can_close_early(self):
        self.path.write_text('score\n1\n2\n', encoding='utf8')
        original = Path.open
        handles = []

        def tracked_open(path, *args, **kwargs):
            handle = original(path, *args, **kwargs)
            handles.append(handle)
            return handle

        with patch.object(Path, 'open', tracked_open):
            rows = S.iter_csv(self.path)
            self.assertFalse(handles)
            self.assertEqual(next(rows), {'score': '1'})
            rows.close()
        self.assertTrue(handles[0].closed)

    def test_iter_rows_adds_file_without_materializing_lists(self):
        self.path.write_text('score\n1\n2\n', encoding='utf8')
        with patch.object(S, 'parse_csv', side_effect=AssertionError('full list')):
            self.assertEqual(list(S.iter_rows([self.path])),
                             [{'score': '1', '_file': self.path.name},
                              {'score': '2', '_file': self.path.name}])

    def test_missing_file_errors_are_not_silently_swallowed(self):
        with self.assertRaises(FileNotFoundError):
            list(S.iter_csv(self.path))

    def test_labels_and_dashboard_stats_match_list_path(self):
        for modern in (False, True):
            header = ['result', 'score', 'note'] + (['mask_backend'] if modern else [])
            source = [['landed', '2', '[score基准]'], ['landed', '3', '[miss]'],
                      ['landed', '35', '[score基准]'], ['landed', '37', '[center x1]'],
                      ['unknown', '', '暂停,无法核对'], ['dead', '', ''],
                      ['landed', '2', '[score基准]'], ['landed', '4', '[center x1]']]
            stream = io.StringIO(newline='')
            writer = csv.writer(stream)
            writer.writerow(header)
            writer.writerows(row + (['simd'] if modern else []) for row in source)
            self.path.write_text(stream.getvalue(), encoding='utf8', newline='')
            self.assertEqual(list(S.iter_labels(S.iter_csv(self.path))),
                             list(S.iter_labels(S.parse_csv(self.path))))
            with patch.object(D, '_CSV_CACHE', {'key': None, 'stats': None}):
                actual = D.stats_from_csv(self.path)
            with patch.object(D, '_CSV_CACHE', {'key': None, 'stats': None}), \
                    patch.object(D, 'iter_csv', S.parse_csv):
                expected = D.stats_from_csv(self.path)
            self.assertEqual(actual, expected)


class NewestCSVTests(unittest.TestCase):
    def candidate(self, mtime=None, error=None):
        return SimpleNamespace(stat=Mock(side_effect=error,
                                        return_value=SimpleNamespace(st_mtime=mtime)))

    def test_latest_and_equal_time_tie_match_old_sorted_last(self):
        paths = [self.candidate(x) for x in (2, 4, 1, 4)]
        expected = sorted(paths, key=lambda p: p.stat().st_mtime)[-1]
        with patch.object(D, 'LOGS_DIR', SimpleNamespace(glob=lambda pattern: iter(paths))):
            self.assertIs(D.newest_csv(), expected)

    def test_disappearing_logs_are_skipped_and_empty_returns_none(self):
        deleted = self.candidate(error=FileNotFoundError())
        inaccessible = self.candidate(error=PermissionError())
        valid = self.candidate(1)
        for paths, expected in (([], None), ([deleted, inaccessible], None),
                                ([deleted, valid, inaccessible], valid)):
            with patch.object(D, 'LOGS_DIR', SimpleNamespace(glob=lambda pattern: iter(paths))):
                self.assertIs(D.newest_csv(), expected)


if __name__ == '__main__':
    unittest.main()
