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


def newc(names, compressed=b'the compressed main archive'):
    """An initramfs as dracut makes one: an uncompressed cpio archive of these
    files, the early microcode, then the rest."""
    out = b''
    for name in [*names, 'TRAILER!!!']:
        data = b'' if name == 'TRAILER!!!' else b'microcode for ' + name.encode()
        encoded = name.encode() + b'\0'
        fields = [0, 0o100644, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(encoded), 0]
        out += b'070701' + b''.join(b'%08X' % f for f in fields) + encoded
        out += b'\0' * (-len(out) % 4) + data
        out += b'\0' * (-len(out) % 4)
    return out + compressed


BOTH = list(void_base.MICROCODE)


class RootfsTrust(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        work = Path(temporary.name)
        self.installs = 0
        self.kernel = False
        self.nvidia = True
        self.microcode = BOTH

        def install(*args, log, env):
            # What rootfs.sh leaves: a root, with or without a kernel, and with
            # or without NVIDIA's modules, which DKMS builds and depmod lists.
            # xbps succeeds either way: a failed DKMS build does not fail it.
            self.installs += 1
            root = Path(env['ROOTFS'])
            (root / 'boot').mkdir(parents=True, exist_ok=True)
            if self.kernel:
                (root / 'boot/vmlinuz-6.18.64_1').write_bytes(b'kernel')
                (root / 'boot/initramfs-6.18.64_1.img').write_bytes(newc(self.microcode))
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

    def test_a_root_whose_initramfs_lacks_a_vendors_microcode_is_not_trusted(self):
        self.kernel, self.microcode = True, BOTH[1:]
        with self.assertRaises(SystemExit) as stopped:
            void_base.rootfs(force=False)
        self.assertIn('GenuineIntel', str(stopped.exception))
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


class BuildFixture(unittest.TestCase):
    """A build with every outside step recorded instead of run."""

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

    def root(self, kernels, nvidia):
        """A root holding these kernels, with NVIDIA's modules built for those in `nvidia`."""
        for kernel in kernels:
            (void_base.ROOTFS / 'boot').mkdir(parents=True, exist_ok=True)
            (void_base.ROOTFS / f'boot/vmlinuz-{kernel}').write_bytes(b'kernel')
            (void_base.ROOTFS / f'boot/initramfs-{kernel}.img').write_bytes(newc(BOTH))
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



class Desktop(BuildFixture):
    """What Base's build hands Cargo's build and the image step."""

    def test_a_fresh_cargo_home_has_both_caches_the_build_binds_in(self):
        void_base.compile_desktop(force=False)
        self.assertTrue((self.cargo_home / 'registry').is_dir())
        self.assertTrue((self.cargo_home / 'git').is_dir())

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



class Account(BuildFixture):
    """The person's account the image gets: from the account file, or the lab's."""

    HASH = '$6$saltsalt$' + 'h' * 86

    def setUp(self):
        super().setUp()
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        (void_base.ROOTFS / 'etc').mkdir(parents=True, exist_ok=True)
        (void_base.ROOTFS / 'etc/passwd').write_text('root:x:0:0::/root:/bin/sh\nsshd:x:22:22::/var/empty:/bin/false\n')
        (void_base.ROOTFS / 'etc/group').write_text('root:x:0:\n_seatd:x:996:\n')

    def write_account(self, **account):
        void_base.ACCOUNT_FILE.write_text(json.dumps(account))

    def build(self, **kw):
        with patch('sys.stderr', new_callable=io.StringIO) as stderr:
            void_base.build(**kw)
        return self.image_step()[1], stderr.getvalue()

    def test_an_account_file_makes_that_account_and_not_the_lab_one(self):
        self.write_account(name='jim', password_hash=self.HASH, full_name='Jim Martin')
        env, stderr = self.build()
        self.assertEqual((env['PERSON'], env['PERSON_PASSWORD_HASH'], env['PERSON_FULL_NAME']),
                         ('jim', self.HASH, 'Jim Martin'))
        self.assertEqual(env['PERSON_PASSWORD'], '')
        self.assertNotIn('WARNING', stderr)

    def test_with_no_account_file_the_lab_account_comes_with_a_warning(self):
        env, stderr = self.build()
        self.assertEqual((env['PERSON'], env['PERSON_PASSWORD'], env['PERSON_PASSWORD_HASH']),
                         ('crowbar', 'crowbar', ''))
        self.assertIn('WARNING', stderr)
        self.assertIn('published', stderr)

    def test_a_check_overwriting_the_image_with_the_lab_account_still_warns(self):
        # The checks write the same image file a person builds with their own
        # account, so the lab account never replaces it silently.
        self.write_account(name='jim', password_hash=self.HASH)
        env, stderr = self.build(lab_account=True)
        self.assertEqual(env['PERSON'], 'crowbar')
        self.assertIn('WARNING', stderr)

    def test_the_account_has_the_uid_the_unlock_helper_looks_for(self):
        env, _ = self.build()
        self.assertEqual(env['PERSON_UID'], str(void_base.PERSON_UID))
        unlock = (void_base.HERE / 'desktop/unlock.py').read_text()
        self.assertIn(f'PERSON_UID = {void_base.PERSON_UID}\n', unlock)

    def test_an_account_file_that_would_break_the_image_is_refused(self):
        for account in [dict(name='jim'), dict(name='jim', password_hash='plain text'),
                        dict(name='jim', password_hash='$6$...'),
                        dict(name='jim', password_hash='$6$saltsalt'),
                        dict(name='jim', password_hash=self.HASH + ':0:'),
                        dict(name='crowbar-shell', password_hash=self.HASH),
                        dict(name='Jim Martin', password_hash=self.HASH),
                        dict(name='jim', password_hash=self.HASH, full_name='Jim, Martin'),
                        dict(name='jim', password_hash=self.HASH, shell='/bin/zsh')]:
            self.write_account(**account)
            with self.assertRaises(SystemExit, msg=account):
                void_base.build()
        self.assertEqual(self.commands, [])

    def test_a_name_the_image_already_has_is_refused_before_anything_is_built(self):
        for name in ('root', 'sshd', '_seatd', 'netdev'):
            self.write_account(name=name, password_hash=self.HASH)
            with self.assertRaises(SystemExit, msg=name) as stopped:
                void_base.build()
            self.assertIn(name, str(stopped.exception))
        self.assertEqual(self.commands, [])

    def test_the_hashes_people_make_are_taken(self):
        for hashed in ('$6$rounds=5000$saltsalt$' + 'a/b.' * 21 + 'xy',
                       '$y$j9T$saltsaltsaltsalt$' + 'A' * 43):
            self.assertTrue(void_base.PASSWORD_HASH.fullmatch(hashed), hashed)


class EarlyCpio(unittest.TestCase):
    """The build reads the microcode's names from the initramfs's first archive."""

    def test_names_in_the_early_archive(self):
        names = ['early_cpio', 'kernel/x86/microcode/GenuineIntel.bin']
        self.assertEqual(void_base.early_cpio(newc(names)), names)

    def test_an_initramfs_with_no_early_archive_names_nothing(self):
        self.assertEqual(void_base.early_cpio(b'\x28\xb5\x2f\xfd zstd'), [])


class RootFillsDisk(unittest.TestCase):
    """Base's check sees the root grown to fill a disk larger than the image."""

    IMAGE = (1 + 64 + 8 * 1024 + 1) * 2048  # sectors, from config.json
    START = (1 + 64) * 2048

    def output(self, disk, size, filesystem):
        return f'{disk}\n{self.START}\n{size}\n{filesystem}\n'

    def test_a_grown_root_passes(self):
        disk = 12 * 1024 * 2048
        size = disk - self.START - 2048
        self.assertTrue(void_base.root_fills_disk(self.output(disk, size, size * 512)))

    def test_btrfs_rounding_down_to_a_whole_block_passes(self):
        # As in the VM: the partition ends on an odd sector, and Btrfs stops
        # at the last whole 4 KiB block in it.
        self.assertTrue(void_base.root_fills_disk('25165824\n133120\n25032671\n12816723968\n'))

    def test_a_root_left_at_the_images_size_fails(self):
        disk, size = 12 * 1024 * 2048, 8 * 1024 * 2048
        self.assertFalse(void_base.root_fills_disk(self.output(disk, size, size * 512)))

    def test_a_partition_grown_without_its_filesystem_fails(self):
        disk = 12 * 1024 * 2048
        size = disk - self.START - 2048
        self.assertFalse(void_base.root_fills_disk(self.output(disk, size, 8 << 30)))

    def test_a_disk_no_larger_than_the_image_shows_nothing(self):
        size = self.IMAGE - self.START - 2048
        self.assertFalse(void_base.root_fills_disk(self.output(self.IMAGE, size, size * 512)))


class PersonCheck(unittest.TestCase):
    """Base's check finds the person's account with a home of its own and a login shell."""

    def setUp(self):
        self.passes = void_base.person_has_home

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
