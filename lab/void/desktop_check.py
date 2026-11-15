#!/usr/bin/env python3
"""Fresh, isolated Alpha desktop smoke test; never uses the development disk.

Run: python3 lab/void/desktop_check.py [--build-cache .lab/void-graphical]
The optional cache reuses only buildroot/target, never rootfs/disk/key/sockets.
Every run owns a new directory under .lab/void-desktop-checks, its own SSH port,
and Unix VNC sockets. Failure leaves logs, screenshots, and the disk in place.
"""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import shlex
import socket
import subprocess
import sys
import tempfile
import time

import compositor_guest as guest


def configure(cache=None):
    base = guest.ROOT / '.lab/void-desktop-checks'
    base.mkdir(parents=True, exist_ok=True)
    guest.WORK = Path(tempfile.mkdtemp(prefix=time.strftime('%Y%m%dT%H%M%S-'), dir=base))
    for name, path in {
        'GUEST_ROOT': 'rootfs', 'BUILD_ROOT': 'buildroot', 'TARGET': 'target',
        'DISK': 'disk.raw', 'KEY': 'id_ed25519', 'PIDFILE': 'qemu.pid',
        'QMP_SOCK': 'qmp.sock', 'SERIAL_LOG': 'serial.log',
    }.items():
        setattr(guest, name, guest.WORK / path)
    guest.VNC = [guest.WORK / f'vnc{n}.sock' for n in range(2)]
    if cache:
        cache = Path(cache).resolve(strict=True)
        if not (cache / 'buildroot/usr/bin/cargo').exists():
            raise ValueError('Build cache must contain a provisioned Void buildroot')
        guest.BUILD_ROOT, guest.TARGET = cache / 'buildroot', cache / 'target'
    with socket.socket() as free_port:
        free_port.bind(('127.0.0.1', 0))
        port = free_port.getsockname()[1]
    guest.CFG = dict(guest.CFG, ssh_port=port, vnc_port=None)
    (guest.WORK / 'run.json').write_text(json.dumps({
        'workspace': str(guest.WORK), 'ssh_port': port,
        'build_root': str(guest.BUILD_ROOT), 'target': str(guest.TARGET),
        'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=guest.ROOT, text=True).strip(),
        'dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=guest.ROOT)),
    }, indent=2) + '\n')


