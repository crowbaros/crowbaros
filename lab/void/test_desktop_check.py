"""Each desktop check run is isolated, and boots what a person would boot."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import desktop_check

guest = desktop_check.guest
base = desktop_check.void_base


class Run(unittest.TestCase):
    """A run in a temporary repository, with Base's disk and the lab's
    development disk already there."""

    def setUp(self):
        names = ['ROOT', 'WORK', 'CFG', 'DISK', 'KEY', 'PIDFILE', 'QMP_SOCK', 'SERIAL_LOG', 'VNC']
        saved = {name: getattr(guest, name) for name in names}
        saved_base = {'BASE_DISK': base.BASE_DISK, 'KEY': base.KEY}
        self.addCleanup(lambda: [setattr(guest, n, v) for n, v in saved.items()])
        self.addCleanup(lambda: [setattr(base, n, v) for n, v in saved_base.items()])
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        guest.ROOT = Path(temporary.name)
        self.development = guest.ROOT / '.lab/void-graphical/disk.raw'
        self.development.parent.mkdir(parents=True)
        self.development.write_bytes(b'preserved VM')
        base.BASE_DISK = guest.ROOT / '.lab/base/base.raw'
        base.BASE_DISK.parent.mkdir(parents=True)
        base.BASE_DISK.write_bytes(b'the Base image')
        base.KEY = base.BASE_DISK.parent / 'id_ed25519'
        firmware = guest.ROOT / 'firmware'
        firmware.mkdir()
        (firmware / 'code.fd').write_bytes(b'OVMF code')
        (firmware / 'vars.fd').write_bytes(b'OVMF variables')
        paths = patch.dict(base.CFG, firmware_code=str(firmware / 'code.fd'),
                           firmware_vars=str(firmware / 'vars.fd'))
        paths.start()
        self.addCleanup(paths.stop)
        # rev-parse asks for text, and status for bytes.
        output = patch.object(desktop_check.subprocess, 'check_output',
                              side_effect=lambda *a, **kw: 'fixture' if kw.get('text') else b'')
        output.start()
        self.addCleanup(output.stop)


class IsolatedRuns(Run):
    def test_each_run_owns_new_state_and_never_selects_another_disk(self):
        desktop_check.configure()
        first = guest.WORK
        desktop_check.configure()
        self.assertNotEqual(first, guest.WORK)
        self.assertEqual(guest.DISK.parent, guest.WORK)
        self.assertNotIn(guest.DISK, (self.development, base.BASE_DISK))
        self.assertEqual(self.development.read_bytes(), b'preserved VM')
        self.assertIsNone(guest.CFG['vnc_port'])
        self.assertGreater(json.loads((guest.WORK / 'run.json').read_text())['ssh_port'], 0)

    def test_the_run_reaches_the_guest_with_the_key_base_put_in_the_image(self):
        desktop_check.configure()
        self.assertEqual(guest.KEY, base.KEY)


class BootsBase(Run):
    """The run boots its own copy of Base's image through the firmware, as a
    person's machine would, and leaves Base's image as it was."""

    def boot(self):
        desktop_check.configure()
        commands = []
        with patch.object(guest, 'run', lambda *args, **kw: commands.append([str(a) for a in args])):
            desktop_check.start()
        qemu = [command for command in commands if command[0] == 'qemu-system-x86_64']
        self.assertEqual(len(qemu), 1, commands)
        return qemu[0]

    def drives(self, command):
        return [command[i + 1] for i, arg in enumerate(command) if arg == '-drive']

    def test_the_run_boots_a_copy_of_bases_image(self):
        command = self.boot()
        disks = [d for d in self.drives(command) if 'if=virtio' in d]
        self.assertEqual(disks, [f'file={guest.DISK},format=raw,if=virtio,cache=unsafe'])
        self.assertEqual(guest.DISK.read_bytes(), b'the Base image')
        self.assertEqual(base.BASE_DISK.read_bytes(), b'the Base image')

    def test_the_firmware_boots_it_with_the_runs_own_variables(self):
        command = self.boot()
        self.assertNotIn('-kernel', command)
        flash = [d for d in self.drives(command) if 'if=pflash' in d]
        self.assertEqual(flash[0], f'if=pflash,format=raw,readonly=on,file={base.CFG["firmware_code"]}')
        variables = Path(flash[1].rsplit('file=', 1)[1])
        self.assertEqual(variables.parent, guest.WORK)
        self.assertEqual(variables.read_bytes(), b'OVMF variables')

    def test_the_run_drives_the_desktop_through_its_own_sockets(self):
        command = self.boot()
        joined = ' '.join(command)
        self.assertIn('virtio-gpu-pci', joined)
        self.assertIn(f'unix:{guest.QMP_SOCK}', joined)
        self.assertIn(f'hostfwd=tcp:127.0.0.1:{guest.CFG["ssh_port"]}-:22', joined)
        for vnc in guest.VNC:
            self.assertIn(f'unix:{vnc}', joined)


if __name__ == '__main__':
    unittest.main()
