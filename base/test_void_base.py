"""Base's build trusts a Void root only once an install of it finished."""
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


if __name__ == '__main__':
    unittest.main()
