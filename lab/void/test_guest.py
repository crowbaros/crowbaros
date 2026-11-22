"""The lab's guest driver: isolated UEFI runs of Base's image."""
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

import guest


class GuestFixture(unittest.TestCase):
    def setUp(self):
        base = guest.void_base
        self.base = base
        saved = {name: getattr(base, name) for name in ('ROOT', 'BASE_DISK', 'DISK', 'KEY')}
        self.addCleanup(lambda: [setattr(base, n, v) for n, v in saved.items()])
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        base.ROOT = self.root
        base.BASE_DISK = self.root / '.lab/base/base.raw'
        base.DISK = self.root / '.lab/base/disk.raw'
        base.BASE_DISK.parent.mkdir(parents=True)
        base.BASE_DISK.write_bytes(b'the Base image')
        base.KEY = base.BASE_DISK.parent / 'id_ed25519'
        firmware = self.root / 'firmware'
        firmware.mkdir()
        (firmware / 'code.fd').write_bytes(b'OVMF code')
        (firmware / 'vars.fd').write_bytes(b'OVMF variables')
        paths = patch.dict(base.CFG, firmware_code=str(firmware / 'code.fd'),
                           firmware_vars=str(firmware / 'vars.fd'), disk_gib=1)
        paths.start()
        self.addCleanup(paths.stop)
        output = patch.object(guest.subprocess, 'check_output',
                              side_effect=lambda *a, **kw: 'fixture' if kw.get('text') else b'')
        output.start()
        self.addCleanup(output.stop)
        self.runs = self.root / '.lab/void-checks'


class IsolatedRuns(GuestFixture):
    def test_each_run_owns_new_state_and_uses_bases_key(self):
        first = guest.Guest.create(self.runs, 'crowbaros-check')
        second = guest.Guest.create(self.runs, 'crowbaros-check')
        self.assertNotEqual(first.work, second.work)
        self.assertEqual(first.disk.parent, first.work)
        self.assertNotIn(first.disk, (self.base.BASE_DISK, self.base.DISK))
        self.assertEqual(first.key, self.base.KEY)
        self.assertEqual(first.revision, 'fixture')
        self.assertFalse(first.dirty)
        recorded = json.loads((first.work / 'run.json').read_text())
        self.assertEqual(recorded['workspace'], str(first.work))
        self.assertEqual(recorded['image'], str(self.base.BASE_DISK))
        self.assertEqual(recorded['ssh_port'], first.ssh_port)
        self.assertEqual(recorded['revision'], first.revision)
        self.assertEqual(recorded['dirty'], first.dirty)
        self.assertGreater(first.ssh_port, 0)


class BootsBase(GuestFixture):
    def boot(self):
        vm = guest.Guest.create(self.runs, 'crowbaros-check')
        self.addCleanup(shutil.rmtree, vm.sockets, True)
        commands = []
        with patch.object(self.base, 'run', lambda *args, **kw: commands.append([str(a) for a in args])):
            vm.start()
        qemu = [command for command in commands if command[0] == 'qemu-system-x86_64']
        self.assertEqual(len(qemu), 1, commands)
        return vm, qemu[0]

    def drives(self, command):
        return [command[i + 1] for i, arg in enumerate(command) if arg == '-drive']

    def test_start_boots_a_grown_copy_of_bases_image_and_leaves_base_untouched(self):
        vm, command = self.boot()
        disks = [d for d in self.drives(command) if 'if=virtio' in d]
        self.assertEqual(disks, [f'file={vm.disk},format=raw,if=virtio,cache=unsafe'])
        self.assertEqual(vm.disk.stat().st_size, self.base.CFG['disk_gib'] << 30)
        with vm.disk.open('rb') as stream:
            self.assertEqual(stream.read(len(b'the Base image')), b'the Base image')
        self.assertEqual(self.base.BASE_DISK.read_bytes(), b'the Base image')

    def test_start_boots_under_uefi_with_the_runs_own_variables(self):
        vm, command = self.boot()
        self.assertNotIn('-kernel', command)
        flash = [d for d in self.drives(command) if 'if=pflash' in d]
        self.assertEqual(flash[0], f'if=pflash,format=raw,readonly=on,file={self.base.CFG["firmware_code"]}')
        variables = Path(flash[1].rsplit('file=', 1)[1])
        self.assertEqual(variables.parent, vm.work)
        self.assertEqual(variables.read_bytes(), b'OVMF variables')

    def test_start_exposes_the_runs_own_sockets_and_display(self):
        vm, command = self.boot()
        joined = ' '.join(command)
        self.assertIn('virtio-gpu-pci', joined)
        self.assertIn(f'unix:{vm.qmp_sock}', joined)
        self.assertIn(f'hostfwd=tcp:127.0.0.1:{vm.ssh_port}-:22', joined)
        for vnc in vm.vnc:
            self.assertIn(f'unix:{vnc}', joined)

    def test_sockets_fit_a_unix_socket_path_however_deep_the_checkout(self):
        # A Unix socket's path is at most 107 bytes, and a worktree's run
        # directory can be longer than that alone.
        self.runs = self.root / ('deep-' * 20) / '.lab/void-desktop-checks'
        vm, command = self.boot()
        for path in (vm.qmp_sock, *vm.vnc):
            self.assertLess(len(os.fsencode(path)), 108, path)
        again = guest.Guest(vm.work, 'crowbaros-check', vm.ssh_port)
        self.assertEqual((again.qmp_sock, again.vnc), (vm.qmp_sock, vm.vnc))


