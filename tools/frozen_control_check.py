"""Run via the frozen exe's -c entry; fake all window/input operations.

The fixture is external to the delivered software. It exercises the embedded
production functions, never captures or inputs to a real window.
"""
import json
import os
import sys
from types import SimpleNamespace

J = sys.modules['__main__']
assert J._is_frozen(), 'must execute using the built jumpbot.exe'
cfg = J.Config.load(J.DEFAULT_CONFIG_PATH)
assert cfg.pause_hotkey == 'Alt+P' and cfg.startup_wait_s == 0 and cfg.start_delay_s == 3


class Clock:
    def __init__(self):
        self.now = 100.0

    def sleep(self, seconds):
        self.now += seconds


clock = Clock()
control = J.RunControl(clock=lambda: clock.now)
win = J.WindowInfo(1, 'fake', 1, 'fake.exe', (0, 0, 553, 1050))
calls = dict(scan=0, foreground=0, mouse=[])
saved = {name: getattr(J, name) for name in ['_CONTROL', 'poll_stop_key', 'find_windows',
                                           'ensure_window_size', 'force_foreground',
                                           'foreground_hwnd', 'point_is_window',
                                           '_send_mouse', 'hold_precise', 'status_write']}
saved_sleep, saved_monotonic = J.time.sleep, J.time.monotonic
saved_no_input = os.environ.get('JUMPBOT_NO_INPUT')


def find(*values):
    assert not control.pause_requested, 'window scan while paused'
    calls['scan'] += 1
    return [] if calls['scan'] < 3 else [win]


def foreground(*values):
    assert not control.pause_requested, 'foreground while paused'
    calls['foreground'] += 1
    return True


try:
    J._CONTROL = control
    J.poll_stop_key = lambda: control.stopped
    J.find_windows = find
    J.ensure_window_size = lambda window, config: window
    J.force_foreground = foreground
    J.foreground_hwnd = lambda: win.hwnd
    J.point_is_window = lambda *values: True
    J._send_mouse = lambda flags, *values: calls['mouse'].append(flags)
    J.status_write = lambda **fields: None
    J.time.sleep = clock.sleep
    J.time.monotonic = lambda: clock.now
    start = clock.now
    assert J.control_ready_window(cfg, initial=True).hwnd == 1
    assert clock.now - start >= 3 and calls['scan'] >= 3
    assert not calls['mouse'], 'input during window wait/countdown'

    # Bypass the environment-only no-input guard solely after replacing the
    # sole foreground mouse sink with a recorder and target checks with fakes.
    os.environ['JUMPBOT_NO_INPUT'] = '0'
    ctrl = J.InputController()
    ctrl.expected_generation = control.generation
    J.hold_precise = lambda seconds: control.toggle('fixture during hold')
    ctrl.press(win, .5, .72, .5)
    assert calls['mouse'] == [0, 2, 4], calls['mouse']
    control.acknowledge_pause()
    frozen_active = control.active_time()
    clock.sleep(180)
    assert abs(control.active_time() - frozen_active) < 1e-6, (control.active_time(), frozen_active)
    try:
        ctrl.press(win, .5, .72, .5)
    except J.ControlInterrupted:
        pass
    else:
        raise AssertionError('paused press was not cancelled')
    assert calls['mouse'] == [0, 2, 4]
    control.toggle('fixture resume')
    vision = SimpleNamespace(reset_prior=lambda: calls.update(prior_reset=True))
    assert J.control_checkpoint(cfg, vision)
    assert calls.get('prior_reset') and not control.needs_resume
    assert abs(control.active_time() - frozen_active) < 1e-6, (control.active_time(), frozen_active)
    assert control.snapshot()['pause_total_s'] >= 183
    calls['mouse'].clear()
    final_checks = []
    def late_pause(*values):
        final_checks.append(1)
        if len(final_checks) == 2:
            control.toggle('fixture last target check')
        return True
    J.point_is_window = late_pause
    ctrl.expected_generation = control.generation
    try:
        ctrl.press(win, .5, .72, .5)
    except J.ControlInterrupted:
        pass
    else:
        raise AssertionError('late pause dispatched DOWN')
    assert calls['mouse'] == [0]
    control.toggle('fixture pause again')
    control.stop('fixture stop')
    try:
        J.control_ready_window(cfg)
    except J.ControlInterrupted:
        pass
    else:
        raise AssertionError('stop did not override resume')
    print(json.dumps(dict(frozen=True, game_input=False, config_defaults=True,
                          startup_wait=True, countdown=True, pause_hold_release=True,
                          paused_no_input=True, long_pause=True, fresh_resume=True,
                          atomic_down=True, stop_priority=True), ensure_ascii=False))
finally:
    for name, value in saved.items():
        setattr(J, name, value)
    J.time.sleep, J.time.monotonic = saved_sleep, saved_monotonic
    if saved_no_input is None:
        os.environ.pop('JUMPBOT_NO_INPUT', None)
    else:
        os.environ['JUMPBOT_NO_INPUT'] = saved_no_input
