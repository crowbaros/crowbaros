"""The desktop check runs every Base boot check and reports each."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import desktop_check
import guest
import test_guest

base = guest.void_base


class FakeGuest:
    def __init__(self, work, responder):
        self.work = Path(work)
        self.responder = responder
        self.calls = []

    def ssh(self, command, check=True, timeout=120, user='root'):
        self.calls.append((user, command))
        returncode, stdout, stderr = self.responder(user, command)
        return subprocess.CompletedProcess([], returncode, stdout, stderr)


class BaseBootChecks(unittest.TestCase):
    def make_check(self, responder):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        vm = FakeGuest(temporary.name, responder)
        check = desktop_check.Check(vm)
        self.addCleanup(check.logfile.close)
        return vm, check

    def test_base_checks_runs_every_check_in_void_base_with_the_right_user(self):
        sample = [
            ('Void Linux', 'root', 'cat /etc/os-release', lambda out: 'ID=void' in out),
            ('person ssh', base.LAB_ACCOUNT['name'], 'id -u', lambda out: out.strip() == '1003'),
            ('booted with UEFI', 'root', 'test -d /sys/firmware/efi && echo efi', lambda out: 'efi' in out),
        ]

        def responder(user, command):
            if command == 'cat /etc/os-release':
                return 0, 'ID=void\n', ''
            if command == 'id -u':
                return 0, ('1003\n' if user == base.LAB_ACCOUNT['name'] else '0\n'), ''
            return 0, 'efi\n', ''

        vm, check = self.make_check(responder)
        with patch.object(base, 'boot_checks', return_value=sample):
            check.base_checks()
        self.assertEqual(vm.calls, [
            ('root', 'cat /etc/os-release'),
            (base.LAB_ACCOUNT['name'], 'id -u'),
            ('root', 'test -d /sys/firmware/efi && echo efi'),
        ])
        self.assertEqual(
            [(row['name'], row['passed']) for row in check.results],
            [('Void Linux', True), ('person ssh', True), ('booted with UEFI', True)],
        )
        log = (vm.work / 'desktop.log').read_text()
        for name, *_ in sample:
            self.assertIn(f'PASS: {name}', log)

    def test_base_checks_reports_every_check_even_when_one_fails(self):
        sample = [
            ('first passes', 'root', 'cmd1', lambda out: out == 'ok'),
            ('second fails on output', 'root', 'cmd2', lambda out: out == 'ok'),
            ('third fails on exit', 'root', 'cmd3', lambda out: True),
            ('fourth still runs', 'root', 'cmd4', lambda out: out == 'ok'),
        ]

        def responder(user, command):
            if command == 'cmd2':
                return 0, 'bad', ''
            if command == 'cmd3':
                return 1, '', 'boom'
            return 0, 'ok', ''

        vm, check = self.make_check(responder)
        with patch.object(base, 'boot_checks', return_value=sample):
            with self.assertRaises(AssertionError) as error:
                check.base_checks()
        self.assertEqual(
            [(row['name'], row['passed']) for row in check.results],
            [
                ('first passes', True),
                ('second fails on output', False),
                ('third fails on exit', False),
                ('fourth still runs', True),
            ],
        )
        self.assertIn('second fails on output', str(error.exception))
        self.assertIn('third fails on exit', str(error.exception))
        log = (vm.work / 'desktop.log').read_text()
        self.assertIn('FAIL: second fails on output', log)
        self.assertIn('FAIL: third fails on exit', log)
        self.assertIn('PASS: fourth still runs', log)



class FakeQmp:
    """Keys go nowhere but to `pressed`."""
    def __init__(self, pressed):
        self.pressed = pressed

    def key(self, qcode, down):
        if down:
            self.pressed(qcode)


class StillDesktopAndBar(unittest.TestCase):
    """The checks that a still desktop draws nothing (#269), and that the bar
    follows the workspaces at once (#273), read Fulcrum's log."""

    def make_check(self, responder):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        check = desktop_check.Check(FakeGuest(temporary.name, responder))
        self.addCleanup(check.logfile.close)
        sleep = patch.object(desktop_check.time, 'sleep')
        sleep.start()
        self.addCleanup(sleep.stop)
        return check

    def still(self, reports):
        counts = iter(reports)

        def responder(user, command):
            if command == 'date +%S':
                return 0, '10\n', ''
            if 'commits so far' in command:
                return 0, f'{next(counts)}\n', ''
            return 1, '', 'unexpected'
        check = self.make_check(responder)
        try:
            check.still('still')
        except AssertionError:
            pass
        return check.results

    def test_a_desktop_that_keeps_drawing_fails(self):
        self.assertEqual(self.still([4, 7]), [{'name': 'still', 'passed': False, 'detail': '3 reports in 12 s'}])

    def test_a_still_desktop_passes(self):
        self.assertEqual(self.still([4, 4]), [{'name': 'still', 'passed': True, 'detail': '0 reports in 12 s'}])

    def bar(self, delays):
        drawn, delays = [], iter(delays)

        def responder(user, command):
            if 'layer crowbaros-bar drew' in command:
                return 0, ''.join(f'INFO layer crowbaros-bar drew {ms} ms after the workspaces changed\n'
                                  for ms in drawn), ''
            if 'control.sock' in command:
                return 0, '{"workspace": 1}', ''
            return 1, '', 'unexpected'
        check = self.make_check(responder)
        check.qmp = FakeQmp(lambda key: key in '12' and drawn.append(next(delays)))
        try:
            check.bar_follows_workspaces()
        except AssertionError:
            pass
        return check.results

    def test_a_bar_that_draws_within_two_frames_passes(self):
        [result] = self.bar([24.1, 23.5, 40.0, 25.6])
        self.assertTrue(result['passed'], result)
        self.assertEqual(result['detail'], '23.5, 24.1, 25.6, 40.0 ms; at most 33.3 ms')

    def test_a_bar_that_waits_for_a_poll_fails(self):
        [result] = self.bar([782.0, 838.0, 700.0, 95.0])
        self.assertFalse(result['passed'], result)


class DisplayUnplugged(unittest.TestCase):
    def test_layer_commits_reads_the_last_report(self):
        log = ('INFO crowbar_compositor: commits so far: layer crowbaros-bar: 3, layer crowbaros-dock: 3 frames=4\n'
               'INFO crowbar_compositor: output disconnected name=Virtual-1\n'
               'INFO crowbar_compositor: commits so far: layer crowbaros-bar: 9, layer crowbaros-dock: 8, '
               'layer crowbaros-wallpaper: 7, window 0x2d foot: 2 frames=12\n')

        def responder(user, command):
            if 'commits so far' in command:
                return 0, log.splitlines()[-1] + '\n', ''
            return 1, '', ''

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        check = desktop_check.Check(FakeGuest(temporary.name, responder))
        self.addCleanup(check.logfile.close)
        self.assertEqual(check.layer_commits(), {'bar': 9, 'dock': 8, 'wallpaper': 7})


class AgentSurface(unittest.TestCase):
    """What the Agent's surface checks read: a pixel of a screenshot, and
    how often a log says something."""

    def make_check(self, responder):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        check = desktop_check.Check(FakeGuest(temporary.name, responder))
        self.addCleanup(check.logfile.close)
        return check

    def test_a_colour_is_like_another_only_within_what_a_renderer_rounds(self):
        light = desktop_check.CANVAS['light']
        self.assertEqual(light, (244, 247, 249))
        self.assertTrue(desktop_check.like((243, 249, 249), light))
        self.assertFalse(desktop_check.like((241, 247, 249), light))
        self.assertFalse(desktop_check.like(desktop_check.CANVAS['dark'], light))
        # No colour at all is like none.
        self.assertFalse(desktop_check.like((), light))

    def test_a_pixel_is_read_from_the_display_asked_for(self):
        class Screens:
            def screendump(self, path, head):
                # The picture of test_guest's Screendumps, on the second head only.
                Path(path).write_bytes(test_guest.Screendumps.RGB if head == 1 else b'no picture')

        check = self.make_check(lambda user, command: (1, '', ''))
        check.qmp = Screens()
        self.assertEqual(check.pixel('second', 3, 2, head=1), (32, 157, 77))
        self.assertTrue((check.work / 'second.png').exists())
        with self.assertRaises(ValueError):
            check.pixel('first', 3, 2)
        # A point the picture does not have is an error, not a colour.
        for x, y in ((7, 2), (3, 5)):
            with self.assertRaises(ValueError):
                check.pixel('second', x, y, head=1)

    def test_a_log_is_counted_by_its_lines_and_a_log_without_the_text_counts_none(self):
        asked = []

        def responder(user, command):
            asked.append(command)
            # grep prints 0 and exits 1 when no line matches.
            return (0, '3\n', '') if 'summon output=' in command else (1, '0\n', '')
        check = self.make_check(responder)
        self.assertEqual(check.count('summon output='), 3)
        self.assertEqual(check.count("agent's surface"), 0)
        self.assertEqual(asked[0], "grep -c -e 'summon output=' /var/log/crowbar-compositor.log")


class Runs(unittest.TestCase):
    """What a whole run leaves behind, on a guest that is not there."""

    class Guest:
        def __init__(self, work):
            self.work = Path(work)
            self.discarded = False

        def start(self):
            pass

        def wait_ssh(self):
            pass

        def running(self):
            return False

        def discard_disk(self):
            self.discarded = True
            return True

    def main(self, *arguments, passes=True, stopped=None):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        vm = self.Guest(temporary.name)

        def checks(check, built):
            check.record('stub', passes)
            if stopped:
                raise stopped

        with patch.object(base, 'preflight'), patch.object(guest.Guest, 'create', return_value=vm), \
                patch.object(guest, 'build_base', return_value={}) as build_base, \
                patch.object(desktop_check.Check, 'checks', checks), \
                patch('sys.argv', ['desktop_check.py', *arguments]):
            try:
                code = desktop_check.main()
            except BaseException as error:
                code = error
        return vm, code, build_base, (vm.work / 'desktop.log').read_text()

    def test_a_run_that_passes_deletes_its_disk_and_one_that_fails_keeps_it(self):
        vm, code, build_base, log = self.main()
        self.assertEqual((code, vm.discarded), (0, True))
        build_base.assert_called_once_with(vm.work / 'build.log', build=True)
        self.assertIn('RESULT: 1 passed, 0 failed', log)
        self.assertNotIn('NOT EVIDENCE', log)
        self.assertTrue((vm.work / 'results.json').exists())
        vm, code, _, _ = self.main(passes=False)
        self.assertEqual((code, vm.discarded), (1, False))

    def test_a_run_stopped_partway_did_not_pass_and_keeps_its_disk(self):
        for stopped in (KeyboardInterrupt(), SystemExit(1), RuntimeError('the guest went away')):
            vm, code, _, log = self.main(stopped=stopped)
            self.assertFalse(vm.discarded, stopped)
            self.assertIn('RESULT: 1 passed, 1 failed', log)
            rows = json.loads((vm.work / 'results.json').read_text())
            self.assertEqual([row['passed'] for row in rows], [True, False])
            # A person's Ctrl-C still ends the program as one.
            self.assertEqual(code, 1 if isinstance(stopped, Exception) else stopped)

    def test_a_run_with_no_build_leaves_nothing_that_reads_as_evidence(self):
        vm, code, build_base, log = self.main('--no-build')
        self.assertEqual(code, 0)
        build_base.assert_called_once_with(vm.work / 'build.log', build=False)
        self.assertFalse((vm.work / 'results.json').exists())
        self.assertTrue((vm.work / 'results-not-evidence.json').exists())
        self.assertIn('RESULT (NOT EVIDENCE): 1 passed, 0 failed', log)
        self.assertIn(f'Not evidence: {vm.work}', log)
        self.assertNotIn('] Evidence:', log)


if __name__ == '__main__':
    unittest.main()