class Check:
    def __init__(self):
        self.results = []
        self.logfile = (guest.WORK / 'desktop.log').open('w', buffering=1)
        self.qmp = None

    def log(self, message):
        line = f'[{time.strftime("%H:%M:%S", time.gmtime())}] {message}'
        print(line, flush=True)
        self.logfile.write(line + '\n')

    def check(self, name, condition, detail=''):
        self.results.append({'name': name, 'passed': bool(condition), 'detail': detail})
        self.log(f'{"PASS" if condition else "FAIL"}: {name}' + (f' — {detail}' if detail else ''))
        if not condition:
            raise AssertionError(name)

    def run(self, command, timeout=20):
        self.log('guest$ ' + command)
        result = guest.ssh(command, check=False, timeout=timeout)
        self.log((result.stdout + result.stderr).rstrip())
        if result.returncode:
            raise RuntimeError(f'guest command exited {result.returncode}: {command}')
        return result.stdout

    def python(self, code, user=None):
        prefix = f'chpst -u {user}:{user} ' if user else ''
        return self.run(prefix + 'python3 -c ' + shlex.quote(code))

    def wait(self, name, probe, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                value = probe()
                if value:
                    return value
            except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
                pass
            time.sleep(0.5)
        raise TimeoutError(name)

    def snapshot(self):
        code = ("import socket; s=socket.socket(socket.AF_UNIX); s.settimeout(3); "
                "s.connect('/run/user/1001/crowbaros-shell/control.sock'); "
                "s.sendall(b'snapshot'); print(s.makefile('rb').read(65537).decode())")
        result = guest.ssh('chpst -u crowbar-shell:crowbar-shell python3 -c ' + shlex.quote(code),
                           check=False, timeout=6)
        return json.loads(result.stdout)

    def ready(self):
        return self.wait('shell ready', lambda: self.snapshot().get('connected'))

    def click(self, x, y):
        self.qmp.pointer(x / 1280, y / 960)
        self.qmp.click()

    def key(self, *keys):
        for key in keys:
            self.qmp.key(key, True)
        for key in reversed(keys):
            self.qmp.key(key, False)

    def capture(self, name):
        self.qmp.screendump(guest.WORK / f'{name}.png', 0)

    def preferences(self):
        result = guest.ssh('cat /home/crowbar-app/.config/crowbaros/appearance.json', check=False)
        return json.loads(result.stdout) if result.returncode == 0 else None

    def app_log(self):
        return guest.ssh('cat /var/log/crowbar-apps.log', check=False).stdout

    def diagnostics(self):
        commands = {
            'processes.txt': 'ps -eo pid,ppid,uid,gid,args; sv status /var/service/*',
            'compositor.log': 'cat /var/log/crowbar-compositor.log',
            'shell.log': 'cat /var/log/crowbar-shell.log',
            'launcher.log': 'cat /var/log/crowbar-launcher.log',
            'apps.log': 'cat /var/log/crowbar-apps.log',
            'kernel.log': 'dmesg | tail -200',
        }
        for name, command in commands.items():
            try:
                r = guest.ssh(command, check=False, timeout=6)
                (guest.WORK / name).write_text(r.stdout + r.stderr)
            except (OSError, subprocess.TimeoutExpired) as error:
                (guest.WORK / name).write_text(str(error))
        try:
            if self.qmp is None:
                self.qmp = guest.Qmp()
            self.capture('final-or-failure')
        except (OSError, RuntimeError, ValueError) as error:
            self.log(f'Could not capture final display: {error}')

    def checks(self, built):
        self.qmp = guest.Qmp()
        self.ready()
        self.check('Void Linux and runit PID 1',
                   self.run('. /etc/os-release; printf "%s\\n" "$ID"; cat /proc/1/comm').splitlines() == ['void', 'runit'])
        self.check('seatd and three Alpha services supervised',
                   all(line.startswith('run: ') for line in self.run('sv status seatd crowbar-compositor crowbar-shell crowbar-launcher').splitlines()))
        digests = self.run('sha256sum ' + ' '.join('/usr/local/bin/' + n for n in built))
        self.check('guest binaries match this build', all(f'{h}  /usr/local/bin/{n}' in digests for n, h in built.items()))
        for name in built:
            linked = self.run('ldd /usr/local/bin/' + name)
            self.check(name + ' linked libraries resolve', '=> /' in linked and 'not found' not in linked)
        self.capture('01-boot')
        self.check('fresh guest has no saved preferences', self.preferences() is None)
        self.click(640, 908)  # Terminal dock entry, through the real shell launch path.
        terminal = self.wait('terminal launched', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'foot'), None))
        self.check('Terminal launch through the shell', bool(terminal))
        self.click(728, 908)
        settings = self.wait('Settings launched', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'com.crowbaros.StudioSettings'), None))
        self.check('Settings launch through the shell', bool(settings))
        self.capture('02-settings-before')
        self.click(640, 908)
        self.wait('Terminal focus', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('dock focuses an existing app without duplicating it',
                   len([c for c in self.snapshot()['clients'] if c['class'] == 'foot']) == 1)
        self.click(210, 22)
        self.wait('workspace 2', lambda: self.snapshot()['workspace'] == 2)
        self.check('workspace switch clears focus on hidden windows', not self.snapshot()['active']['address'])
        self.click(175, 22)
        self.wait('workspace 1', lambda: self.snapshot()['workspace'] == 1)
        self.click(728, 908)
        self.wait('Settings focus', lambda: self.snapshot()['active']['address'] == settings['address'])
        # Settings initially focuses Dark. Tab, Space chooses Light, Ctrl+S saves.
        self.key('tab')
        self.key('spc')
        self.wait('Light preview', lambda: 'event=preview theme=Light' in self.app_log())
        self.check('preview has not written preferences', self.preferences() is None)
        self.key('ctrl', 's')
        saved = self.wait('Settings Save', lambda: self.preferences())
        self.check('Save persists Light appearance', saved['theme'] == 'light')
        self.wait('shell follows saved appearance', lambda: 'SHELL appearance theme=Light' in guest.ssh('cat /var/log/crowbar-shell.log', check=False).stdout)
        self.capture('03-settings-saved')
        self.key('tab')
        self.key('spc')
        self.wait('Warm Paper preview', lambda: 'event=preview theme=WarmPaper' in self.app_log())
        self.key('ctrl', 'z')
        self.wait('Undo acknowledgement', lambda: 'STUDIO event=undo' in self.app_log())
        self.check('Undo keeps saved preferences unchanged', self.preferences() == saved)
        self.key('ctrl', 'q')
        self.wait('Settings closes cleanly', lambda: not any(c['class'] == settings['class'] for c in self.snapshot()['clients']))
        self.click(728, 908)
        self.wait('Settings restores saved appearance', lambda: 'event=opened theme=Light' in self.app_log())
        self.check('Settings relaunch restores saved appearance', self.preferences() == saved)
        self.identities()
        self.socket_refusals()
        before = self.run('pgrep -x crowbaros-shell; pgrep -x foot').splitlines()
        self.run('sv restart crowbar-shell')
        self.ready()
        after = self.run('pgrep -x crowbaros-shell; pgrep -x foot').splitlines()
        self.check('shell restarts without killing the app', before[0] != after[0] and before[1:] == after[1:])
        self.check('restarted shell still controls the compositor', self.snapshot()['connected'])
        render = self.run('tail -20 /var/log/crowbar-compositor.log')
        frames = [int(value) for value in re.findall(r'frames=(\d+)', render)]
        self.check('compositor records active shell and app rendering',
                   len(frames) >= 2 and frames[-1] > frames[0]
                   and all(re.search(label + r': [1-9]\d*', render) for label in
                           ('layer crowbaros-bar', 'layer crowbaros-dock', r'com\.crowbaros\.StudioSettings')))
        boot_id = self.run('cat /proc/sys/kernel/random/boot_id').strip()
        self.run("nohup sh -c 'sleep 1; reboot' >/dev/null 2>&1 &")
        self.wait('new boot ID after clean reboot', lambda: (r := guest.ssh('cat /proc/sys/kernel/random/boot_id', check=False, timeout=6)).returncode == 0 and r.stdout.strip() != boot_id, timeout=180)
        self.ready()
        self.check('saved preferences survive clean reboot', self.preferences() == saved)
        log_offset = len(self.app_log())
        self.click(728, 908)
        self.wait('Settings restored Light after reboot', lambda: 'event=opened theme=Light' in self.app_log()[log_offset:])
        self.wait('Settings after reboot', lambda: any(c['class'] == settings['class'] for c in self.snapshot()['clients']))
        self.check('post-reboot session uses the same built binaries', self.run('sha256sum /usr/local/bin/crowbaros-studio-settings').startswith(built['crowbaros-studio-settings']))
        self.capture('04-restored-after-reboot')

    def identities(self):
        report = json.loads(self.python("""
import grp, json, os
from pathlib import Path
names = {'crowbar-compositor': 1000, 'crowbaros-shell': 1001, 'crowbaros-studio-settings': 1002, 'foot': 1002}
found = {}
seat = grp.getgrnam('_seatd')
for proc in Path('/proc').glob('[0-9]*'):
 try:
  name = Path(os.readlink(proc/'exe')).name
  status = dict(line.split(':',1) for line in (proc/'status').read_text().splitlines() if ':' in line)
  if name not in names and int(status['Uid'].split()[0]) != 1002: continue
  if name not in names: name += '(' + proc.name + ')'
  found[name] = {'uid': list(map(int,status['Uid'].split())), 'groups': list(map(int,status['Groups'].split())), 'no_new_privs': status['NoNewPrivs'].strip(), 'capabilities': status['CapEff'].strip(), 'fds': {p.name: os.readlink(p) for p in (proc/'fd').iterdir() if int(p.name)>2}, 'env': (proc/'environ').read_bytes().decode().split(chr(0))}
 except (FileNotFoundError, ProcessLookupError): pass
print(json.dumps({'processes': found, 'seat_members': seat.gr_mem, 'seat_gid': seat.gr_gid}))
"""))
        (guest.WORK / 'identities.json').write_text(json.dumps(report, indent=2) + '\n')
        found = report['processes']
        for name, uid in [('crowbar-compositor', 1000), ('crowbaros-shell', 1001), ('crowbaros-studio-settings', 1002), ('foot', 1002)]:
            self.check(name + ' has the assigned non-root identity', name in found and found[name]['uid'] == [uid] * 4)
        self.check('only compositor belongs to seatd', report['seat_members'] == ['crowbar-compositor'] and all(report['seat_gid'] not in found[n]['groups'] for n in found if n != 'crowbar-compositor'))
        trusted = {fd for n in ('crowbar-compositor', 'crowbaros-shell')
                   for fd in found[n]['fds'].values() if fd.startswith('socket:[')}
        apps = [name for name in found if found[name]['uid'][0] == 1002]
        self.check('ordinary app child processes were inspected', any('(' in name for name in apps))
        for name in apps:
            self.check(name + ' cannot gain privileges', found[name]['no_new_privs'] == '1' and int(found[name]['capabilities'], 16) == 0)
            self.check(name + ' inherited no trusted sockets or seat devices',
                       not trusted.intersection(found[name]['fds'].values())
                       and not any(fd.startswith(('/dev/input/', '/dev/dri/card', '/dev/tty')) for fd in found[name]['fds'].values()))
            self.check(name + ' inherited no privileged environment', not any(v.startswith(('CROWBAR_SHELL_CONTROL_FD=', 'WAYLAND_SOCKET=', 'LIBSEAT_BACKEND=', 'DBUS_SESSION_BUS_ADDRESS=')) for v in found[name]['env']))

    def socket_refusals(self):
        # Give the app the exact paths and temporarily allow directory traversal:
        # refusal must still come from peer credentials, not just Unix modes.
        paths = ['/run/crowbar-trusted/compositor/control', '/run/crowbar-trusted/compositor/wayland', '/run/crowbar-launch/socket', '/run/user/1001/crowbaros-shell/control.sock']
        code = """
import json, socket
results = []
for path in PATHS:
 s = socket.socket(socket.AF_UNIX); s.settimeout(2)
 try:
  s.connect(path); s.sendall(b'{"op":"snapshot"}\\n')
  data = s.recv(1024)
  results.append(not data or b'"ok": false' in data)
 except (PermissionError, ConnectionResetError, BrokenPipeError): results.append(True)
 except TimeoutError: results.append(False)
 finally: s.close()
print(json.dumps(results))
""".replace('PATHS', repr(paths))
        directories = ['/run/crowbar-trusted/compositor', '/run/crowbar-launch', '/run/user/1001', '/run/user/1001/crowbaros-shell']
        modes = json.loads(self.python('import os,json; print(json.dumps({p:os.stat(p).st_mode & 0o7777 for p in ' + repr(paths + directories) + '}))'))
        try:
            self.run('chmod o+x ' + ' '.join(directories) + '; chmod o+rw ' + ' '.join(paths))
            refused = json.loads(self.python(code, 'crowbar-app'))
            self.check('trusted sockets reject the app UID even with paths exposed', refused == [True] * len(paths))
        finally:
            self.python('import os; modes=' + repr(modes) + '; [os.chmod(p,m) for p,m in modes.items()]')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build-cache', type=Path)
    args = parser.parse_args()
    configure(args.build_cache)
    check = Check()
    check.log(f'Evidence and new guest disk: {guest.WORK}')
    try:
        with (guest.WORK / 'build.log').open('w') as log:
            # subprocess output is sent explicitly; Python progress is captured too.
            original_run = guest.run
            def logged_run(*arguments, **kwargs):
                kwargs.setdefault('stdout', log)
                kwargs.setdefault('stderr', subprocess.STDOUT)
                return original_run(*arguments, **kwargs)
            guest.run = logged_run
            with contextlib.redirect_stdout(log):
                guest.rootfs()
                guest.build()
                guest.image(desktop=True)
            guest.run = original_run
        built = {name: guest.void_lab.sha256(guest.TARGET / 'release' / name) for name in guest.BINARIES}
        guest.start()
        guest.wait_ssh()
        check.checks(built)
    except Exception as error:
        if not check.results or check.results[-1]['passed']:
            check.results.append({'name': 'smoke run completed', 'passed': False, 'detail': repr(error)})
        check.log(f'FAIL: {error!r}')
    finally:
        if guest.running():
            check.diagnostics()
            guest.stop()
        failed = sum(not row['passed'] for row in check.results)
        (guest.WORK / 'results.json').write_text(json.dumps(check.results, indent=2) + '\n')
        check.log(f'RESULT: {len(check.results)-failed} passed, {failed} failed')
        check.log(f'Evidence: {guest.WORK}')
        check.logfile.close()
    return int(failed != 0)


if __name__ == '__main__':
    raise SystemExit(main())
