"""The read-only status report exposes no runit control interface."""
import os
from pathlib import Path
import signal
import stat
import subprocess
import tempfile
import time
import unittest


class MonitorStatus(unittest.TestCase):
    def test_publisher_uses_only_status_and_publishes_a_readable_atomic_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / 'state'
            tools = root / 'bin'
            tools.mkdir()
            sv = tools / 'sv'
            sv.write_text('#!/bin/sh\n[ "$1" = status ] || exit 99\nprintf "run: example: (pid 2) 10s\\n"\n')
            sv.chmod(0o755)
            script = root / 'run'
            source = (Path(__file__).parent / 'desktop/monitor-status-run').read_text()
            script.write_text(source.replace('/run/crowbar-monitor', str(state)))
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'])
            process = subprocess.Popen(['sh', str(script)], env=env, start_new_session=True,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            try:
                report = state / 'services'
                deadline = time.monotonic() + 4
                while not report.exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(report.exists(), 'publisher did not produce its report')
                self.assertEqual(report.read_text(), 'run: example: (pid 2) 10s\n')
                self.assertEqual(stat.S_IMODE(report.stat().st_mode), 0o644)
                self.assertEqual(stat.S_IMODE(state.stat().st_mode), 0o755)
                self.assertFalse((state / 'services.tmp').exists())
            finally:
                os.killpg(process.pid, signal.SIGTERM)
                process.communicate(timeout=4)


if __name__ == '__main__':
    unittest.main()
