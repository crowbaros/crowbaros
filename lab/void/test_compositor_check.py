"""The compositor check boots Base's image and clears its desktop services."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import compositor_check
import guest

base = guest.void_base


class FakeGuest:
    def __init__(self, work, revision='deadbeef', dirty=False):
        self.work = Path(work)
        self.revision = revision
        self.dirty = dirty
        self.commands = []
        self.started = False
        self.waited = False
        self.stopped = False
        self.discarded = False

    def discard_disk(self):
        self.discarded = True
        return True

    def running(self):
        return self.started and not self.stopped

    def start(self):
        self.started = True

    def wait_ssh(self):
        self.waited = True

    def stop(self):
        self.stopped = True

    def ssh(self, command, check=True, timeout=120, user='root'):
        self.commands.append(command)
        return subprocess.CompletedProcess([], 0, '', '')


class CompositorOnBase(unittest.TestCase):
    def test_uses_bases_lab_account_and_person_uid(self):
        self.assertEqual(compositor_check.PERSON, base.LAB_ACCOUNT['name'])
        self.assertEqual(compositor_check.PERSON_UID, base.PERSON_UID)
        self.assertIn(f'HOME=/home/{base.LAB_ACCOUNT["name"]}', compositor_check.AS_APP)

    def test_apps_refused_matches_policy_source(self):
        self.assertEqual(
            set(compositor_check.apps_refused()),
            {'zwlr_layer_shell_v1', 'ext_session_lock_manager_v1', 'wp_security_context_manager_v1'},
        )

    def test_stop_desktop_services_stops_base_services_and_clears_boot_sockets(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        vm = FakeGuest(temporary.name)
        ev = compositor_check.Evidence(vm)
        self.addCleanup(ev.stream.close)
        compositor_check.stop_desktop_services(ev)
        joined = '\n'.join(vm.commands)
        for service in ('crowbar-shell', 'crowbar-compositor', 'crowbar-launcher',
                        'crowbar-unlock', 'crowbar-ctrl-alt-del'):
            self.assertIn(service, joined)
        # `crowbar-compositor` is longer than the kernel's 15-char comm name,
        # so stopping it must find its PID with pidof rather than `pkill -x`.
        self.assertIn('pidof crowbar-compositor', joined)
        self.assertNotIn('pkill -x', joined)
        self.assertIn('/run/crowbar-trusted', joined)
        self.assertIn('/run/crowbar-apps', joined)

    def main(self, *arguments, passes=True, stopped=None):
        """Run the check's `main` on a guest that is not there, with one
        check that passes or fails, and then what stops the run, if anything."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        vm = FakeGuest(temporary.name, revision='abc1234', dirty=True)

        def run_checks(ev, built):
            ev.check('stub', passes)
            if stopped:
                raise stopped

        with patch.object(base, 'preflight'), \
                patch.object(guest.Guest, 'create', return_value=vm), \
                patch.object(guest, 'build_base', return_value={
                    'crowbar-compositor': 'aaa', 'crowbar-probe': 'bbb'}) as build_base, \
                patch.object(compositor_check, 'run_checks', side_effect=run_checks), \
                patch.object(sys, 'argv', ['compositor_check.py', *arguments]):
            try:
                code = compositor_check.main()
            except BaseException as error:
                code = error
        [log] = vm.work.glob('compositor-*.log')
        return vm, code, build_base, log.read_text()

    def test_a_run_that_fails_keeps_its_disk(self):
        vm, code, _, _ = self.main(passes=False)
        self.assertEqual(code, 1)
        self.assertFalse(vm.discarded)

    def test_a_run_stopped_partway_did_not_pass_and_keeps_its_disk(self):
        for stopped in (KeyboardInterrupt(), SystemExit(1), RuntimeError('the guest went away')):
            vm, code, _, log = self.main(stopped=stopped)
            self.assertFalse(vm.discarded, stopped)
            self.assertTrue(vm.stopped)
            self.assertIn('RESULT: 1 passed, 1 failed', log)
            # A person's Ctrl-C still ends the program as one.
            self.assertEqual(code, 1 if isinstance(stopped, Exception) else stopped)

    def test_a_run_with_no_build_says_it_is_not_evidence(self):
        vm, code, build_base, log = self.main('--no-build')
        self.assertEqual(code, 0)
        build_base.assert_called_once_with(vm.work / 'build.log', build=False)
        self.assertEqual(log.count(guest.NOT_EVIDENCE), 2)
        self.assertIn('RESULT (NOT EVIDENCE): 1 passed, 0 failed', log)
        self.assertNotIn('NOT EVIDENCE', self.main()[3])

    def test_main_builds_base_boots_isolated_run_and_stops_guest(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        vm = FakeGuest(temporary.name, revision='abc1234', dirty=True)
        seen = {}

        def fake_run_checks(ev, built):
            seen['vm'] = ev.vm
            seen['built'] = built
            ev.check('stub', True)

        with patch.object(base, 'preflight') as preflight, \
                patch.object(guest.Guest, 'create', return_value=vm) as create, \
                patch.object(guest, 'build_base', return_value={
                    'crowbar-compositor': 'aaa',
                    'crowbar-probe': 'bbb',
                    'crowbaros-shell': 'ccc',
                    'crowbaros-studio-settings': 'ddd',
                }) as build_base, \
                patch.object(compositor_check, 'run_checks', side_effect=fake_run_checks), \
                patch.object(sys, 'argv', ['compositor_check.py']):
            code = compositor_check.main()

        self.assertEqual(code, 0)
        preflight.assert_called_once_with(vm=True, build=True)
        self.assertEqual(create.call_args.args[0], guest.ROOT / '.lab/void-compositor-checks')
        build_base.assert_called_once_with(vm.work / 'build.log', build=True)
        self.assertTrue(vm.started)
        self.assertTrue(vm.waited)
        self.assertTrue(vm.stopped)
        self.assertTrue(vm.discarded)
        self.assertEqual(seen['vm'], vm)
        self.assertEqual(seen['built'], {'crowbar-compositor': 'aaa', 'crowbar-probe': 'bbb'})
        logs = list(vm.work.glob('compositor-*.log'))
        self.assertEqual(len(logs), 1)
        log = logs[0].read_text()
        self.assertIn('CrowbarOS commit abc1234, with changes that are not committed', log)


if __name__ == '__main__':
    unittest.main()
