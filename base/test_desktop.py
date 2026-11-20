"""The desktop's helpers in the Base image: the launcher service, the unlock helper,
and the service that reboots for Ctrl+Alt+Del."""
import importlib.util
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

DESKTOP = Path(__file__).resolve().parent / 'desktop'


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, DESKTOP / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


launcher = load('launcher', 'launcher.py')
unlocker = load('unlock', 'unlock.py')
ctrl_alt_del = load('ctrl_alt_del', 'ctrl-alt-del.py')
PERSON = types.SimpleNamespace(pw_name='alex', pw_dir='/home/alex', pw_uid=1003, pw_shell='/bin/bash')
SHELL_UID = 1001


class LauncherRequests(unittest.TestCase):
    def test_only_the_shell_may_make_a_request(self):
        with patch.object(launcher, 'dispatch') as dispatch:
            for peer in (0, 1003, 1000, 1002):
                reply = launcher.answer(b'{"op":"appearance"}\n', peer, SHELL_UID, PERSON)
                self.assertFalse(reply['ok'], peer)
            dispatch.assert_not_called()

    def test_the_shell_reads_the_persons_appearance(self):
        with patch.object(launcher, 'appearance', return_value={'theme': 'light'}) as read:
            reply = launcher.answer(b'{"op":"appearance"}\n', SHELL_UID, SHELL_UID, PERSON)
        self.assertEqual(reply, {'ok': True, 'preferences': {'theme': 'light'}})
        read.assert_called_once_with('/home/alex', 1003)

    def test_a_launch_runs_as_the_launcher_itself_and_may_gain_privileges(self):
        with patch.object(launcher, 'desktop_path', side_effect=lambda path: path), \
                patch.object(launcher.subprocess, 'run') as run, patch('builtins.open'):
            run.return_value.returncode = 0
            reply = launcher.answer(b'{"op":"launch","desktop":"/usr/share/applications/foot.desktop"}\n',
                                    SHELL_UID, SHELL_UID, PERSON)
        self.assertEqual(reply, {'ok': True})
        command, options = run.call_args
        # As on any desktop: sudo and su work from the terminal.
        self.assertEqual(command[0], ['/usr/bin/gio', 'launch', '/usr/share/applications/foot.desktop'])
        self.assertFalse({'user', 'group', 'extra_groups'} & set(options))
        self.assertEqual(options['env']['WAYLAND_DISPLAY'], 'wayland-0')

    def test_a_malformed_or_unknown_request_is_refused(self):
        for line in (b'{"op":"appearance"}', b'[]\n', b'{"op":"run","command":"sh"}\n',
                     b'{"op":"launch","desktop":"/usr/share/applications/foot.desktop","uid":0}\n',
                     b'x' * 5000 + b'\n'):
            self.assertFalse(launcher.answer(line, SHELL_UID, SHELL_UID, PERSON)['ok'], line[:40])



class CtrlAltDel(unittest.TestCase):
    """The service reboots when the compositor says Ctrl+Alt+Del, and for no one else."""

    def said(self, data, peer):
        import socket
        ours, theirs = socket.socketpair()
        with ours, theirs:
            theirs.sendall(data)
            theirs.shutdown(socket.SHUT_WR)
            with patch.object(ctrl_alt_del, 'peer_uid', return_value=peer):
                return ctrl_alt_del.heard(ours, 1000)

    def test_the_compositor_saying_the_chord_is_heard(self):
        self.assertTrue(self.said(b'ctrl-alt-del\n', 1000))

    def test_anyone_else_or_anything_else_is_not(self):
        self.assertFalse(self.said(b'ctrl-alt-del\n', 1003))
        self.assertFalse(self.said(b'ctrl-alt-del\n', 1001))
        self.assertFalse(self.said(b'reboot\n', 1000))
        self.assertFalse(self.said(b'ctrl-alt-', 1000))
        self.assertFalse(self.said(b'', 1000))


class UnlockHelper(unittest.TestCase):
    """The unlock helper tells the compositor to unlock on the person's
    password alone."""

    def setUp(self):
        self.told = []
        self.stored = unlocker.libcrypt.crypt(b'crowbar', unlocker.libcrypt.crypt_gensalt(b'$6$', 0, None, 0))
        self.person = 'jim'
        patches = [
            patch.object(unlocker, 'person_name', lambda: self.person),
            patch.object(unlocker, 'shadow_hash', lambda account: self.stored if account == 'jim' else None),
            patch.object(unlocker, 'tell_compositor', lambda op: self.told.append(op)),
            patch.object(unlocker, 'WRONG_PAUSE', 0),
        ]
        for p in patches:
            p.start()
            self.addCleanup(p.stop)

    def test_the_password_unlocks_and_nothing_else_does(self):
        self.assertEqual(unlocker.unlock({'password': 'wrong'}), {'ok': False, 'error': 'Wrong password'})
        self.assertEqual(unlocker.unlock({'password': ''}), {'ok': False, 'error': 'Wrong password'})
        self.assertEqual(self.told, [])
        self.assertEqual(unlocker.unlock({'password': 'crowbar'}), {'ok': True})
        self.assertEqual(self.told, ['unlock'])

    def test_a_locked_or_empty_account_matches_nothing(self):
        for stored in (b'!' + self.stored, b'*', b'', None):
            self.stored = stored
            self.assertFalse(unlocker.password_matches('crowbar'), stored)
            self.assertFalse(unlocker.password_matches(''), stored)

    def test_an_account_from_the_account_file_does_not_take_the_lab_password(self):
        self.stored = unlocker.libcrypt.crypt(b'their own', unlocker.libcrypt.crypt_gensalt(b'$6$', 0, None, 0))
        self.assertEqual(unlocker.unlock({'password': 'crowbar'}), {'ok': False, 'error': 'Wrong password'})
        self.assertEqual(unlocker.unlock({'password': 'their own'}), {'ok': True})

    def test_with_no_person_account_nothing_matches(self):
        self.person = None
        self.assertFalse(unlocker.password_matches('crowbar'))

    def test_a_request_is_the_password_and_nothing_else(self):
        for request in ({}, {'password': 1}, {'password': 'crowbar', 'account': 'root'}):
            with self.assertRaises(ValueError):
                unlocker.unlock(request)
        self.assertEqual(unlocker.unlock({'password': 'x' * (unlocker.MAX_PASSWORD + 1)})['ok'], False)
        self.assertEqual(self.told, [])


