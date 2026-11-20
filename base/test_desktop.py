"""The desktop's helpers in the Base image: the launcher service and the unlock helper."""
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

    def test_desktop_path_cannot_select_arbitrary_code(self):
        for path in ('/tmp/evil.desktop', '/usr/share/applications/../evil.desktop',
                     '/usr/share/applications/foot.desktop/../../evil.desktop'):
            with self.assertRaises(ValueError):
                launcher.desktop_path(path)

    def test_app_environment_sets_cursor_theme_and_size(self):
        env = launcher.app_environment(types.SimpleNamespace(pw_dir='/home/crowbar-app', pw_uid=1002))
        self.assertEqual(env['XCURSOR_THEME'], 'Vanilla-DMZ')
        self.assertEqual(env['XCURSOR_SIZE'], '24')

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
