"""Base's build: which roots it trusts, what it hands the build and the image,
and how its check reads SSH."""
import io
import json
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
        self.nvidia = True

        def install(*args, log, env):
            # What rootfs.sh leaves: a root, with or without a kernel, and with
            # or without NVIDIA's modules, which DKMS builds and depmod lists.
            # xbps succeeds either way: a failed DKMS build does not fail it.
            self.installs += 1
            root = Path(env['ROOTFS'])
            (root / 'boot').mkdir(parents=True, exist_ok=True)
            if self.kernel:
                (root / 'boot/vmlinuz-6.18.64_1').write_bytes(b'kernel')
                modules = root / 'usr/lib/modules/6.18.64_1'
                modules.mkdir(parents=True, exist_ok=True)
                listed = ['kernel/drivers/net/virtio_net.ko.zst:']
                if self.nvidia:
                    listed += [f'kernel/drivers/video/{name}.ko:' for name in
                               ('nvidia', 'nvidia-modeset', 'nvidia-drm', 'nvidia-uvm')]
                (modules / 'modules.dep').write_text('\n'.join(listed) + '\n')
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

    def test_a_root_whose_nvidia_module_did_not_build_is_not_trusted(self):
        self.kernel, self.nvidia = True, False
        with self.assertRaises(SystemExit) as stopped:
            void_base.rootfs(force=False)
        self.assertIn('nvidia', str(stopped.exception))
        self.assertIn('6.18.64_1', str(stopped.exception))
        self.assertIn('/var/lib/dkms', str(stopped.exception))
        self.assertFalse((void_base.WORK / 'rootfs.done').exists())

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
        self.envs = []

        def run(*args, **kw):
            self.commands.append([str(a) for a in args])
            self.envs.append(kw.get('env', {}))

        for name, value in [('WORK', self.work), ('TARGET', self.work / 'target'),
                            ('BUILD_ROOT', self.work / 'buildroot'), ('KEY', self.work / 'id_ed25519'),
                            ('ROOTFS', self.work / 'rootfs'), ('run', run),
                            ('install', lambda *args, **kw: None), ('sha256', lambda path: 'digest'),
                            ('preflight', lambda **kw: None), ('rootfs', lambda force: None),
                            ('ACCOUNT_FILE', Path(temporary.name) / 'account.json')]:
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

    def root(self, kernels, nvidia):
        """A root holding these kernels, with NVIDIA's modules built for those in `nvidia`."""
        for kernel in kernels:
            (void_base.ROOTFS / 'boot').mkdir(parents=True, exist_ok=True)
            (void_base.ROOTFS / f'boot/vmlinuz-{kernel}').write_bytes(b'kernel')
            modules = void_base.ROOTFS / 'usr/lib/modules' / kernel
            modules.mkdir(parents=True)
            names = ('nvidia', 'nvidia-modeset', 'nvidia-drm', 'nvidia-uvm') if kernel in nvidia else ()
            (modules / 'modules.dep').write_text(
                ''.join(f'kernel/drivers/video/{name}.ko:\n' for name in names))
        self.work.mkdir(parents=True, exist_ok=True)
        void_base.KEY.write_text('key')

    def image_step(self):
        return next((c, e) for c, e in zip(self.commands, self.envs)
                    if any(a.endswith('host/image.sh') for a in c))

    def test_no_image_is_written_when_the_image_kernel_lacks_nvidias_module(self):
        # The newest kernel is the one the image boots, and DKMS built only for the older.
        self.root(['6.18.9_1', '6.18.10_1'], nvidia=['6.18.9_1'])
        with self.assertRaises(SystemExit) as stopped:
            void_base.build()
        self.assertIn('6.18.10_1', str(stopped.exception))
        self.assertFalse(any(any(a.endswith('host/image.sh') for a in c) for c in self.commands))

    def test_the_image_boots_the_newest_kernel_the_build_checked(self):
        self.root(['6.18.9_1', '6.18.10_1'], nvidia=['6.18.9_1', '6.18.10_1'])
        void_base.build()
        _, env = self.image_step()
        self.assertEqual(env['KVER'], '6.18.10_1')

    def test_the_kernel_command_line_carries_what_nvidias_driver_needs(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        _, env = self.image_step()
        self.assertIn('nvidia-drm.modeset=1', env['KERNEL_ARGS'].split())
        self.assertIn('nvidia-drm.fbdev=1', env['KERNEL_ARGS'].split())

    def test_the_image_step_gets_each_program_whole(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        image, _ = self.image_step()
        programs = image[[a.endswith('host/image.sh') for a in image].index(True) + 1:]
        self.assertEqual(programs, [str(path) for path in void_base.binaries().values()])
        self.assertTrue(all(' ' in path for path in programs))


    def write_account(self, **account):
        void_base.ACCOUNT_FILE.write_text(json.dumps(account))

    def test_an_account_file_makes_that_account_and_not_the_lab_one(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        self.write_account(name='jim', password_hash='$6$salt$hash', full_name='Jim Martin')
        void_base.build()
        _, env = self.image_step()
        self.assertEqual((env['PERSON'], env['PERSON_PASSWORD_HASH'], env['PERSON_FULL_NAME']),
                         ('jim', '$6$salt$hash', 'Jim Martin'))
        self.assertEqual(env['PERSON_PASSWORD'], '')

    def test_with_no_account_file_the_lab_account_comes_with_a_warning(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        with patch('sys.stderr', new_callable=io.StringIO) as stderr:
            void_base.build()
        _, env = self.image_step()
        self.assertEqual((env['PERSON'], env['PERSON_PASSWORD'], env['PERSON_PASSWORD_HASH']),
                         ('crowbar', 'crowbar', ''))
        self.assertIn('WARNING', stderr.getvalue())
        self.assertIn('published', stderr.getvalue())

    def test_the_checks_build_the_lab_account_whatever_the_account_file_says(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        self.write_account(name='jim', password_hash='$6$salt$hash')
        with patch('sys.stderr', new_callable=io.StringIO) as stderr:
            void_base.build(lab_account=True)
        _, env = self.image_step()
        self.assertEqual(env['PERSON'], 'crowbar')
        self.assertNotIn('WARNING', stderr.getvalue())

    def test_an_account_file_that_would_break_the_image_is_refused(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        for account in [dict(name='jim'), dict(name='jim', password_hash='plain text'),
                        dict(name='jim', password_hash='$6$salt$hash:0:'),
                        dict(name='root', password_hash='$6$salt$hash'),
                        dict(name='crowbar-shell', password_hash='$6$salt$hash'),
                        dict(name='Jim Martin', password_hash='$6$salt$hash'),
                        dict(name='jim', password_hash='$6$salt$hash', full_name='Jim, Martin'),
                        dict(name='jim', password_hash='$6$salt$hash', shell='/bin/zsh')]:
            self.write_account(**account)
            with self.assertRaises(SystemExit, msg=account):
                void_base.build()
        self.assertEqual(self.commands, [])


class PersonCheck(unittest.TestCase):
    """Base's check finds the person's account with a home of its own and a login shell."""

    def setUp(self):
        self.passes = dict((name, ok) for name, _, ok in void_base.CHECKS)[
            "the person's account has a home and a login shell"]

    def test_a_home_and_a_shell_pass(self):
        self.assertTrue(self.passes('crowbar:/home/crowbar:/bin/bash\ncrowbar 700\n'))

    def test_no_home_or_no_shell_fails(self):
        self.assertFalse(self.passes('crowbar:/home/crowbar:/bin/false\ncrowbar 700\n'))
        self.assertFalse(self.passes('crowbar:/home/crowbar:/bin/bash\n'))
        self.assertFalse(self.passes('crowbar:/home/crowbar:/bin/bash\nroot 755\n'))


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


class NvidiaCheck(unittest.TestCase):
    """Base's check sees NVIDIA's driver built into the image, and, in a VM
    with no NVIDIA card, left unloaded."""

    def setUp(self):
        checks = dict((name, ok) for name, _, ok in void_base.CHECKS)
        self.cmdline = checks["kernel command line carries NVIDIA's settings"]
        self.module = checks["NVIDIA's module is built, and not loaded without an NVIDIA card"]

    def test_the_command_line_needs_both_settings(self):
        line = 'BOOT_IMAGE=/boot/vmlinuz-6.18.64_1 root=LABEL=crowbaros-root rw console=ttyS0,115200'
        self.assertTrue(self.cmdline(line + ' nvidia-drm.modeset=1 nvidia-drm.fbdev=1\n'))
        self.assertFalse(self.cmdline(line + ' nvidia-drm.modeset=1\n'))
        self.assertFalse(self.cmdline(line + ' nvidia-drm.modeset=10 nvidia-drm.fbdev=1\n'))

    def test_a_built_module_that_is_not_loaded_passes(self):
        self.assertTrue(self.module('/lib/modules/6.18.64_1/kernel/drivers/video/nvidia.ko\n'
                                    'virtio_gpu\nvirtio_net\n'))

    def test_a_loaded_module_fails(self):
        self.assertFalse(self.module('/lib/modules/6.18.64_1/kernel/drivers/video/nvidia.ko\n'
                                     'nvidia_drm\nnvidia_modeset\nnvidia\nvirtio_net\n'))

    def test_a_module_that_is_not_there_fails(self):
        self.assertFalse(self.module('virtio_gpu\nvirtio_net\n'))


if __name__ == '__main__':
    unittest.main()
