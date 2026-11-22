"""Base's build: which roots it trusts, what it hands the build and the image,
and how its check reads SSH."""
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
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


class StaticXbps(unittest.TestCase):
    """The static xbps the builds run is the one the configuration pins."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.cache = Path(temporary.name) / 'cache'
        self.cache.mkdir()
        self.downloads = {}

        def run(*args, **kw):
            """curl, with what each address serves."""
            command = [str(a) for a in args]
            self.assertEqual(command[0], 'curl')
            Path(command[command.index('-o') + 1]).write_bytes(self.downloads.pop(command[-1]))

        for name, value in [('CACHE', self.cache), ('CFG', dict(void_base.CFG)), ('run', run)]:
            p = patch.object(void_base, name, value)
            p.start()
            self.addCleanup(p.stop)

    def archive(self, program):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode='w') as tar:
            info = tarfile.TarInfo('usr/bin/xbps-install.static')
            info.size = len(program)
            tar.addfile(info, io.BytesIO(program))
        return data.getvalue()

    def pin(self, name, program):
        """Pins an archive whose xbps-install is `program`, which its address
        serves once."""
        archive = self.archive(program)
        url = f'https://example.org/static/{name}'
        void_base.CFG['xbps_static_url'] = url
        void_base.CFG['xbps_static_sha256'] = hashlib.sha256(archive).hexdigest()
        self.downloads[url] = archive

    def program(self, xbps=None):
        return ((xbps or void_base.fetch()) / 'usr/bin/xbps-install.static').read_bytes()

    def test_a_new_pin_runs_the_new_xbps(self):
        # The unpacked copy used to be kept for as long as it was there, so a
        # machine or a CI cache that had the old xbps went on running it.
        self.pin('xbps-static-1.tar', b'the old xbps')
        self.assertEqual(self.program(), b'the old xbps')
        self.pin('xbps-static-2.tar', b'the new xbps')
        self.assertEqual(self.program(), b'the new xbps')

    def test_a_new_pin_at_the_same_address_runs_the_new_xbps(self):
        # The download was kept under the address's last part alone, so the
        # last pin's archive was found, and refused, at every build.
        self.pin('xbps-static.tar', b'the old xbps')
        self.assertEqual(self.program(), b'the old xbps')
        self.pin('xbps-static.tar', b'the new xbps')
        self.assertEqual(self.program(), b'the new xbps')

    def test_another_pin_leaves_the_xbps_a_build_is_using(self):
        # Checkouts share the cache, and each may pin its own.
        self.pin('xbps-static-1.tar', b'the old xbps')
        using = void_base.fetch()
        self.pin('xbps-static-2.tar', b'the new xbps')
        self.assertNotEqual(void_base.fetch(), using)
        self.assertEqual(self.program(using), b'the old xbps')

    def test_an_unpacking_that_stopped_partway_is_not_the_xbps(self):
        self.pin('xbps-static-1.tar', b'the xbps')
        with patch.object(void_base.tarfile.TarFile, 'extractall', side_effect=KeyboardInterrupt), \
                self.assertRaises(KeyboardInterrupt):
            void_base.fetch()
        self.assertEqual([path.name for path in self.cache.iterdir() if path.is_dir()], [])
        self.assertEqual(self.program(), b'the xbps')

    def test_a_build_downloads_to_a_file_of_its_own(self):
        # Every build downloaded to one file beside the archive, so two
        # builds of one pin wrote it together, and one moved it away from
        # under the other.
        self.pin('xbps-static-1.tar', b'the xbps')
        pin = void_base.CFG['xbps_static_sha256']
        others = [self.cache / f'{name}xbps-static-1.tar.part' for name in ('', f'{pin}-', f'{pin[:16]}-')]
        for other in others:
            other.write_bytes(b'another build is writing this')
        self.assertEqual(self.program(), b'the xbps')
        for other in others:
            self.assertEqual(other.read_bytes(), b'another build is writing this')

    def test_an_archive_in_the_cache_that_is_not_the_pinned_one_is_refused_and_removed(self):
        self.pin('xbps-static-1.tar', b'the xbps')
        pin = void_base.CFG['xbps_static_sha256']
        archive = self.cache / f'{pin}-xbps-static-1.tar'
        archive.write_bytes(b'not the archive')
        with self.assertRaises(SystemExit):
            void_base.fetch()
        self.assertFalse(archive.exists())
        self.assertEqual(self.program(), b'the xbps')

    def test_an_unpacked_copy_with_no_xbps_in_it_is_unpacked_again(self):
        # As a cache restored in part leaves it: the directory, and not what
        # was in it.
        self.pin('xbps-static-1.tar', b'the xbps')
        pin = void_base.CFG['xbps_static_sha256']
        (self.cache / f'xbps-static-{pin}/usr').mkdir(parents=True)
        self.assertEqual(self.program(), b'the xbps')

    def test_the_pinned_xbps_is_downloaded_and_unpacked_once(self):
        self.pin('xbps-static-1.tar', b'the xbps')
        self.program()
        with patch.object(void_base.tarfile, 'open', side_effect=AssertionError('unpacked again')):
            self.assertEqual(self.program(), b'the xbps')
        self.assertEqual(self.downloads, {})

    def test_a_download_that_is_not_the_pinned_archive_is_refused_and_not_kept(self):
        self.pin('xbps-static-1.tar', b'the xbps')
        url = void_base.CFG['xbps_static_url']
        self.downloads[url] = self.archive(b'another xbps')
        with self.assertRaises(SystemExit):
            void_base.fetch()
        self.assertEqual(list(self.cache.iterdir()), [])
        # The next build downloads it again, and takes the pinned one.
        self.downloads[url] = self.archive(b'the xbps')
        self.assertEqual(self.program(), b'the xbps')


class BuildFixture(unittest.TestCase):
    """A build with every outside step recorded instead of run."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name) / 'a checkout with spaces/.lab/base'
        self.commands = []
        self.envs = []

        # What the image step leaves where it was told to write, and whether
        # it then stops as a failed or interrupted step does.
        self.image_written = b'the new image'
        self.image_step_stops = False

        def run(*args, **kw):
            self.commands.append([str(a) for a in args])
            self.envs.append(kw.get('env', {}))
            if any(str(a).endswith('host/image.sh') for a in args):
                Path(kw['env']['OUT']).write_bytes(self.image_written)
                if self.image_step_stops:
                    raise SystemExit('the image step stopped partway')

        for name, value in [('WORK', self.work), ('TARGET', self.work / 'target'),
                            ('BASE_DISK', self.work / 'base.raw'),
                            ('BUILD_ROOT', self.work / 'buildroot'), ('KEY', self.work / 'id_ed25519'),
                            ('ROOTFS', self.work / 'rootfs'), ('run', run),
                            ('install', lambda *args, **kw: None), ('sha256', lambda path: 'digest'),
                            ('preflight', lambda **kw: None), ('rootfs', lambda force: None),
                            ('fetch', lambda: Path('/xbps-static')),
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

    def test_nvidias_module_keeps_video_memory_on_disk_across_suspend(self):
        # Without both, the GPU loses the compositor's video memory when the
        # machine suspends, and the desktop wakes grey (issue #272).
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        _, env = self.image_step()
        self.assertEqual(env['NVIDIA_OPTIONS'].split(), [
            'NVreg_PreserveVideoMemoryAllocations=1', 'NVreg_TemporaryFilePath=/var/tmp'])

    def test_the_image_step_writes_nvidias_options_beside_the_packages_own(self):
        # A file named nvidia.conf in /etc/modprobe.d would hide the package's
        # /usr/lib/modprobe.d/nvidia.conf, which blacklists nouveau.
        script = (Path(void_base.HERE) / 'host/image.sh').read_text()
        self.assertIn('options nvidia $NVIDIA_OPTIONS', script)
        self.assertIn('"$R/etc/modprobe.d/crowbaros-nvidia.conf"', script)

    def test_a_build_that_stops_partway_leaves_the_last_whole_image(self):
        # The image step writes the disk in several steps. Stopped between
        # them, it used to leave a part-written disk where `boot` and a
        # person's `dd` look for the image.
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.BASE_DISK.write_bytes(b'the last whole image')
        self.image_written = b'half of the new image'
        self.image_step_stops = True
        with self.assertRaises(SystemExit):
            void_base.build()
        self.assertEqual(void_base.BASE_DISK.read_bytes(), b'the last whole image')

    def test_a_first_build_that_stops_partway_leaves_no_image(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        self.image_step_stops = True
        with self.assertRaises(SystemExit):
            void_base.build()
        self.assertFalse(void_base.BASE_DISK.exists())

    def test_a_finished_build_puts_the_new_image_in_place_of_the_last(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.BASE_DISK.write_bytes(b'the last whole image')
        void_base.build()
        self.assertEqual(void_base.BASE_DISK.read_bytes(), b'the new image')
        self.assertEqual([path.name for path in self.work.glob('base.raw*')], ['base.raw'])

    def test_the_image_step_gets_each_program_whole(self):
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        image, _ = self.image_step()
        programs = image[[a.endswith('host/image.sh') for a in image].index(True) + 1:]
        self.assertEqual(programs, [str(path) for path in void_base.binaries().values()])
        self.assertTrue(all(' ' in path for path in programs))

    def test_the_desktops_helpers_are_one_program_built_with_the_desktop(self):
        self.assertIn('base/helpers', dict(void_base.PROJECTS))
        self.assertEqual(void_base.binaries()['crowbar-helpers'].name, 'crowbar-helpers')

    def test_python_is_only_in_an_image_built_for_the_lab_checks(self):
        # The lab's checks drive the guest with `python3 -c`; the person's
        # image has nothing that needs it.
        self.assertNotIn('python3', void_base.CFG['packages'])
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        _, person = self.image_step()
        self.assertEqual(person['LAB_PACKAGES'], '')
        self.commands.clear()
        self.envs.clear()
        void_base.build(lab_account=True)
        _, lab = self.image_step()
        self.assertEqual(lab['LAB_PACKAGES'].split(), ['python3'])
        self.assertEqual(lab['XBPS'], '/xbps-static/usr/bin')

    def test_the_image_step_has_a_mount_namespace_to_lend_the_lab_packages_dev(self):
        # Python's install byte-compiles its library, which needs /dev/shm.
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build(lab_account=True)
        image, _ = self.image_step()
        self.assertIn('--mount', image[:image.index('sh')])



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
        helpers = (void_base.HERE / 'helpers/src/main.rs').read_text()
        self.assertIn(f'pub const PERSON_UID: u32 = {void_base.PERSON_UID};\n', helpers)

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

    def test_the_account_files_ssh_keys_reach_the_image_one_per_line(self):
        keys = ['ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJ96oqWr9ruQ7W jim@tower',
                'ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY=',
                'sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29t key']
        self.write_account(name='jim', password_hash=self.HASH, ssh_keys=keys)
        env, _ = self.build()
        self.assertEqual(env['PERSON_SSH_KEYS'].splitlines(), keys)
        self.assertTrue(env['PERSON_SSH_KEYS'].endswith('\n'))

    def test_an_account_with_no_ssh_keys_gets_only_the_builds_key(self):
        # image.sh writes the build's key for the person whatever the file says.
        for account in [dict(name='jim', password_hash=self.HASH), None]:
            if account:
                self.write_account(**account)
            else:
                void_base.ACCOUNT_FILE.unlink()
            env, _ = self.build()
            self.assertEqual(env['PERSON_SSH_KEYS'], '')
            self.assertTrue(env['PUBKEY'].endswith('id_ed25519.pub'))

    def test_ssh_keys_that_would_do_more_than_let_a_key_in_are_refused(self):
        good = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJ96oqWr9ruQ7W'
        for keys in ['ssh-ed25519 AAAA', [good + '\nssh-rsa AAAA'], [good + ' a\nb'],
                     ['command="sh" ' + good], ['AAAAC3NzaC1lZDI1NTE5AAAAIJ96'],
                     ['ssh-dss AAAAB3NzaC1kc3M='], [''], [42], [good, None]]:
            self.write_account(name='jim', password_hash=self.HASH, ssh_keys=keys)
            with self.assertRaises(SystemExit, msg=keys):
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


class PersonSshCheck(unittest.TestCase):
    """Base's check logs in as the person, with the build's key, not as root."""

    def test_the_check_runs_as_the_lab_account_and_wants_its_uid(self):
        [(user, command, ok)] = [
            (user, command, ok) for name, user, command, ok in void_base.boot_checks()
            if name == "the person's account takes the build's key over SSH"
        ]
        self.assertEqual(user, void_base.LAB_ACCOUNT['name'])
        self.assertTrue(ok(f'{void_base.PERSON_UID}\n'))
        self.assertFalse(ok('0\n'))
        with patch('subprocess.run') as run:
            void_base.ssh(command, user=user)
        self.assertIn('crowbar@127.0.0.1', run.call_args.args[0])
        self.assertNotIn('root@127.0.0.1', run.call_args.args[0])

    def test_boot_checks_resolves_root_and_person_users_for_every_check(self):
        resolved = void_base.boot_checks()
        self.assertEqual(len(resolved), len(void_base.CHECKS))
        users = {name: user for name, user, _, _ in resolved}
        self.assertEqual(users['Void Linux'], 'root')
        self.assertEqual(users["the person's account takes the build's key over SSH"],
                         void_base.LAB_ACCOUNT['name'])
        self.assertEqual(users['the person may reboot and power off from the terminal'],
                         void_base.LAB_ACCOUNT['name'])


class SystemLogCheck(unittest.TestCase):
    """Base's check wants both a service's and the kernel's message in the log."""

    def setUp(self):
        self.passes = dict((name, ok) for name, _, ok, *_ in void_base.CHECKS)[
            "the system's log keeps services' and the kernel's messages"]

    def test_both_messages_pass(self):
        self.assertTrue(self.passes('kernel\nservice\n'))

    def test_either_alone_or_neither_fails(self):
        for out in ('kernel\n', 'service\n', ''):
            self.assertFalse(self.passes(out), out)


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


class CompositorRestartCheck(unittest.TestCase):
    """Base's check finds that restarting the compositor's service puts one
    new compositor in the place of the old."""

    def setUp(self):
        self.passes = void_base.compositor_restarted

    def test_one_new_compositor_that_stays_passes(self):
        self.assertTrue(self.passes('restart 0\nbefore 640\nafter 1669\nlater 1669\n'))

    def test_the_old_compositor_left_running_fails(self):
        # runit stopped openvt, the old compositor kept the seat, and each new
        # one failed to start.
        self.assertFalse(self.passes('restart 1\nbefore 640\nafter 640\nlater 640\n'))
        self.assertFalse(self.passes('restart 0\nbefore 640\nafter 640 1669\nlater 640 1669\n'))

    def test_no_compositor_or_one_that_does_not_stay_fails(self):
        self.assertFalse(self.passes('restart 0\nbefore 640\nafter\nlater\n'))
        self.assertFalse(self.passes('restart 0\nbefore 640\nafter 1669\nlater 1702\n'))
        self.assertFalse(self.passes('restart 0\nbefore 640\nafter 1669\n'))
        self.assertFalse(self.passes(''))


class DesktopServicesCheck(unittest.TestCase):
    """Base's check finds the desktop's services running, and not one that
    runit starts again each second."""

    def setUp(self):
        self.passes = void_base.desktop_services_stay_up

    def status(self, shell='/usr/local/bin/crowbaros-shell', **other):
        """`sv status` of the desktop's services, each up for 12 seconds
        unless `other` gives its line, then the program the shell's service
        runs."""
        states = {service: 'run: {}: (pid 700) 12s' for service in void_base.DESKTOP_SERVICES}
        states.update({name.replace('_', '-'): line for name, line in other.items()})
        return ''.join(line.format(f'/var/service/{service}') + '\n'
                       for service, line in states.items()) + f'shell {shell}\n'

    def test_every_service_up_for_a_while_passes(self):
        self.assertTrue(self.passes(self.status()))
        self.assertTrue(self.passes(self.status(seatd='run: {}: (pid 512) 3s; run: log: (pid 511) 3s')))

    def test_a_service_that_is_down_fails(self):
        self.assertFalse(self.passes(self.status(
            crowbar_shell='down: {}: 1s, normally up, want up')))

    def test_a_service_runit_has_only_just_started_again_fails(self):
        # A shell that cannot start is running whenever runit has just started it.
        self.assertFalse(self.passes(self.status(crowbar_shell='run: {}: (pid 2281) 0s')))
        self.assertFalse(self.passes(self.status(crowbar_unlock='run: {}: (pid 2281) 2s')))

    def test_a_shell_service_still_waiting_for_the_compositor_fails(self):
        # The service's program waits for the compositor's sockets before it
        # becomes the shell: up for 12 seconds, and no shell yet.
        self.assertFalse(self.passes(self.status(shell='/usr/local/bin/crowbar-helpers')))
        self.assertFalse(self.passes(self.status(shell='')))

    def test_the_check_looks_for_the_program_the_shells_service_becomes(self):
        connect = (Path(void_base.HERE) / 'helpers/src/shell_connect.rs').read_text()
        self.assertIn(f'const SHELL_PROGRAM: &str = "{void_base.SHELL_PROGRAM}";', connect)

    def test_a_service_that_is_not_there_fails(self):
        self.assertFalse(self.passes(self.status(
            crowbar_launcher="fail: {}: unable to change to service directory: file does not exist")))
        self.assertFalse(self.passes(''))

    def test_the_check_asks_about_every_service_of_the_desktop(self):
        # The services the image step turns on for the desktop, but for
        # Ctrl+Alt+Del's, which the check of every service covers.
        script = (Path(void_base.HERE) / 'host/image.sh').read_text()
        self.assertIn('for service in compositor shell launcher unlock ctrl-alt-del; do', script)
        self.assertIn('ln -sfn /etc/sv/seatd "$SV/seatd"', script)
        command = next(command for name, command, *_ in void_base.CHECKS
                       if name == "the desktop's services stay up")
        for service in ('seatd', 'crowbar-compositor', 'crowbar-shell', 'crowbar-launcher',
                        'crowbar-unlock'):
            self.assertIn(f'/var/service/{service}', command)


class RebootCheck(unittest.TestCase):
    """Base's check finds that the person may reboot and power off."""

    def setUp(self):
        self.person = void_base.person_may_reboot

    def test_wheel_and_both_commands_through_sudo_pass(self):
        self.assertTrue(self.person('crowbar netdev wheel\n/usr/local/bin/reboot\n/usr/local/bin/poweroff\n'
                                    '/usr/bin/reboot\n/usr/bin/poweroff\n'))

    def test_voids_own_commands_or_no_wheel_fail(self):
        # Void's own, found first, tell the person "access denied".
        self.assertFalse(self.person('crowbar netdev wheel\n/usr/bin/reboot\n/usr/bin/poweroff\n'
                                     '/usr/bin/reboot\n/usr/bin/poweroff\n'))
        self.assertFalse(self.person('crowbar netdev\n/usr/local/bin/reboot\n/usr/local/bin/poweroff\n'
                                     '/usr/bin/reboot\n/usr/bin/poweroff\n'))
        self.assertFalse(self.person('crowbar netdev wheel\n/usr/local/bin/reboot\n/usr/local/bin/poweroff\n'))


class SshCheck(unittest.TestCase):
    """Base's check reads sshd's settings as sshd -T prints them."""

    def setUp(self):
        self.passes = dict((name, ok) for name, _, ok, *_ in void_base.CHECKS)['SSH takes keys and no passwords']

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
        checks = dict((name, ok) for name, _, ok, *_ in void_base.CHECKS)
        self.cmdline = checks["kernel command line carries NVIDIA's settings"]
        self.module = checks["NVIDIA's module is built, and not loaded without an NVIDIA card"]
        self.suspend = checks["NVIDIA's module saves video memory to the disk across suspend"]

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

    OPTIONS = 'options nvidia NVreg_PreserveVideoMemoryAllocations=1 NVreg_TemporaryFilePath=/var/tmp\n'

    def test_both_options_and_var_tmp_on_the_root_pass(self):
        # modprobe -c's lines for the module, then the file system under /var/tmp.
        self.assertTrue(self.suspend(self.OPTIONS + 'options nvidia_drm modeset=1\nbtrfs\n'))

    def test_the_drivers_own_defaults_fail(self):
        # What the module did with no options: 2 and "" (issue #272).
        self.assertFalse(self.suspend('btrfs\n'))
        self.assertFalse(self.suspend('options nvidia NVreg_PreserveVideoMemoryAllocations=1\nbtrfs\n'))

    def test_var_tmp_in_memory_fails(self):
        self.assertFalse(self.suspend(self.OPTIONS + 'tmpfs\n'))

    def test_a_later_line_that_sets_them_otherwise_fails(self):
        # modprobe gives the module every line's options, and the last value wins.
        self.assertFalse(self.suspend(self.OPTIONS + 'options nvidia NVreg_TemporaryFilePath=/tmp\nbtrfs\n'))


if __name__ == '__main__':
    unittest.main()