class SshAndBuild(GuestFixture):
    def test_ssh_uses_the_runs_port_key_and_requested_user(self):
        vm = guest.Guest.create(self.runs, 'crowbaros-check')
        ok = subprocess.CompletedProcess([], 0, '1003\n', '')
        with patch.object(guest.subprocess, 'run', return_value=ok) as run:
            result = vm.ssh('id -u', user='crowbar')
        self.assertEqual(result.stdout, '1003\n')
        args = run.call_args.args[0]
        self.assertIn(str(self.base.KEY), args)
        self.assertIn(str(vm.ssh_port), args)
        self.assertIn(f'UserKnownHostsFile={vm.work / "known_hosts"}', args)
        self.assertIn('crowbar@127.0.0.1', args)

    def test_build_base_builds_with_the_lab_account_and_returns_digests(self):
        log = self.root / 'build.log'
        binaries = {'crowbar-compositor': self.root / 'comp', 'crowbar-probe': self.root / 'probe'}
        for path in binaries.values():
            path.write_bytes(b'binary')
        with patch.object(guest.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as run, \
                patch.object(self.base, 'binaries', return_value=binaries), \
                patch.object(self.base, 'sha256', return_value='deadbeef'):
            built = guest.build_base(log)
        self.assertEqual(built, {'crowbar-compositor': 'deadbeef', 'crowbar-probe': 'deadbeef'})
        command = [str(a) for a in run.call_args.args[0]]
        self.assertTrue(command[1].endswith('void_base.py'))
        self.assertEqual(command[2:], ['build', '--lab-account'])

    def test_build_base_names_the_log_when_the_build_fails(self):
        log = self.root / 'build.log'
        with patch.object(guest.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1)):
            with self.assertRaises(RuntimeError) as error:
                guest.build_base(log)
        self.assertIn(str(log), str(error.exception))




class FulcrumClient(unittest.TestCase):
    """The checks' client of Fulcrum's own channels, against a socket that
    plays Fulcrum. The bytes are those of the compositor's README."""

    VERSION = b'\x00\x00\x00\x0d{"version":1}'

    def ask(self, says, request='{"op":"snapshot"}'):
        """What the client prints, and what Fulcrum heard, when Fulcrum says
        each of `says` in turn, each after as many bytes as the one before."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = str(Path(directory.name) / 'control')
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(path)
        listener.listen(1)
        heard = []

        def fulcrum():
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(5)
                for hears, said in says:
                    heard.append(connection.recv(hears, socket.MSG_WAITALL))
                    connection.sendall(said)
        served = threading.Thread(target=fulcrum)
        served.start()
        command = guest.ask_fulcrum(path, request).replace('python3', sys.executable, 1)
        printed = subprocess.run(command, shell=True, text=True, capture_output=True, timeout=20)
        served.join()
        listener.close()
        self.assertEqual(printed.stderr, '')
        return printed.stdout, heard

    def test_it_states_the_version_then_asks_in_a_frame_and_prints_the_reply(self):
        reply = b'{"reply":"snapshot","workspace":1}'
        printed, heard = self.ask([(17, self.VERSION),
                                   (21, struct.pack('>I', len(reply)) + reply)])
        self.assertEqual(heard, [self.VERSION, b'\x00\x00\x00\x11{"op":"snapshot"}'])
        self.assertEqual(printed, reply.decode() + '\n')

    def test_it_passes_over_the_events_the_shell_is_sent_unasked(self):
        event = b'{"event":"snapshot","workspace":1}'
        reply = b'{"reply":"error","message":"no"}'
        frames = b''.join(struct.pack('>I', len(m)) + m for m in (event, b'{"event":"locked"}', reply))
        printed, _ = self.ask([(17, self.VERSION + frames[:9]), (21, frames[9:])], '{"op":"unlock"}')
        self.assertEqual(printed, reply.decode() + '\n')

    def test_it_prints_nothing_when_fulcrum_closes_or_states_another_version(self):
        for said in (b'', b'\x00\x00\x00\x0d{"version":2}', b'{"reply":"snapshot"}\n'):
            with self.subTest(said=said):
                printed, _ = self.ask([(17, said)])
                self.assertEqual(printed, '')

    def test_the_frame_a_check_knocks_with_states_the_version(self):
        self.assertEqual(guest.FULCRUM_VERSION_FRAME, self.VERSION)


if __name__ == '__main__':
    unittest.main()