class PreferencesSafety(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = Path(self.temp.name)
        self.folder = self.home / '.config/crowbaros'
        self.folder.mkdir(parents=True)

    def tearDown(self):
        self.temp.cleanup()

    def test_absent_and_regular_preferences(self):
        self.assertIsNone(launcher.appearance(self.home, os.getuid()))
        (self.folder / 'appearance.json').write_text('{"theme":"light"}')
        self.assertEqual(launcher.appearance(self.home, os.getuid()), {'theme': 'light'})

    def test_file_symlink_cannot_make_root_read_another_file(self):
        secret = self.home / 'unrelated.json'
        secret.write_text('{"secret":"must not escape"}')
        (self.folder / 'appearance.json').symlink_to(secret)
        with self.assertRaises(OSError):
            launcher.appearance(self.home, os.getuid())

    def test_directory_symlink_cannot_escape_the_preferences_directory(self):
        self.folder.rmdir()
        outside = self.home / 'outside'
        outside.mkdir()
        (outside / 'appearance.json').write_text('{}')
        self.folder.symlink_to(outside)
        with self.assertRaises(OSError):
            launcher.appearance(self.home, os.getuid())

    def test_fifo_and_oversized_file_are_refused_without_blocking(self):
        file = self.folder / 'appearance.json'
        os.mkfifo(file)
        with self.assertRaises(ValueError):
            launcher.appearance(self.home, os.getuid())
        file.unlink()
        file.write_bytes(b' ' * 65537)
        with self.assertRaises(ValueError):
            launcher.appearance(self.home, os.getuid())

    def test_any_desktop_entry_launches_by_its_full_path(self):
        entry = self.home / '.local/share/applications/mine.desktop'
        entry.parent.mkdir(parents=True)
        entry.write_text('[Desktop Entry]\n')
        self.assertEqual(launcher.desktop_path(str(entry)), str(entry))
        for path in ('mine.desktop', str(self.home / 'script.sh')):
            with self.assertRaises(ValueError):
                launcher.desktop_path(path)
        with self.assertRaises(ValueError):
            launcher.desktop_path(str(entry.parent))
        entry.parent.rename(self.home / 'x.desktop')
        with self.assertRaises(ValueError):
            launcher.desktop_path(str(self.home / 'x.desktop'))

    def test_app_environment_sets_cursor_theme_and_size(self):
        env = launcher.app_environment(PERSON)
        self.assertEqual(env['XCURSOR_THEME'], 'Vanilla-DMZ')
        self.assertEqual(env['XCURSOR_SIZE'], '24')

    def test_an_app_is_the_person_at_the_ordinary_wayland_socket(self):
        env = launcher.app_environment(PERSON)
        self.assertEqual((env['HOME'], env['USER'], env['LOGNAME'], env['SHELL']),
                         ('/home/alex', 'alex', 'alex', '/bin/bash'))
        self.assertEqual((env['XDG_RUNTIME_DIR'], env['WAYLAND_DISPLAY']), ('/run/user/1003', 'wayland-0'))
        self.assertTrue(env['PATH'].startswith('/home/alex/.local/bin:'))

    def test_an_app_gets_the_systems_locale(self):
        with tempfile.TemporaryDirectory() as folder:
            conf = Path(folder) / 'locale.conf'
            conf.write_text('# comment\nLANG="en_GB.UTF-8"\n')
            self.assertEqual(launcher.system_locale(conf), 'en_GB.UTF-8')
            self.assertEqual(launcher.system_locale(Path(folder) / 'missing'), 'C.UTF-8')

    def test_preferences_cannot_read_a_different_owners_file_or_hard_link(self):
        file = self.folder / 'appearance.json'
        file.write_text('{}')
        with self.assertRaises(ValueError):
            launcher.appearance(self.home, os.getuid() + 1)
        os.link(file, self.home / 'second-link.json')
        with self.assertRaises(ValueError):
            launcher.appearance(self.home, os.getuid())


if __name__ == '__main__':
    unittest.main()
