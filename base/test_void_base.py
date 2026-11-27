"""Base's build: which roots it trusts, what it hands the build and the image,
and how its check reads SSH."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
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

    def cached_root(self):
        """A finished root, saved as CI saves it, with the user namespace left
        out: the test owns every file already."""
        self.kernel = True
        void_base.rootfs(force=False)
        (void_base.ROOTFS / 'etc').mkdir()
        (void_base.ROOTFS / 'etc/shadow').write_text('root:*:\n')
        (void_base.ROOTFS / 'etc/shadow').chmod(0o600)
        (void_base.ROOTFS / 'bin').symlink_to('usr/bin')
        archive = void_base.WORK / 'cache/root.tar.zst'
        void_base.save_root(archive)
        return archive

    def test_a_saved_root_is_restored_in_place_of_the_one_there_and_used(self):
        with patch.object(void_base, 'userns', lambda: []):
            archive = self.cached_root()
            (void_base.ROOTFS / 'etc/shadow').write_text('changed\n')
            (void_base.ROOTFS / 'stray').write_text('left by another build\n')
            self.assertTrue(void_base.restore_root(archive))
        root = void_base.ROOTFS
        self.assertEqual((root / 'etc/shadow').read_text(), 'root:*:\n')
        self.assertEqual((root / 'etc/shadow').stat().st_mode & 0o777, 0o600)
        self.assertEqual(os.readlink(root / 'bin'), 'usr/bin')
        self.assertFalse((root / 'stray').exists())
        void_base.rootfs(force=False)
        self.assertEqual(self.installs, 1)

    def test_a_root_that_does_not_unpack_is_installed_again(self):
        with patch.object(void_base, 'userns', lambda: []):
            archive = self.cached_root()
            archive.write_bytes(archive.read_bytes()[:len(archive.read_bytes()) // 2])
            self.assertFalse(void_base.restore_root(archive))
        self.assertFalse((void_base.WORK / 'rootfs.done').exists())
        void_base.rootfs(force=False)
        self.assertEqual(self.installs, 2)

    def test_a_restored_root_from_other_packages_is_installed_again(self):
        with patch.object(void_base, 'userns', lambda: []):
            archive = self.cached_root()
            with patch.dict(void_base.CFG, packages=[*void_base.CFG['packages'], 'another']):
                void_base.restore_root(archive)
                void_base.rootfs(force=False)
        self.assertEqual(self.installs, 2)

    def test_an_unfinished_root_is_not_saved(self):
        self.kernel, self.nvidia = True, False
        with self.assertRaises(SystemExit):
            void_base.rootfs(force=False)
        archive = void_base.WORK / 'cache/root.tar.zst'
        with patch.object(void_base, 'userns', lambda: []), self.assertRaises(SystemExit):
            void_base.save_root(archive)
        self.assertFalse(archive.exists())

    def test_the_root_key_follows_what_installs_the_root(self):
        key = void_base.root_key()
        with patch.dict(void_base.CFG, packages=[*void_base.CFG['packages'], 'another']):
            self.assertNotEqual(void_base.root_key(), key)
        with patch.dict(void_base.CFG, xbps_static_sha256='0' * 64):
            self.assertNotEqual(void_base.root_key(), key)
        with patch.dict(void_base.CFG, void_repository='https://example.org/current'):
            self.assertNotEqual(void_base.root_key(), key)
        script = (void_base.HERE / 'host/rootfs.sh').read_text()
        here = void_base.WORK / 'here'
        (here / 'host').mkdir(parents=True)
        (here / 'host/rootfs.sh').write_text(script + 'true\n')
        with patch.object(void_base, 'HERE', here):
            self.assertNotEqual(void_base.root_key(), key)
        (here / 'host/rootfs.sh').write_text(script)
        with patch.object(void_base, 'HERE', here):
            self.assertEqual(void_base.root_key(), key)

    def test_the_root_key_holds_through_a_change_to_anything_else(self):
        key = void_base.root_key()
        with patch.dict(void_base.CFG, nvidia_options=['another'], vcpus=64, lab_packages=[],
                        build_packages=['another']):
            self.assertEqual(void_base.root_key(), key)
        source = Path(void_base.__file__).read_text()
        roots = void_base.ROOT_CODE
        code = void_base.reachable_source(source, roots)
        # The install, its checks, the xbps it runs, and the archive are in the
        # key. The boot checks, the VM, and the desktop's build are not.
        for name in ('install', 'fetch', 'userns', 'subordinate', 'run_shown', 'missing_from_root',
                     'early_cpio', 'image_kernel', 'NVIDIA_MODULES', 'MICROCODE', 'ROOT_ARCHIVE'):
            self.assertIn(name, code)
        for name in ('CHECKS', 'check', 'boot', 'qemu', 'build', 'compile_desktop', 'main'):
            self.assertNotIn(name, code)
        self.assertEqual(void_base.reachable_source(source + '\nCHECKS = []\n', roots), code)
        # Every value of config.json the root's code reads is in the key, and
        # found by reading that code, not listed by hand.
        self.assertEqual(void_base.config_read(source, roots),
                         {'packages', 'void_repository', 'xbps_static_url', 'xbps_static_sha256'})

    def test_code_is_reached_through_whatever_names_it(self):
        source = ('import os\nLIMIT = 3\nOTHER = 4\n'
                  'def helper():\n    return LIMIT\n'
                  'def wrapped():\n    return (lambda: helper())()\n'
                  'def unused():\n    return OTHER\n')
        code = void_base.reachable_source(source, ['wrapped'])
        self.assertEqual(set(code), {'import', 'wrapped', 'helper', 'LIMIT'})
        changed = void_base.reachable_source(source.replace('LIMIT = 3', 'LIMIT = 5'), ['wrapped'])
        self.assertNotEqual(changed, code)
        self.assertEqual(void_base.reachable_source(source.replace('OTHER = 4', 'OTHER = 5'),
                                                    ['wrapped']), code)
        # An import may be what a name means, and so may a decorator.
        for change in (source.replace('import os', 'import sys as os'),
                       source.replace('def helper', '@staticmethod\ndef helper'),
                       source.replace('def helper', 'async def helper')):
            self.assertNotEqual(void_base.reachable_source(change, ['wrapped']), code)

    def test_a_module_the_key_cannot_follow_is_refused(self):
        source = 'LIMIT = [3]\ndef helper():\n    return LIMIT\n'
        void_base.reachable_source(source, ['helper'])
        for more in ('LIMIT.append(4)\n', 'if LIMIT:\n    LIMIT = []\n', 'LIMIT += [4]\n',
                     'try:\n    LIMIT = []\nexcept OSError:\n    pass\n',
                     'for LIMIT in ():\n    pass\n', 'del LIMIT\n',
                     'if __name__ == "__main__":\n    LIMIT = []\n    helper()\n',
                     'if __name__ == "__main__":\n    helper()\nelse:\n    LIMIT = []\n'):
            with self.assertRaises(ValueError, msg=more):
                void_base.reachable_source(source + more, ['helper'])
        void_base.reachable_source(source + 'if __name__ == "__main__":\n    helper()\n', ['helper'])

    def test_code_the_key_does_not_follow_may_not_change_what_it_does(self):
        source = 'LIMIT = [3]\nCFG = {"packages": []}\ndef helper():\n    return LIMIT, CFG\n'
        void_base.reachable_source(source + 'def other():\n    return LIMIT[0], CFG["packages"]\n',
                                   ['helper'])
        for more in ('SETUP = LIMIT.append(4)\n', 'X = (LIMIT := 5)\n',
                     'def other():\n    global LIMIT\n    LIMIT = []\n',
                     'def other():\n    LIMIT.append(4)\n',
                     'def other():\n    CFG["packages"].append("another")\n',
                     'def other():\n    LIMIT[0] = 4\n', 'def other():\n    del CFG["packages"]\n',
                     'class Other:\n    LIMIT.clear()\n',
                     'if __name__ == "__main__":\n    LIMIT.append(4)\n'):
            with self.assertRaises(ValueError, msg=more):
                void_base.reachable_source(source + more, ['helper'])
        # What the key follows may change what it likes: the change is in the key.
        inside = source.replace('return LIMIT, CFG', 'LIMIT.append(4)\n    return LIMIT, CFG')
        self.assertNotEqual(void_base.reachable_source(inside, ['helper']),
                            void_base.reachable_source(source, ['helper']))

    def test_config_the_roots_code_reads_is_found_or_refused(self):
        source = ('CFG = {}\nREPOSITORY = CFG["repository"]\n'
                  'def install():\n    return REPOSITORY, CFG["packages"]\n'
                  'def boot():\n    return CFG["vcpus"]\n')
        self.assertEqual(void_base.config_read(source, ['install']), {'repository', 'packages'})
        for use in ('CFG.get("packages")', 'CFG[name]', 'dict(CFG)'):
            with self.assertRaises(ValueError, msg=use):
                void_base.config_read(source.replace('CFG["packages"]', use), ['install'])
        with patch.object(void_base.Path, 'read_text', return_value='import os\nos.getcwd()\n'), \
                self.assertRaises(SystemExit):
            void_base.root_key()

    def test_a_build_root_installed_without_cargo_is_not_trusted(self):
        build_root = void_base.WORK / 'buildroot'
        with patch.object(void_base, 'BUILD_ROOT', build_root), \
                patch.object(void_base, 'run', lambda *args, **kw: None):
            with self.assertRaises(SystemExit):
                void_base.compile_desktop(force=False)
        self.assertFalse(build_root.with_name('buildroot.done').exists())


class PinFixture(unittest.TestCase):
    """A cache of the test's own, and a curl that serves what each address is given."""

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

    def tar(self, path, data):
        """An archive of one file."""
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode='w') as tar:
            info = tarfile.TarInfo(path)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        return out.getvalue()


