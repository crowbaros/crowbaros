#!/usr/bin/env python3
"""The desktop check: Base's image, booted under UEFI, driven as a person would.

Run: python3 lab/void/compositor_guest.py desktop-check
It builds the image with Base's one command, `base/void_base.py build`, which
keeps its Void roots and Cargo target in .lab/base as a cache. Every run then
owns a new directory under .lab/void-desktop-checks, with its own copy of the
disk, firmware variables, SSH port, and Unix VNC and QMP sockets. It boots that
copy through the firmware and GRUB, as a person's machine would, and never
touches Base's own disk or the lab's. Failure leaves logs, screenshots, and the
disk in place.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time

import compositor_guest as guest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'base'))
import void_base  # noqa: E402


def configure():
    base = guest.ROOT / '.lab/void-desktop-checks'
    base.mkdir(parents=True, exist_ok=True)
    guest.WORK = Path(tempfile.mkdtemp(prefix=time.strftime('%Y%m%dT%H%M%S-'), dir=base))
    for name, path in {
        'DISK': 'disk.raw', 'PIDFILE': 'qemu.pid', 'QMP_SOCK': 'qmp.sock', 'SERIAL_LOG': 'serial.log',
    }.items():
        setattr(guest, name, guest.WORK / path)
    guest.VNC = [guest.WORK / f'vnc{n}.sock' for n in range(2)]
    # The key Base's build put in the image for root.
    guest.KEY = void_base.KEY
    with socket.socket() as free_port:
        free_port.bind(('127.0.0.1', 0))
        port = free_port.getsockname()[1]
    guest.CFG = dict(guest.CFG, ssh_port=port, vnc_port=None)
    (guest.WORK / 'run.json').write_text(json.dumps({
        'workspace': str(guest.WORK), 'ssh_port': port, 'image': str(void_base.BASE_DISK),
        'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=guest.ROOT, text=True).strip(),
        'dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=guest.ROOT)),
    }, indent=2) + '\n')


def start():
    """Boots this run's own copy of Base's image through the firmware."""
    for path in (guest.QMP_SOCK, guest.PIDFILE, *guest.VNC):
        path.unlink(missing_ok=True)
    subprocess.run(['cp', '--reflink=auto', '--sparse=always', void_base.BASE_DISK, guest.DISK],
                   check=True)
    nvram = guest.WORK / 'nvram.fd'
    shutil.copyfile(void_base.CFG['firmware_vars'], nvram)
    guest.run(*void_base.qemu('crowbaros-desktop-check', guest.DISK, nvram, guest.CFG['ssh_port']),
              # A VNC server per connector. A VNC client that sets a desktop
              # size on the second one plugs the second monitor in.
              '-vnc', f'unix:{guest.VNC[0]},display=gpu0,head=0',
              '-vnc', f'unix:{guest.VNC[1]},display=gpu0,head=1',
              '-qmp', f'unix:{guest.QMP_SOCK},server=on,wait=off',
              '-chardev', f'file,id=ser0,path={guest.SERIAL_LOG}', '-serial', 'chardev:ser0',
              '-display', 'none', '-daemonize', '-pidfile', guest.PIDFILE)


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

    def type(self, text):
        for letter in text:
            self.key(letter)

    def shell_log(self):
        return guest.ssh('cat /var/log/crowbar-shell.log', check=False).stdout

    def shell_unlock(self):
        """The compositor's reply when the shell asks it to unlock."""
        code = ("import socket; s=socket.socket(socket.AF_UNIX); s.settimeout(3); "
                "s.connect('/run/crowbar-trusted/compositor/control'); "
                "s.sendall(b'{\"op\":\"unlock\"}\\n'); print(s.makefile('rb').readline().decode())")
        return self.python(code, 'crowbar-shell')

    def unlock(self, password='crowbar'):
        """The person's password, typed with nothing first, as the lock screen
        sends it to the unlock helper."""
        self.type(password)
        self.key('ret')

    def capture(self, name):
        self.qmp.screendump(guest.WORK / f'{name}.png', 0)

    def person(self):
        """The person's account name and home: every app runs as it."""
        if not hasattr(self, '_person'):
            account = self.run(f'getent passwd {void_base.PERSON_UID}').split(':')
            self._person = account[0], account[5]
        return self._person

    def as_app(self, command, out):
        """Starts a command as an app would run: as the person, with their
        groups, at the ordinary Wayland socket, with its output in `out`."""
        person, home = self.person()
        self.run(f'cd {home} && setsid -f chpst -u "{person}:$(id -Gn {person} | tr " " :)" '
                 f'env -i HOME={home} PATH=/usr/local/bin:/usr/bin:/bin '
                 f'XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} WAYLAND_DISPLAY=wayland-0 '
                 f'{command} >{out} 2>&1 </dev/null')

    def button(self, down):
        self.qmp.command('input-send-event', events=[{'type': 'btn', 'data': {'down': down, 'button': 'left'}}])
        time.sleep(0.15)

    def preferences(self):
        result = guest.ssh(f'cat {self.person()[1]}/.config/crowbaros/appearance.json', check=False)
        return json.loads(result.stdout) if result.returncode == 0 else None

    def app_log(self):
        return guest.ssh('cat /var/log/crowbar-apps.log', check=False).stdout

    def diagnostics(self):
        commands = {
            'processes.txt': 'ps -eo pid,ppid,uid,gid,args; sv status /var/service/*',
            'compositor.log': 'cat /var/log/crowbar-compositor.log',
            'shell.log': 'cat /var/log/crowbar-shell.log',
            'launcher.log': 'cat /var/log/crowbar-launcher.log',
            'unlock.log': 'cat /var/log/crowbar-unlock.log',
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
        self.check('seatd and four Alpha services supervised',
                   all(line.startswith('run: ') for line in self.run('sv status seatd crowbar-compositor crowbar-shell crowbar-launcher crowbar-unlock').splitlines()))
        digests = self.run('sha256sum ' + ' '.join('/usr/local/bin/' + n for n in built))
        self.check('guest binaries match this build', all(f'{h}  /usr/local/bin/{n}' in digests for n, h in built.items()))
        for name in built:
            linked = self.run('ldd /usr/local/bin/' + name)
            self.check(name + ' linked libraries resolve', '=> /' in linked and 'not found' not in linked)
        shell_log = self.shell_log
        # The session starts locked, and only the unlock helper's word, on the
        # person's password, unlocks it. The lock screen takes the password
        # at once, with no chord first.
        self.wait('the session starts locked', lambda: self.snapshot().get('locked') and 'SHELL lock open=true' in shell_log())
        self.capture('00-locked-at-boot')
        self.check('the session starts locked, behind the lock screen',
                   self.snapshot()['locked'] and 'SHELL lock open=true' in shell_log())
        answered = shell_log().count('SHELL unlock ok=false')
        self.unlock('wrong')
        self.wait('a wrong password is answered', lambda: shell_log().count('SHELL unlock ok=false') > answered)
        self.capture('00-wrong-password')
        self.check('the lock screen takes a password with no chord first, and a wrong one leaves the session locked',
                   self.snapshot()['locked'])
        refused = self.shell_unlock()
        self.check("the shell's own word does not unlock", '"reply":"error"' in refused and self.snapshot()['locked'], refused.strip())
        self.unlock()
        self.wait('the password unlocks', lambda: not self.snapshot().get('locked') and 'SHELL lock open=false' in shell_log())
        self.check("the person's password unlocks the session", 'SHELL unlock ok=true' in shell_log())
        self.capture('01-boot')
        self.check('fresh guest has no saved preferences', self.preferences() is None)
        self.click(660, 935)  # Terminal dock entry (Applications · Chromium · Terminal · Settings).
        terminal = self.wait('terminal launched', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'foot'), None))
        self.check('Terminal launch through the shell', bool(terminal))
        self.wait('launched window focus', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('a window launched from the shell takes the keyboard',
                   self.snapshot()['active']['address'] == terminal['address'])
        self.key('meta_l')
        self.wait('Super opens the launcher', lambda: 'SHELL launcher open=true' in shell_log())
        self.capture('01-launcher')
        self.key('meta_l')
        self.wait('Super closes the launcher', lambda: 'SHELL launcher open=false' in shell_log())
        self.check('Super tapped alone opens and closes the launcher',
                   'SHELL launcher open=true' in shell_log() and 'SHELL launcher open=false' in shell_log())
        self.key('meta_l', 'l')
        self.wait('Super+L locks', lambda: self.snapshot().get('locked') and 'SHELL lock open=true' in shell_log())
        self.capture('01-locked')
        self.check('Super+L locks the session and shows the lock screen',
                   self.snapshot()['locked'] and 'SHELL lock open=true' in shell_log())
        self.key('ctrl', 'alt', 'ret')
        time.sleep(1)
        self.check('Ctrl+Alt+Enter does not unlock', self.snapshot()['locked'])
        self.unlock()
        self.wait('the password unlocks', lambda: not self.snapshot().get('locked'))
        self.check('the lock screen unlocks, and the window has the keyboard back',
                   self.snapshot()['active']['address'] == terminal['address'])
        self.key('ctrl', 'alt', 'ret')
        time.sleep(1)
        self.check('Ctrl+Alt+Enter does not lock', not self.snapshot()['locked'])
        self.click(1232, 12)  # The bar's lock button.
        self.wait("the bar's lock button locks", lambda: self.snapshot().get('locked'))
        self.check("the bar's lock button locks the session", self.snapshot()['locked'])
        self.unlock()
        self.wait('the password unlocks after the lock button', lambda: not self.snapshot().get('locked'))
        self.run('/etc/zzz.d/suspend/50-crowbar-lock')
        self.wait('the suspend hook locks', lambda: self.snapshot().get('locked'))
        self.check('the suspend hook locks the session before the machine sleeps', int(self.run("grep -c 'session locked by=authority' /var/log/crowbar-compositor.log || true").strip() or 0) > 0)
        self.unlock()
        self.wait('the password unlocks after the suspend hook', lambda: not self.snapshot().get('locked'))
        self.click(700, 935)
        settings = self.wait('Settings launched', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'com.crowbaros.StudioSettings'), None))
        self.check('Settings launch through the shell', bool(settings))
        self.capture('02-settings-before')
        self.click(660, 935)
        self.wait('Terminal focus', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('dock focuses an existing app without duplicating it',
                   len([c for c in self.snapshot()['clients'] if c['class'] == 'foot']) == 1)
        self.click(80, 12)
        self.wait('workspace 2', lambda: self.snapshot()['workspace'] == 2)
        self.check('workspace switch clears focus on hidden windows', not self.snapshot()['active']['address'])
        self.click(60, 12)
        self.wait('workspace 1', lambda: self.snapshot()['workspace'] == 1)
        self.click(700, 935)
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
        self.click(700, 935)
        self.wait('Settings restores saved appearance', lambda: 'event=opened theme=Light' in self.app_log())
        self.check('Settings relaunch restores saved appearance', self.preferences() == saved)
        self.identities()
        self.socket_refusals()
        self.display_bridge()
        self.between_apps()
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
        # A file the person writes in their home, to find after the reboot.
        person, home = self.person()
        kept = f'{home}/kept-across-reboot'
        token = os.urandom(8).hex()
        self.run(f'chpst -u {person}:{person} sh -c {shlex.quote(f"echo {token} > {kept}")}')
        self.check("the person writes a file in their home",
                   self.run(f"stat -c %U {kept}").strip() == person)
        followed = self.shell_log().count('SHELL appearance theme=Light')
        boot_id = self.run('cat /proc/sys/kernel/random/boot_id').strip()
        self.run("nohup sh -c 'sleep 1; reboot' >/dev/null 2>&1 &")
        self.wait('new boot ID after clean reboot', lambda: (r := guest.ssh('cat /proc/sys/kernel/random/boot_id', check=False, timeout=6)).returncode == 0 and r.stdout.strip() != boot_id, timeout=180)
        self.ready()
        self.wait('the session starts locked after reboot', lambda: self.snapshot().get('locked'))
        self.unlock()
        self.wait('the password unlocks after reboot', lambda: not self.snapshot().get('locked'))
        self.check('saved preferences survive clean reboot', self.preferences() == saved)
        self.wait('the shell follows the saved appearance after the reboot',
                  lambda: self.shell_log().count('SHELL appearance theme=Light') > followed)
        self.check('the shell follows the saved appearance after the reboot', True)
        self.check("the file in the person's home survives the reboot",
                   self.run(f'cat {kept}').strip() == token)
        log_offset = len(self.app_log())
        self.click(700, 935)
        self.wait('Settings restored Light after reboot', lambda: 'event=opened theme=Light' in self.app_log()[log_offset:])
        self.wait('Settings after reboot', lambda: any(c['class'] == settings['class'] for c in self.snapshot()['clients']))
        self.check('post-reboot session uses the same built binaries', self.run('sha256sum /usr/local/bin/crowbaros-studio-settings').startswith(built['crowbaros-studio-settings']))
        self.capture('04-restored-after-reboot')
        # Open the Displays page in Studio Settings (sidebar button 2 at x=136, y=204)
        self.click(136, 204)
        self.wait('Settings Displays page loads outputs', lambda: 'STUDIO event=displays-loaded count=' in self.app_log()[log_offset:])
        self.check('Settings Displays page loads live outputs from Fulcrum',
                   bool(re.search(r'STUDIO event=displays-loaded count=[1-9]', self.app_log()[log_offset:])))
        self.capture('05-settings-displays-light')
        # Preview Dark appearance, then view Displays page in Dark theme too
        self.click(136, 156)
        self.click(320, 350)
        self.wait('Dark preview before Displays capture', lambda: 'event=preview theme=Dark' in self.app_log()[log_offset:])
        self.click(136, 204)
        self.capture('06-settings-displays-dark')
        # Verify Chromium is configured as the default browser and launches natively from the dock
        person = self.person()[0]
        default_browser = self.run(f"su -s /bin/sh {person} -c 'xdg-settings get default-web-browser'").strip()
        https_handler = self.run(f"su -s /bin/sh {person} -c 'xdg-mime query default x-scheme-handler/https'").strip()
        self.check('Chromium is configured as the default browser',
                   default_browser == 'chromium.desktop' and https_handler == 'chromium.desktop',
                   f'browser={default_browser!r} https={https_handler!r}')
        self.click(620, 935)  # Chromium dock entry
        browser = self.wait(
            'Chromium launched through the shell',
            lambda: next((c for c in self.snapshot()['clients'] if 'chromium' in c['class'].lower()), None),
            timeout=30,
        )
        self.check('Chromium launches natively on Fulcrum from the dock', bool(browser))
        self.wait('Chromium takes focus', lambda: self.snapshot()['active']['address'] == browser['address'], timeout=15)
        time.sleep(1.5)
        self.capture('07-chromium')

    def identities(self):
        report = json.loads(self.python("""
import grp, json, os
from pathlib import Path
names = {'crowbar-compositor': 1000, 'crowbaros-shell': 1001, 'crowbaros-studio-settings': PERSON, 'foot': PERSON}
found = {}
seat = grp.getgrnam('_seatd')
for proc in Path('/proc').glob('[0-9]*'):
 try:
  name = Path(os.readlink(proc/'exe')).name
  status = dict(line.split(':',1) for line in (proc/'status').read_text().splitlines() if ':' in line)
  if name not in names and int(status['Uid'].split()[0]) != PERSON: continue
  if name not in names: name += '(' + proc.name + ')'
  found[name] = {'uid': list(map(int,status['Uid'].split())), 'groups': list(map(int,status['Groups'].split())), 'no_new_privs': status['NoNewPrivs'].strip(), 'capabilities': status['CapEff'].strip(), 'fds': {p.name: os.readlink(p) for p in (proc/'fd').iterdir() if int(p.name)>2}, 'env': (proc/'environ').read_bytes().decode().split(chr(0))}
 except (FileNotFoundError, ProcessLookupError): pass
print(json.dumps({'processes': found, 'seat_members': seat.gr_mem, 'seat_gid': seat.gr_gid, 'netdev_gid': grp.getgrnam('netdev').gr_gid}))
""".replace('PERSON', str(void_base.PERSON_UID))))
        (guest.WORK / 'identities.json').write_text(json.dumps(report, indent=2) + '\n')
        found = report['processes']
        person = void_base.PERSON_UID
        for name, uid in [('crowbar-compositor', 1000), ('crowbaros-shell', 1001)]:
            self.check(name + ' has the assigned non-root identity', name in found and found[name]['uid'] == [uid] * 4)
        for name in ('foot', 'crowbaros-studio-settings'):
            self.check(name + " runs as the person's account", name in found and found[name]['uid'] == [person] * 4)
            self.check(name + " is at the ordinary Wayland socket, in the person's runtime directory",
                       {f'XDG_RUNTIME_DIR=/run/user/{person}', 'WAYLAND_DISPLAY=wayland-0'} <= set(found[name]['env']))
        self.check("the person's terminal may join Wi-Fi, in netdev", report['netdev_gid'] in found['foot']['groups'])
        launcher = self.run("for p in $(pgrep -f '^/usr/bin/python3 /usr/local/bin/crowbar-launcher'); do grep -E '^(Uid|Gid):' /proc/$p/status; done")
        self.check('the launcher runs as the person, not root',
                   bool(launcher) and all(line.split()[1:] == [str(person)] * 4 for line in launcher.splitlines() if line.startswith('Uid:')))
        owners = self.run('stat -c "%U %G %a" /run/crowbar-launch /run/crowbar-launch/socket').split('\n')
        self.check('root owns the launch socket and its directory, so no app can take its place',
                   owners[0] == 'root crowbar-shell 750' and owners[1].startswith('root crowbar-shell'), ' | '.join(owners))
        self.check('only compositor belongs to seatd', report['seat_members'] == ['crowbar-compositor'] and all(report['seat_gid'] not in found[n]['groups'] for n in found if n != 'crowbar-compositor'))
        trusted = {fd for n in ('crowbar-compositor', 'crowbaros-shell')
                   for fd in found[n]['fds'].values() if fd.startswith('socket:[')}
        apps = [name for name in found if found[name]['uid'][0] == person]
        self.check('ordinary app child processes were inspected', any('(' in name for name in apps))
        for name in apps:
            self.check(name + ' inherited no trusted sockets or seat devices',
                       not trusted.intersection(found[name]['fds'].values())
                       and not any(fd.startswith(('/dev/input/', '/dev/dri/card', '/dev/tty')) for fd in found[name]['fds'].values()))
            self.check(name + ' inherited no privileged environment', not any(v.startswith(('CROWBAR_SHELL_CONTROL_FD=', 'WAYLAND_SOCKET=', 'LIBSEAT_BACKEND=')) for v in found[name]['env']))

    def socket_refusals(self):
        # Give the app the exact paths and temporarily allow directory traversal:
        # refusal must still come from peer credentials, not just Unix modes.
        paths = ['/run/crowbar-trusted/compositor/control', '/run/crowbar-trusted/compositor/wayland', '/run/crowbar-launch/socket', '/run/user/1001/crowbaros-shell/control.sock',
                 '/run/crowbar-unlock/socket', '/run/crowbar-trusted/unlock/compositor']
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
        directories = ['/run/crowbar-trusted/compositor', '/run/crowbar-launch', '/run/user/1001', '/run/user/1001/crowbaros-shell',
                       '/run/crowbar-unlock', '/run/crowbar-trusted/unlock']
        modes = json.loads(self.python('import os,json; print(json.dumps({p:os.stat(p).st_mode & 0o7777 for p in ' + repr(paths + directories) + '}))'))
        try:
            self.run('chmod o+x ' + ' '.join(directories) + '; chmod o+rw ' + ' '.join(paths))
            refused = json.loads(self.python(code, self.person()[0]))
            self.check("the shell's sockets, the control channel, the launcher, and the unlock helper refuse an app, even with paths exposed",
                       refused == [True] * len(paths))
        finally:
            self.python('import os; modes=' + repr(modes) + '; [os.chmod(p,m) for p,m in modes.items()]')
        # The apps' socket: an app is offered what a Wayland app expects, the
        # GPU's buffers included, and not layer shell; any other account,
        # even the shell's, is refused there.
        out = '/tmp/app-globals.out'
        self.as_app('crowbar-probe globals', out)
        offered = self.wait('the app lists its globals', lambda: (r := self.run(f'cat {out}')) and 'wl_compositor' in r and r, timeout=10)
        self.check("an app at the person's socket is offered the GPU's buffers and not layer shell",
                   'zwp_linux_dmabuf_v1' in offered and 'xdg_wm_base' in offered and 'zwlr_layer_shell_v1' not in offered)
        apps = '/run/crowbar-apps'
        modes = json.loads(self.python(f'import os,json; print(json.dumps({{p:os.stat(p).st_mode & 0o7777 for p in ["{apps}", "{apps}/wayland-0"]}}))'))
        try:
            self.run(f'chmod o+x {apps}; chmod o+rw {apps}/wayland-0')
            other = self.run(f'chpst -u crowbar-shell:crowbar-shell env -i WAYLAND_DISPLAY={apps}/wayland-0 '
                             'timeout 5 /usr/local/bin/crowbar-probe globals 2>&1; echo exit=$?')
        finally:
            self.python('import os; modes=' + repr(modes) + '; [os.chmod(p,m) for p,m in modes.items()]')
        # The probe ran, reached the socket, and was turned away: not a probe
        # that could not start (127) or a socket it could not open.
        status = int(other.rsplit('exit=', 1)[1])
        self.check("the person's socket refuses another account, even with its path exposed",
                   'wl_compositor' not in other and status not in (0, 126, 127)
                   and 'No such file' not in other and 'ermission denied' not in other, other.strip())

    def probe_window(self, command, app_id, out):
        """Opens one of the probe's maximized windows as an app. A new window
        takes the keyboard, as on any desktop."""
        self.as_app(command, out)
        window = self.wait(app_id + ' maps', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == app_id), None))
        self.wait(app_id + ' takes the keyboard', lambda: self.snapshot()['active']['address'] == window['address'])
        return window

    def between_apps(self):
        """Copy and paste, the primary selection, and drag and drop, between
        two of the person's apps, on a workspace of their own."""
        self.click(100, 12)
        self.wait('workspace 3', lambda: self.snapshot()['workspace'] == 3)
        token = os.urandom(6).hex()
        output = lambda name: guest.ssh(f'cat /tmp/{name}.out', check=False).stdout
        try:
            self.probe_window(f'crowbar-probe copy copied-{token}', 'org.crowbaros.ProbeCopy', '/tmp/copy.out')
            self.check('a new window an app opens takes the keyboard, with no click', True)
            self.wait('the first app copies', lambda: 'copied' in output('copy'))
            self.probe_window('crowbar-probe paste', 'org.crowbaros.ProbePaste', '/tmp/paste.out')
            pasted = self.wait('the second app pastes',
                               lambda: (o := output('paste')) and f'pasted clipboard=copied-{token}' in o and f'pasted primary=copied-{token}' in o and o)
            self.capture('08-pasted')
            self.check('what one app copies, another pastes', f'pasted clipboard=copied-{token}' in pasted)
            self.check("one app's primary selection pastes in another", f'pasted primary=copied-{token}' in pasted)
            self.run("pkill -f 'crowbar-probe (copy|paste)'; true")
            self.probe_window(f'crowbar-probe drag dragged-{token}', 'org.crowbaros.ProbeDrag', '/tmp/drag.out')
            self.key('meta_l', 'left')
            self.probe_window('crowbar-probe drop', 'org.crowbaros.ProbeDrop', '/tmp/drop.out')
            self.key('meta_l', 'right')
            time.sleep(0.5)
            self.capture('09-drag-and-drop')
            self.qmp.pointer(320 / 1280, 480 / 960)
            self.button(True)
            for x in range(400, 1000, 80):
                self.qmp.pointer(x / 1280, 480 / 960)
            self.button(False)
            self.wait('the drop', lambda: f'dropped dragged-{token}' in output('drop'))
            self.check('what the person drags from one app drops on another',
                       'drag started' in output('drag') and f'dropped dragged-{token}' in output('drop'))
        finally:
            self.run("pkill -f 'crowbar-probe (copy|paste|drag|drop)'; true")
        self.click(60, 12)
        self.wait('workspace 1', lambda: self.snapshot()['workspace'] == 1)

    def display_bridge(self):
        code = """
import json, socket
def ask(req):
 s = socket.socket(socket.AF_UNIX); s.settimeout(3)
 try:
  s.connect('/run/crowbar-displays/socket')
  s.sendall((json.dumps(req) + '\\n').encode())
  buf = b''
  while not buf.endswith(b'\\n'):
   chunk = s.recv(4096)
   if not chunk: break
   buf += chunk
  return json.loads(buf.decode())
 finally:
  s.close()
snap = ask({'op': 'snapshot'})
lock = ask({'op': 'lock'})
before = ask({'op': 'outputs'})
name = before['outputs'][0]['name']
orig_scale = before['outputs'][0]['scale']
scaled = ask({'op': 'configure_output', 'name': name, 'scale': 150})
mid = ask({'op': 'outputs'})
restored = ask({'op': 'configure_output', 'name': name, 'scale': orig_scale})
after = ask({'op': 'outputs'})
print(json.dumps({
 'snap_refused': snap.get('reply') == 'error',
 'lock_refused': lock.get('reply') == 'error',
 'has_outputs': before.get('reply') == 'outputs' and len(before.get('outputs', [])) >= 1,
 'scaled_ok': scaled.get('reply') == 'ok' and mid['outputs'][0]['scale'] == 150,
 'restored_ok': restored.get('reply') == 'ok' and after['outputs'][0]['scale'] == orig_scale,
}))
"""
        report = json.loads(self.python(code, self.person()[0]))
        self.check("display bridge refuses non-display compositor ops from the person's apps",
                   report['snap_refused'] and report['lock_refused'])
        self.check('display bridge serves outputs and applies scale changes',
                   report['has_outputs'] and report['scaled_ok'] and report['restored_ok'])
        layout = self.run('cat /home/crowbar-compositor/layout || true')
        self.check('compositor persists output placement to layout file',
                   bool(layout.strip()) and 'scale' in layout)


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.parse_args()
    # What building and booting the image need, before the build spends its time.
    void_base.preflight(vm=True, build=True)
    configure()
    check = Check()
    check.log(f'Evidence and new guest disk: {guest.WORK}')
    try:
        build_log = guest.WORK / 'build.log'
        check.log(f'Building the image with Base\'s build; its output goes to {build_log}')
        with build_log.open('w') as log:
            if subprocess.run([sys.executable, void_base.HERE / 'void_base.py', 'build', '--lab-account'],
                              stdout=log, stderr=subprocess.STDOUT).returncode:
                raise RuntimeError(f'Base\'s build failed; see {build_log}')
        built = {name: void_base.sha256(path) for name, path in void_base.binaries().items()}
        start()
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
