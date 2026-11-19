"""Base's build: which roots it trusts, what it hands the build and the image,
and how its check reads SSH."""
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import void_base


class RootfsTrust(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        work = Path(temporary.name)
        self.installs = 0
        self.kernel = False

        def install(*args, log, env):
            # What rootfs.sh leaves: a root, with or without a kernel.
            self.installs += 1
            boot = Path(env['ROOTFS']) / 'boot'
            boot.mkdir(parents=True, exist_ok=True)
            if self.kernel:
                (boot / 'vmlinuz-6.18.64_1').write_bytes(b'kernel')
            return 0

        for name, value in [('WORK', work), ('ROOTFS', work / 'rootfs'),
                            ('fetch', lambda: work / 'xbps'), ('run_shown', install)]:
            p = patch.object(void_base, name, value)
            p.start()
            self.addCleanup(p.stop)

    def test_a_root_installed_without_a_kernel_is_installed_again(self):
        with self.assertRaises(SystemExit):
            void_base.rootfs(force=False)
        self.kernel = True
        void_base.rootfs(force=False)
        self.assertEqual(self.installs, 2)

    def test_a_finished_root_is_used_as_it_is(self):
        self.kernel = True
        void_base.rootfs(force=False)
        void_base.rootfs(force=False)
        self.assertEqual(self.installs, 1)

    def test_a_build_root_installed_without_cargo_is_not_trusted(self):
        build_root = void_base.WORK / 'buildroot'
        with patch.object(void_base, 'BUILD_ROOT', build_root), \
                patch.object(void_base, 'run', lambda *args, **kw: None):
            with self.assertRaises(SystemExit):
                void_base.compile_desktop(force=False)
        self.assertFalse(build_root.with_name('buildroot.done').exists())


class Desktop(unittest.TestCase):
    """What Base's build hands Cargo's build and the image step."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name) / 'a checkout with spaces/.lab/base'
        self.commands = []
        for name, value in [('WORK', self.work), ('TARGET', self.work / 'target'),
                            ('BUILD_ROOT', self.work / 'buildroot'), ('KEY', self.work / 'id_ed25519'),
                            ('run', lambda *args, **kw: self.commands.append([str(a) for a in args])),
                            ('install', lambda *args, **kw: None), ('sha256', lambda path: 'digest'),
                            ('preflight', lambda **kw: None), ('rootfs', lambda force: None)]:
            p = patch.object(void_base, name, value)
            p.start()
            self.addCleanup(p.stop)
        self.cargo_home = Path(temporary.name) / 'fresh cargo home'
        p = patch.dict(os.environ, CARGO_HOME=str(self.cargo_home))
        p.start()
        self.addCleanup(p.stop)

    def test_a_fresh_cargo_home_has_both_caches_the_build_binds_in(self):
        void_base.compile_desktop(force=False)
        self.assertTrue((self.cargo_home / 'registry').is_dir())
        self.assertTrue((self.cargo_home / 'git').is_dir())

    def test_the_image_step_gets_each_program_whole(self):
        self.work.mkdir(parents=True)
        void_base.KEY.write_text('key')
        void_base.build()
        image = next(c for c in self.commands if any(a.endswith('host/image.sh') for a in c))
        programs = image[[a.endswith('host/image.sh') for a in image].index(True) + 1:]
        self.assertEqual(programs, [str(path) for path in void_base.binaries().values()])
        self.assertTrue(all(' ' in path for path in programs))


class SshCheck(unittest.TestCase):
    """Base's check reads sshd's settings as sshd -T prints them."""

    def setUp(self):
        self.passes = dict((name, ok) for name, _, ok in void_base.CHECKS)['SSH takes keys and no passwords']

    def test_keys_only_passes_in_either_case(self):
        self.assertTrue(self.passes('PasswordAuthentication no\nKbdInteractiveAuthentication no\n'
                                    'PubkeyAuthentication yes\n'))
        self.assertTrue(self.passes('passwordauthentication no\nkbdinteractiveauthentication no\n'
                                    'pubkeyauthentication yes\n'))

    def test_passwords_fail(self):
        self.assertFalse(self.passes('PasswordAuthentication yes\nKbdInteractiveAuthentication no\n'
                                     'PubkeyAuthentication yes\n'))


if __name__ == '__main__':
    unittest.main()