class StaticXbps(PinFixture):
    """The static xbps the builds run is the one the configuration pins."""

    def archive(self, program):
        return self.tar('usr/bin/xbps-install.static', program)

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
        self.cwds = []

        # What the image step leaves where it was told to write, and whether
        # it then stops as a failed or interrupted step does.
        self.image_written = b'the new image'
        self.image_step_stops = False

        def run(*args, **kw):
            self.commands.append([str(a) for a in args])
            self.envs.append(kw.get('env', {}))
            self.cwds.append(kw.get('cwd'))
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
                            ('opencode', lambda: Path('/opencode')),
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

    def test_files_is_built_with_the_desktop(self):
        self.assertIn('apps/files', dict(void_base.PROJECTS))
        self.assertEqual(void_base.PROGRAMS['crowbaros-files'], 'apps/files')

    def test_the_desktops_helpers_are_one_program_built_with_the_desktop(self):
        self.assertIn('base/helpers', dict(void_base.PROJECTS))
        self.assertEqual(void_base.binaries()['crowbar-helpers'].name, 'crowbar-helpers')

    def test_python_is_only_in_an_image_built_for_the_lab_checks(self):
        # The lab's checks drive the guest with `python3 -c` and the stub
        # Model; the person's image has neither.
        self.assertNotIn('python3', void_base.CFG['packages'])
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        _, person = self.image_step()
        self.assertEqual(person['LAB_PACKAGES'], '')
        self.assertEqual(person['STUB_MODEL'], '')
        self.assertEqual(person['OPENCODE'], '/opencode')
        self.commands.clear()
        self.envs.clear()
        void_base.build(lab_account=True)
        _, lab = self.image_step()
        self.assertEqual(lab['LAB_PACKAGES'].split(), ['python3'])
        self.assertEqual(lab['XBPS'], '/xbps-static/usr/bin')
        self.assertTrue(lab['STUB_MODEL'].endswith('lab/void/stub_model.py'))
        self.assertEqual(lab['OPENCODE'], '/opencode')

    def test_opencode_ripgrep_and_git_are_bundled_in_base(self):
        self.assertIn('ripgrep', void_base.CFG['packages'])
        self.assertIn('git', void_base.CFG['packages'])
        self.assertTrue(void_base.CFG['opencode_version'].startswith('2.'))
        self.assertEqual(len(void_base.CFG['opencode_sha256']), 64)
        self.assertIn(f"v{void_base.CFG['opencode_version']}", void_base.CFG['opencode_url'])
        image_sh = (Path(void_base.HERE) / 'host/image.sh').read_text()
        self.assertIn('install -D -m 755 "$OPENCODE" "$R/usr/local/bin/opencode"', image_sh)
        self.assertIn('install -D -m 755 "$STUB_MODEL" "$R/usr/local/lib/crowbar-lab/stub-model.py"', image_sh)

    def test_the_image_step_has_a_mount_namespace_to_lend_the_lab_packages_dev(self):
        # Python's install byte-compiles its library, which needs /dev/shm.
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build(lab_account=True)
        image, _ = self.image_step()
        self.assertIn('--mount', image[:image.index('sh')])



