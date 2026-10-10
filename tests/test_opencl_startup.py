"""No-input driver preflight failures must not reach the parent's ICD loader."""
import dataclasses
import os
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import jumpbot as J


class OpenCLStartupTests(unittest.TestCase):
    def test_native_failure_is_cached_before_parent_import(self):
        cfg = dataclasses.replace(J.Config(), opencl_enabled=True, opencl_device='fixture-device')
        environment = dict(os.environ)
        environment.pop('JUMPBOT_OPENCL_PROBE', None)
        environment.pop('JUMPBOT_DISABLE_OPENCL', None)
        result = SimpleNamespace(returncode=-1073741676, stdout='', stderr='')
        with patch.dict(os.environ, environment, clear=True), \
                patch.dict(J._OPENCL_ENUMERATION_CHECKS, {}, clear=True), \
                patch.object(J.subprocess, 'run', return_value=result) as run:
            self.assertFalse(J.Vision(cfg)._init_opencl())
            self.assertFalse(J.Vision(cfg)._init_opencl())
            run.assert_called_once()
            command = run.call_args.args[0]
            self.assertEqual(command[-2:], ['--_opencl-check', 'fixture-device'])
            env = run.call_args.kwargs['env']
            self.assertEqual(env['JUMPBOT_NO_INPUT'], '1')
            self.assertEqual(env['JUMPBOT_OPENCL_PROBE'], '1')
            self.assertEqual(env['JUMPBOT_DISABLE_CUDA'], '1')

    def test_timeout_keeps_cpu_available(self):
        cfg = dataclasses.replace(J.Config(), opencl_enabled=True)
        environment = dict(os.environ)
        environment.pop('JUMPBOT_OPENCL_PROBE', None)
        environment.pop('JUMPBOT_DISABLE_OPENCL', None)
        with patch.dict(os.environ, environment, clear=True), \
                patch.dict(J._OPENCL_ENUMERATION_CHECKS, {}, clear=True), \
                patch.object(J.subprocess, 'run', side_effect=subprocess.TimeoutExpired('fixture', 20)):
            vision = J.Vision(cfg)
            self.assertFalse(vision._init_opencl())
            self.assertFalse(vision._cl_state)
