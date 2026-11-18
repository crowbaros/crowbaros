"""Safety regressions for isolated desktop runs and the temporary launcher."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import desktop_check

spec = importlib.util.spec_from_file_location('alpha_launcher', Path(__file__).parent / 'guest/alpha-launcher.py')
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)

spec = importlib.util.spec_from_file_location('alpha_unlock', Path(__file__).parent / 'guest/alpha-unlock.py')
unlocker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(unlocker)


class UnlockHelper(unittest.TestCase):
    """The unlock helper tells the compositor to unlock on the person's
    password alone."""

    def setUp(self):
        self.told = []
        self.stored = unlocker.libcrypt.crypt(b'crowbar', unlocker.libcrypt.crypt_gensalt(b'$6$', 0, None, 0))
        patches = [
            patch.object(unlocker, 'shadow_hash', lambda account: self.stored),
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

    def test_preferences_cannot_read_a_different_owners_file_or_hard_link(self):
        file = self.folder / 'appearance.json'
        file.write_text('{}')
        with self.assertRaises(ValueError):
            launcher.appearance(self.home, os.getuid() + 1)
        os.link(file, self.home / 'second-link.json')
        with self.assertRaises(ValueError):
            launcher.appearance(self.home, os.getuid())


class IsolatedRuns(unittest.TestCase):
    def test_each_run_owns_new_state_and_never_selects_the_development_disk(self):
        guest = desktop_check.guest
        names = ['ROOT', 'WORK', 'CFG', 'GUEST_ROOT', 'BUILD_ROOT', 'TARGET', 'DISK',
                 'KEY', 'PIDFILE', 'QMP_SOCK', 'SERIAL_LOG', 'VNC']
        saved = {name: getattr(guest, name) for name in names}
        with tempfile.TemporaryDirectory() as temporary:
            try:
                guest.ROOT = Path(temporary)
                development = guest.ROOT / '.lab/void-graphical'
                development.mkdir(parents=True)
                disk = development / 'disk.raw'
                disk.write_bytes(b'preserved VM')
                with patch.object(desktop_check.subprocess, 'check_output', return_value=b'fixture'):
                    # rev-parse requests text output, while status requests bytes.
                    desktop_check.subprocess.check_output.side_effect = lambda *a, **kw: 'fixture' if kw.get('text') else b''
                    desktop_check.configure()
                    first = guest.WORK
                    desktop_check.configure()
                self.assertNotEqual(first, guest.WORK)
                self.assertNotEqual(disk, guest.DISK)
                self.assertEqual(guest.DISK.parent, guest.WORK)
                self.assertEqual(disk.read_bytes(), b'preserved VM')
                self.assertIsNone(guest.CFG['vnc_port'])
                self.assertGreater(json.loads((guest.WORK / 'run.json').read_text())['ssh_port'], 0)
            finally:
                for name, value in saved.items():
                    setattr(guest, name, value)


if __name__ == '__main__':
    unittest.main()