class NoHarnessOrNode(BuildFixture):
    """Base carries no Pi Durable Harness, no Node.js, and no `crowbar-agent`
    runit service (ADR 0036). `crowbar-agent` itself is Lever's Rust client."""

    def test_config_has_no_node_pin_and_no_nodejs_package(self):
        cfg = json.loads((Path(void_base.HERE) / 'config.json').read_text())
        self.assertNotIn('node_url', cfg)
        self.assertNotIn('node_sha256', cfg)
        for packages in ('packages', 'lab_packages', 'build_packages'):
            self.assertFalse([name for name in cfg[packages] if name.startswith('nodejs')], packages)

    def test_build_downloads_no_node_bundles_nothing_and_ships_no_harness(self):
        self.assertFalse(hasattr(void_base, 'node'))
        self.assertFalse(hasattr(void_base, 'bundle_harness'))
        self.assertNotIn('agent/client', dict(void_base.PROJECTS))
        self.assertEqual(void_base.PROGRAMS['crowbar-agent'], 'lever')
        self.assertFalse((Path(void_base.HERE) / 'desktop/agent-run').exists())
        image_script = (Path(void_base.HERE) / 'host/image.sh').read_text()
        for retired in ('crowbar-agent', 'harness.mjs', '$NODE', '$HARNESS'):
            self.assertNotIn(retired, image_script)
        self.root(['6.18.64_1'], nvidia=['6.18.64_1'])
        void_base.build()
        _, env = self.image_step()
        for key in ('NODE', 'NODE_LICENSE', 'HARNESS'):
            self.assertNotIn(key, env)
        self.assertFalse(any(cmd and Path(cmd[0]).name == 'npm' for cmd in self.commands))

    def test_agent_subcommand_is_gone(self):
        with patch('sys.argv', ['void_base.py', 'agent']), \
                patch('sys.stderr', new_callable=io.StringIO), \
                self.assertRaises(SystemExit) as stopped:
            void_base.main()
        self.assertEqual(stopped.exception.code, 2)

    def test_boot_check_fails_if_the_image_holds_harness_node_or_agent_service(self):
        checks = {name: (command, ok) for name, command, ok, *_ in void_base.CHECKS}
        command, passes = checks['the image holds no Harness, no Node.js, and no crowbar-agent service']
        for path in void_base.RETIRED_AGENT_PATHS:
            self.assertIn(path, command)
        for cmd in void_base.RETIRED_AGENT_COMMANDS:
            self.assertIn(cmd, command)
        self.assertIn('xbps-query -l', command)
        self.assertTrue(passes('clean\n'))
        for bad in ('', 'path /usr/local/lib/crowbar-agent\nclean\n',
                    'path /var/service/crowbar-agent\nclean\n',
                    'cmd node /usr/bin/node\nclean\n',
                    'pkg nodejs-22.0.0_1\nclean\n'):
            self.assertFalse(passes(bad), bad)


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
        self.assertIn('for service in compositor shell launcher unlock ctrl-alt-del lever pipewire monitor-status; do', script)
        self.assertIn('ln -sfn /etc/sv/seatd "$SV/seatd"', script)
        command = next(command for name, command, *_ in void_base.CHECKS
                       if name == "the desktop's services stay up")
        for service in ('seatd', 'crowbar-compositor', 'crowbar-shell', 'crowbar-launcher',
                        'crowbar-unlock', 'crowbar-lever', 'crowbar-pipewire', 'crowbar-monitor-status'):
            self.assertIn(f'/var/service/{service}', command)


