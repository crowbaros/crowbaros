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

    def ssh(self, command, check=True, timeout=120, user='root', input=None):
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

    def main(self, *arguments, passes=True, stopped=None, env=None):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        vm = self.Guest(temporary.name)

        def checks(check, built, credential=None):
            check.record('stub', passes)
            if stopped:
                raise stopped

        test_env = {'ANTHROPIC_API_KEY': 'test-only-credential'} if env is None else env
        with patch.dict(desktop_check.os.environ, test_env, clear=True), \
                patch.object(base, 'preflight'), patch.object(guest.Guest, 'create', return_value=vm), \
                patch.object(guest, 'build_base', return_value={}) as build_base, \
                patch.object(desktop_check.Check, 'checks', checks), \
                patch('sys.argv', ['desktop_check.py', *arguments]):
            try:
                code = desktop_check.main()
            except BaseException as error:
                code = error
        log = (vm.work / 'desktop.log').read_text() if (vm.work / 'desktop.log').exists() else ''
        return vm, code, build_base, log

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



class RealModelCredentialAndTyping(unittest.TestCase):
    """The desktop check requires a real Model credential from the host
    environment, fails calmly before building without one, encodes QMP text
    cleanly, and catches any credential leak into evidence."""

    def test_missing_credential_exits_and_names_what_it_needs(self):
        with self.assertRaises(SystemExit) as stopped:
            desktop_check.real_model_credential({})
        message = str(stopped.exception)
        self.assertIn('ANTHROPIC_API_KEY', message)
        self.assertIn('OPENAI_API_KEY', message)
        self.assertIn('GEMINI_API_KEY', message)
        self.assertIn('CROWBAR_AGENT_PROVIDER', message)
        self.assertIn('CROWBAR_AGENT_API_KEY', message)

    def test_main_stops_before_building_when_no_credential_is_set(self):
        with patch.dict(desktop_check.os.environ, {}, clear=True), \
                patch.object(base, 'preflight') as preflight, \
                patch.object(guest, 'build_base') as build_base, \
                patch('sys.argv', ['desktop_check.py']):
            with self.assertRaises(SystemExit) as stopped:
                desktop_check.main()
        self.assertIn('ANTHROPIC_API_KEY', str(stopped.exception))
        preflight.assert_not_called()
        build_base.assert_not_called()

    def test_explicit_provider_and_key_win_over_ambient_keys(self):
        cred = desktop_check.real_model_credential({
            'ANTHROPIC_API_KEY': 'ambient-anthropic',
            'CROWBAR_AGENT_PROVIDER': 'openai',
            'CROWBAR_AGENT_API_KEY': 'explicit-openai',
            'CROWBAR_AGENT_MODEL': 'gpt-4o-mini',
        })
        self.assertEqual(cred, {
            'provider': 'openai',
            'api_key': 'explicit-openai',
            'model': 'gpt-4o-mini',
        })

    def test_explicit_key_without_provider_is_refused(self):
        with self.assertRaises(SystemExit) as stopped:
            desktop_check.real_model_credential({'CROWBAR_AGENT_API_KEY': 'sk-test'})
        self.assertIn('CROWBAR_AGENT_PROVIDER', str(stopped.exception))

    def test_standard_provider_env_vars_are_detected(self):
        self.assertEqual(
            desktop_check.real_model_credential({'GEMINI_API_KEY': 'gem-key'}),
            {'provider': 'google', 'api_key': 'gem-key', 'model': None},
        )
        self.assertEqual(
            desktop_check.real_model_credential({'OPENAI_API_KEY': 'oa-key', 'CROWBAR_AGENT_MODEL': 'gpt-4o-mini'}),
            {'provider': 'openai', 'api_key': 'oa-key', 'model': 'gpt-4o-mini'},
        )

    def test_qmp_keys_for_text_maps_spaces_punctuation_and_case(self):
        self.assertEqual(
            desktop_check.qmp_keys_for_text('Hi app_id?'),
            [('shift', 'h'), ('i',), ('spc',), ('a',), ('p',), ('p',),
             ('shift', 'minus'), ('i',), ('d',), ('shift', 'slash')],
        )
        with self.assertRaises(ValueError):
            desktop_check.qmp_keys_for_text('bad\n')

    def test_turn_state_kind_reads_tagged_shell_turn_state(self):
        self.assertEqual(desktop_check.turn_state_kind({'state': {'kind': 'working'}}), 'working')
        self.assertEqual(desktop_check.turn_state_kind({'state': {'kind': 'done'}}), 'done')
        self.assertEqual(
            desktop_check.turn_state_kind({'state': {'kind': 'failed', 'message': 'err'}}),
            'failed',
        )
        self.assertIsNone(desktop_check.turn_state_kind({}))

    def test_evidence_has_secret_detects_leaks_and_ignores_binary_artifacts(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        work = Path(temporary.name)
        (work / 'desktop.log').write_text('clean log\n')
        (work / 'shot.png').write_bytes(b'PNG secret-xyz')
        self.assertEqual(desktop_check.evidence_has_secret(work, 'secret-xyz'), [])
        (work / 'harness.log').write_text('oops secret-xyz leaked\n')
        self.assertEqual(desktop_check.evidence_has_secret(work, 'secret-xyz'), ['harness.log'])

    def test_embedded_guest_scripts_compile_cleanly(self):
        compile(desktop_check.HARNESS_LOGIN_SCRIPT, '<HARNESS_LOGIN_SCRIPT>', 'exec')
        compile(desktop_check.HARNESS_AUDIT_SCRIPT, '<HARNESS_AUDIT_SCRIPT>', 'exec')

    def test_harness_audit_script_verifies_exact_turn_context_and_rejects_decoy(self):
        import sqlite3, sys
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        work = Path(temporary.name)
        db = work / 'harness.sqlite'
        log = work / 'harness.log'
        log.write_text('started\n')

        prompt = 'what is the exact app_id of my active window?'
        full_user_content = (
            '[Desktop context]\nWorkspace: 1\nActive window: ~ (app_id: foot)\n'
            'Windows by workspace:\n- Workspace 1: ~ (app_id: foot) [active]\n'
            'Displays:\n- Virtual-1: 1280x960 at (0, 0), scale 1x\n\n' + prompt
        )
        conn = sqlite3.connect(db)
        conn.execute('CREATE TABLE documents (id INTEGER PRIMARY KEY, kind TEXT NOT NULL)')
        conn.execute('CREATE TABLE document_revisions (document_id INTEGER NOT NULL, seq INTEGER NOT NULL, kind TEXT NOT NULL, content TEXT NOT NULL)')
        conn.execute('CREATE TABLE entries (id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL, record TEXT NOT NULL)')
        conn.execute('INSERT INTO documents (id, kind) VALUES (2, ?)', ('"crowbar.conversations"',))
        # Decoy conversation (at=3) in base revision; target conversation (at=9) in delta revision.
        conn.execute(
            'INSERT INTO document_revisions (document_id, seq, kind, content) VALUES (2, 1, ?, ?)',
            ('base', json.dumps({'conversations': {'decoy111': {'at': 3, 'started': 1, 'title': '', 'turns': 1}}})),
        )
        conn.execute(
            'INSERT INTO document_revisions (document_id, seq, kind, content) VALUES (2, 2, ?, ?)',
            ('delta', json.dumps([['s', ['conversations', 'real2222'], {'at': 9, 'started': 2, 'title': '', 'turns': 1}]])),
        )
        def msg_entry(turn, content):
            return json.dumps({
                'kind': 'crowbar.message',
                'data': {'turn': turn, 'text': prompt},
                'model': [{'role': 'user', 'content': content, 'timestamp': 1}],
            })

        def assistant_entry(content):
            return json.dumps({
                'kind': 'pi.assistant',
                'model': [{'role': 'assistant', 'content': [{'type': 'text', 'text': content}], 'timestamp': 2}],
            })

        # Cross-conversation decoy (at=3, turn 1 with full desktop context),
        # same-conversation plain user message (at=9, turn 1, NO context),
        # same-conversation assistant reply echoing the prompt + context markers,
        # and same-conversation wrong-turn user message (turn 2 with context):
        # ALL combined MUST still fail when auditing conversation 'real2222' turn 1.
        conn.execute(
            'INSERT INTO entries (id, conversation_id, record) VALUES (1, 3, ?)',
            (msg_entry(1, full_user_content),),
        )
        conn.execute(
            'INSERT INTO entries (id, conversation_id, record) VALUES (2, 9, ?)',
            (msg_entry(1, prompt),),
        )
        conn.execute(
            'INSERT INTO entries (id, conversation_id, record) VALUES (3, 9, ?)',
            (assistant_entry('You asked: ' + full_user_content),),
        )
        conn.execute(
            'INSERT INTO entries (id, conversation_id, record) VALUES (4, 9, ?)',
            (msg_entry(2, full_user_content),),
        )
        conn.commit()
        conn.close()

        spec = {
            'db': str(db),
            'conversation': 'real2222',
            'turn': 1,
            'prompt': prompt,
            'sentinel': 'crowbarsecretwindowabcdef',
            'secret': 'sk-test-secret-123',
            'logs': [str(log)],
        }
        res = subprocess.run(
            [sys.executable, '-c', desktop_check.HARNESS_AUDIT_SCRIPT],
            input=json.dumps(spec), text=True, capture_output=True, check=True,
        )
        audit = json.loads(res.stdout)
        self.assertTrue(audit['has_conversation'])
        self.assertFalse(audit['has_desktop_context'])
        self.assertFalse(audit['has_active_foot'])
        self.assertFalse(audit['has_display'])

        # Now replace turn 1 on conversation_id=9 with the real user-role Model input carrying desktop context.
        conn = sqlite3.connect(db)
        conn.execute(
            'UPDATE entries SET record = ? WHERE id = 2',
            (msg_entry(1, full_user_content),),
        )
        conn.commit()
        conn.close()

        res2 = subprocess.run(
            [sys.executable, '-c', desktop_check.HARNESS_AUDIT_SCRIPT],
            input=json.dumps(spec), text=True, capture_output=True, check=True,
        )
        audit2 = json.loads(res2.stdout)
        self.assertTrue(audit2['has_conversation'])
        self.assertTrue(audit2['has_desktop_context'])
        self.assertTrue(audit2['has_active_foot'])
        self.assertTrue(audit2['has_display'])
        self.assertFalse(audit2['has_sentinel'])
        self.assertFalse(audit2['has_secret_in_db'])
        self.assertFalse(audit2['has_secret_in_logs'])

    def test_check_scrubs_secret_from_logs_results_and_diagnostics(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        secret = 'sk-never-write-this'
        vm = FakeGuest(temporary.name, lambda user, cmd: (0, f'diag saw {secret}\n', ''))
        check = desktop_check.Check(vm, secret=secret)
        check.qmp = type('Q', (), {'screendump': lambda *a, **kw: None})()
        self.addCleanup(check.logfile.close)
        check.log(f'echo {secret}')
        check.record('checked', True, f'detail {secret}')
        check.diagnostics()
        self.assertEqual(desktop_check.evidence_has_secret(vm.work, secret), [])
        self.assertNotIn(secret, (vm.work / 'desktop.log').read_text())
        self.assertNotIn(secret, (vm.work / 'harness.log').read_text())
        self.assertEqual(check.results[0]['detail'], 'detail [REDACTED]')

    def test_main_scrubs_secret_from_uncaught_exception_in_results(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        secret = "sk-exception-'secret'\\999"
        vm = Runs.Guest(temporary.name)

        def boom(check, built, credential=None):
            raise RuntimeError(f'boom with {secret}')

        with patch.dict(desktop_check.os.environ, {'ANTHROPIC_API_KEY': secret}, clear=True), \
                patch.object(base, 'preflight'), patch.object(guest.Guest, 'create', return_value=vm), \
                patch.object(guest, 'build_base', return_value={}), \
                patch.object(desktop_check.Check, 'checks', boom), \
                patch('sys.argv', ['desktop_check.py']):
            code = desktop_check.main()
        self.assertEqual(code, 1)
        self.assertEqual(desktop_check.evidence_has_secret(vm.work, secret), [])
        results_text = (vm.work / 'results.json').read_text()
        self.assertNotIn(secret, results_text)
        self.assertNotIn(repr(secret)[1:-1], results_text)
        self.assertIn('[REDACTED]', results_text)

if __name__ == '__main__':
    unittest.main()
