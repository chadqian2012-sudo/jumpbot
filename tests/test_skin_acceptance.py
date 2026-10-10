"""No-input checks for the multi-round escalation gate."""
import unittest

from tools.run_skin_acceptance import ready_for_next_round


class SkinAcceptanceTests(unittest.TestCase):
    def test_only_complete_pass_can_escalate(self):
        passed = dict(state='complete', return_code=0, round_pass=True)
        self.assertTrue(ready_for_next_round(passed, False))
        for key, value in [('state', 'paused'), ('state', 'stopped'),
                           ('return_code', 1), ('round_pass', False), ('round_pass', 1)]:
            with self.subTest(key=key, value=value):
                self.assertFalse(ready_for_next_round({**passed, key: value}, False))
        self.assertFalse(ready_for_next_round(passed, True))
        self.assertFalse(ready_for_next_round({}, False))