class LeverChecks(unittest.TestCase):
    """Base builds `crowbar-lever`, installs its runit service, points the
    shell at its surface socket, and checks it at boot."""

    OK_STATUS = (
        f'ok {void_base.VERSION_FRAME} '
        '{"reply":"status","version":"0.1.0","harness_started":false,'
        '"harnesses":[{"id":"harness-1","name":"OpenCode"}],"free_hosted_models":false}'
    )

    def test_lever_is_built_and_wired_into_base(self):
        self.assertIn('lever', dict(void_base.PROJECTS))
        self.assertEqual(void_base.PROGRAMS['crowbar-lever'], 'lever')
        self.assertEqual(void_base.PROGRAMS['crowbar-agent'], 'lever')
        self.assertEqual(void_base.binaries()['crowbar-lever'].name, 'crowbar-lever')
        self.assertEqual(void_base.binaries()['crowbar-agent'].name, 'crowbar-agent')
        lever_run = (Path(void_base.HERE) / 'desktop/lever-run').read_text()
        self.assertIn('/usr/local/bin/crowbar-launcher as-app', lever_run)
        self.assertIn('/usr/local/bin/crowbar-lever', lever_run)
        self.assertIn('--harness /usr/local/bin/opencode --harness-arg acp --harness-name OpenCode', lever_run)
        self.assertIn('--fulcrum-socket /run/crowbar-trusted/agent/compositor', lever_run)
        self.assertIn(void_base.LEVER_SURFACE_SOCKET, lever_run)
        self.assertIn(void_base.LEVER_STATE, lever_run)
        shell_run = (Path(void_base.HERE) / 'desktop/shell-run').read_text()
        self.assertIn(f'CROWBAR_LEVER_SOCKET={void_base.LEVER_SURFACE_SOCKET}', shell_run)

    def test_opencode_and_stub_model_boot_check_parsers(self):
        tools_ok = (
            'opencode-path /usr/local/bin/opencode\n'
            f'opencode-version opencode v{void_base.CFG["opencode_version"]}\n'
            'rg-path /usr/bin/rg\n'
            'git-path /usr/bin/git\n'
        )
        self.assertTrue(void_base.opencode_and_tools_installed(tools_ok))
        self.assertFalse(void_base.opencode_and_tools_installed(tools_ok.replace(void_base.CFG['opencode_version'], '0.0.0')))
        self.assertFalse(void_base.opencode_and_tools_installed(tools_ok.replace('/usr/bin/rg', '')))

        stub_ok = '\n'.join([
            'person crowbar',
            'idle-before none',
            'refused-rc 1',
            'refused-err crowbar-agent: The Agent gave no answer: No Model is available.',
            'models-rc 0',
            'models-out stub/stub-1  Stub One  (default)',
            'models-out stub/stub-2  Stub Two',
            'hello-rc 0',
            'hello-out Hello from stub-1!',
            'set-model-rc 0',
            'request-asked [request] Run: id -un && id -u',
            'answer-rc 0',
            'cmd-rc 0',
            f'cmd-out Command ran on stub-2 as: crowbar {void_base.PERSON_UID}',
            'opencode-working 2 450000',
            'config-unchanged yes',
            'idle-after none',
        ]) + '\n'
        self.assertTrue(void_base.opencode_stub_turn_ok(stub_ok))
        self.assertFalse(void_base.opencode_stub_turn_ok(stub_ok.replace('idle-before none', 'idle-before running')))
        self.assertFalse(void_base.opencode_stub_turn_ok(stub_ok.replace('refused-rc 1', 'refused-rc 0')))
        self.assertFalse(void_base.opencode_stub_turn_ok(stub_ok.replace('config-unchanged yes', 'config-unchanged no')))
        self.assertFalse(void_base.opencode_stub_turn_ok(stub_ok.replace('idle-after none', 'idle-after running')))
        # The command ran without asking first, or its request was not answered.
        self.assertFalse(void_base.opencode_stub_turn_ok(stub_ok.replace('request-asked [request] Run: id -un && id -u\n', '')))
        self.assertFalse(void_base.opencode_stub_turn_ok(stub_ok.replace('answer-rc 0', 'answer-rc 1')))

    def test_lever_answers_surface_socket_check(self):
        good = '\n'.join([
            f'shell {self.OK_STATUS}',
            'root refused',
            'person refused',
            'group refused',
            'other error: Permission denied',
            f'directory {void_base.PERSON_UID} 1001 2750',
            f'socket {void_base.PERSON_UID} 1001 660',
            'shell-account 1001 1001',
        ]) + '\n'
        self.assertTrue(void_base.lever_answers_surface(good))
        self.assertFalse(void_base.lever_answers_surface(good.replace('root refused', f'root {self.OK_STATUS}')))
        self.assertFalse(void_base.lever_answers_surface(good.replace('"harness_started":false', '"harness_started":true')))
        self.assertFalse(void_base.lever_answers_surface(good.replace('2750', '755')))

    def test_lever_answers_person_socket_check(self):
        good = '\n'.join([
            f'person {self.OK_STATUS}',
            'root refused',
            'shell error: Permission denied',
            f'directory {void_base.PERSON_UID} 700',
            f'socket {void_base.PERSON_UID} 600',
        ]) + '\n'
        self.assertTrue(void_base.lever_answers_person(good))
        self.assertFalse(void_base.lever_answers_person(good.replace('root refused', f'root {self.OK_STATUS}')))
        self.assertFalse(void_base.lever_answers_person(good.replace('600', '660')))

    def test_lever_runs_as_the_person_check(self):
        uid = void_base.PERSON_UID
        state = f'/home/crowbar/{void_base.LEVER_STATE}'
        good = '\n'.join([
            'service run: /var/service/crowbar-lever: (pid 812) 5s',
            f'process {uid} {uid} {uid} 10 990',
            f'person {uid} {uid} {uid} 10 990',
            f'program {void_base.LEVER_PROGRAM}',
            'cwd /home/crowbar',
            'home /home/crowbar',
            'env HOME=/home/crowbar',
            f'env XDG_RUNTIME_DIR=/run/user/{uid}',
            'env WAYLAND_DISPLAY=wayland-0',
            'env CROWBAR_SESSION=crowbaros',
            'env PATH=/home/crowbar/.local/bin:/usr/local/bin:/usr/bin:/bin',
            'env LANG=C.UTF-8',
            f'state 700 {uid} {state}',
            f'state 600 {uid} {state}/lever.log',
            f'state 600 {uid} {state}/lever.sqlite',
            f'state 600 {uid} {state}/running',
        ]) + '\n'
        self.assertTrue(void_base.lever_runs_as_the_person(good))
        self.assertFalse(void_base.lever_runs_as_the_person(good.replace('state 600', 'state 644', 1)))
        self.assertFalse(void_base.lever_runs_as_the_person(good.replace('lever.sqlite', 'other.db')))
        self.assertFalse(void_base.lever_runs_as_the_person(good.replace('env CROWBAR_SESSION=crowbaros\n', '')))

    def test_lever_restarted_and_memory_checks(self):
        good = f'before 400\nafter 512\nshell {self.OK_STATUS}\nlogged 1\n'
        self.assertTrue(void_base.lever_restarted(good))
        self.assertFalse(void_base.lever_restarted(good.replace('after 512', 'after 400')))
        self.assertFalse(void_base.lever_restarted(good.replace('logged 1', 'logged 0')))
        self.assertTrue(void_base.lever_is_small('4096\n'))
        self.assertFalse(void_base.lever_is_small(f'{50 * 1024}\n'))
        self.assertFalse(void_base.lever_is_small('0\n'))


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


class UnlockHelperCheck(unittest.TestCase):
    """Base's check has the unlock helper lock through the compositor that
    booted: the two speak one framing (ADR 0035), and the tests of each play
    the other."""

    def setUp(self):
        checks = {name: (command, ok) for name, command, ok, *_ in void_base.CHECKS}
        self.command, self.passes = checks["the compositor answers the unlock helper's lock"]

    def test_it_runs_the_helper_the_suspend_hook_runs(self):
        hook = (void_base.ROOT / 'base/desktop/zzz-lock').read_text()
        self.assertIn('/usr/local/bin/crowbar-unlock lock', hook)
        self.assertIn('/usr/local/bin/crowbar-unlock lock', self.command)

    def test_a_lock_the_compositor_answered_passes(self):
        self.assertTrue(self.passes('exit 0\n'))

    def test_a_lock_the_compositor_refused_or_never_answered_fails(self):
        self.assertFalse(self.passes('exit 1\n'))
        self.assertFalse(self.passes(''))

    def test_it_comes_after_the_compositor_is_seen_to_stay_up(self):
        names = [name for name, *_ in void_base.CHECKS]
        self.assertLess(names.index("the desktop's services stay up"),
                        names.index("the compositor answers the unlock helper's lock"))


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


class AudioBluetoothAndMpv(unittest.TestCase):
    def test_pipewire_audio_ready_checks_pulse_socket_and_graph(self):
        good = (
            "pulse-socket yes\n"
            "PipeWire 'pipewire-0' [1.6.10, crowbar@crowbaros]\n"
            "Audio\n"
            " ├─ Sinks:\n"
            " │  * 34. Dummy Output [vol: 1.00]\n"
            "Clients:\n"
            "   32. WirePlumber [1.6.10]\n"
        )
        self.assertTrue(void_base.pipewire_audio_ready(good))
        self.assertFalse(void_base.pipewire_audio_ready(good.replace("pulse-socket yes", "pulse-socket no")))
        self.assertFalse(void_base.pipewire_audio_ready(good.replace("WirePlumber", "other")))

    def test_bluetooth_reachable_requires_service_and_dbus_reply(self):
        good = (
            "run: /var/service/bluetoothd: (pid 611) 18s\n"
            "dbus-exit 0\n"
            "method return time=1.0 sender=:1.4 -> destination=:1.9\n"
            "   array [\n"
            "   ]\n"
        )
        self.assertTrue(void_base.bluetooth_reachable(good))
        self.assertFalse(void_base.bluetooth_reachable(good.replace("dbus-exit 0", "dbus-exit 1")))
        self.assertFalse(void_base.bluetooth_reachable(good.replace("run: ", "down: ")))

    def test_mpv_defaults_ready_checks_mime_version_conf_and_libs(self):
        good = (
            "video mpv.desktop\n"
            "audio mpv.desktop\n"
            "mpv mpv v0.42.0 Copyright © 2000-2026 mpv/MPlayer/mplayer2 projects\n"
            "conf yes\n"
            "\tlibpipewire-0.3.so.0 => /usr/lib/libpipewire-0.3.so.0 (0x00007f00)\n"
        )
        self.assertTrue(void_base.mpv_defaults_ready(good))
        self.assertFalse(void_base.mpv_defaults_ready(good.replace("video mpv.desktop", "video vlc.desktop")))
        self.assertFalse(void_base.mpv_defaults_ready(good.replace("audio mpv.desktop", "audio ")))
        self.assertFalse(void_base.mpv_defaults_ready(good.replace("conf yes", "conf no")))
        self.assertFalse(void_base.mpv_defaults_ready(good + "\tlibfoo.so => not found\n"))
        self.assertFalse(void_base.mpv_defaults_ready(
            good.replace("mpv mpv v0.42.0 Copyright © 2000-2026 mpv/MPlayer/mplayer2 projects",
                         "mpv Error parsing option osd-color (option requires parameter)")))

    def test_mpv_conf_quotes_hash_colors(self):
        # Unquoted `#` starts a comment in mpv.conf, turning hex colors into
        # empty values that make mpv fail option parsing on startup.
        conf = (Path(void_base.HERE) / 'desktop/mpv.conf').read_text()
        for line in conf.splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith('#'):
                continue
            self.assertNotIn('=#', stripped, f'unquoted # in mpv.conf: {line}')

    def test_agent_sign_in_desktop_entry_runs_login_in_foot_and_is_installed(self):
        entry = (Path(void_base.HERE) / 'desktop/com.crowbaros.AgentSignIn.desktop').read_text()
        self.assertIn('[Desktop Entry]', entry)
        self.assertIn('Type=Application', entry)
        self.assertIn('NoDisplay=true', entry)
        self.assertIn('Terminal=false', entry)
        self.assertIn('Exec=foot ', entry)
        self.assertIn('/usr/local/bin/crowbar-agent login', entry)
        image_sh = (Path(void_base.HERE) / 'host/image.sh').read_text()
        self.assertIn('com.crowbaros.AgentSignIn.desktop', image_sh)


if __name__ == '__main__':
    unittest.main()
