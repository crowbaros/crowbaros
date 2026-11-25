#!/usr/bin/env python3
"""The desktop check: Base's image, booted under UEFI, driven as a person would.

Run: lab/void/desktop_check.py
It builds the image with Base's one command, `base/void_base.py build
--lab-account`, which keeps its Void roots and Cargo target in .lab/base as a
cache. Every run then owns a new directory under .lab/void-desktop-checks,
with its own copy of the disk, firmware variables, SSH port, and Unix VNC and
QMP sockets. It boots that copy through the firmware and GRUB, as a person's
machine would, runs Base's boot checks and the desktop checks, and never
touches Base's own disk. Failure leaves logs, screenshots, and the disk in
place.
"""
import argparse
import json
import os
import re
import shlex
import subprocess
import time

import guest

void_base = guest.void_base


class Check:
    def __init__(self, vm):
        self.guest = vm
        self.work = vm.work
        self.results = []
        self.logfile = (self.work / 'desktop.log').open('w', buffering=1)
        self.qmp = None

    def log(self, message):
        line = f'[{time.strftime("%H:%M:%S", time.gmtime())}] {message}'
        print(line, flush=True)
        self.logfile.write(line + '\n')

    def record(self, name, condition, detail=''):
        detail = str(detail) if detail else ''
        self.results.append({'name': name, 'passed': bool(condition), 'detail': detail})
        self.log(f'{"PASS" if condition else "FAIL"}: {name}' + (f' — {detail}' if detail else ''))
        return bool(condition)

    def check(self, name, condition, detail=''):
        if not self.record(name, condition, detail):
            raise AssertionError(name)

    def run(self, command, timeout=20):
        self.log('guest$ ' + command)
        result = self.guest.ssh(command, check=False, timeout=timeout)
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
        result = self.guest.ssh('chpst -u crowbar-shell:crowbar-shell python3 -c ' + shlex.quote(code),
                                check=False, timeout=6)
        return json.loads(result.stdout)

    def agent_view(self):
        """The shell's view of the Agent drawer and mark, read from the
        shell's own control socket as `crowbar-shell`."""
        code = ("import socket; s=socket.socket(socket.AF_UNIX); s.settimeout(3); "
                "s.connect('/run/user/1001/crowbaros-shell/control.sock'); "
                "s.sendall(b'agent'); print(s.makefile('rb').read(1048577).decode())")
        result = self.guest.ssh('chpst -u crowbar-shell:crowbar-shell python3 -c ' + shlex.quote(code),
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
        for seq in qmp_keys_for_text(text):
            self.key(*seq)

    def shell_log(self):
        return self.guest.ssh('cat /var/log/crowbar-shell.log', check=False).stdout

    def shell_unlock(self):
        """The compositor's reply when the shell asks it to unlock."""
        return self.run('chpst -u crowbar-shell:crowbar-shell '
                        + guest.ask_fulcrum('/run/crowbar-trusted/compositor/control', '{"op":"unlock"}'))

    def unlock(self, password='crowbar'):
        """The person's password, typed with nothing first, as the lock screen
        sends it to the unlock helper."""
        self.type(password)
        self.key('ret')

    def capture(self, name, head=0):
        self.qmp.screendump(self.work / f'{name}.png', head)

    def pixel(self, name, x, y, head=0):
        """Captures a display as `name`, and returns the colour of one of its
        pixels: red, green, and blue."""
        self.capture(name, head)
        width, height, rows = guest.png_pixels(self.work / f'{name}.png')
        if not (0 <= x < width and 0 <= y < height):
            raise ValueError(f'{name}.png is {width} by {height}, and has no pixel at {x},{y}')
        return tuple(rows[y][3 * x:3 * x + 3])

    def count(self, text, log='/var/log/crowbar-compositor.log'):
        """How many lines of a log in the guest hold `text`."""
        result = self.guest.ssh(f'grep -c -e {shlex.quote(text)} {log}', check=False)
        return int(result.stdout.strip() or 0)

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
        result = self.guest.ssh(f'cat {self.person()[1]}/.config/crowbaros/appearance.json', check=False)
        return json.loads(result.stdout) if result.returncode == 0 else None

    def app_log(self):
        return self.guest.ssh('cat /var/log/crowbar-apps.log', check=False).stdout

    def compositor_log(self):
        return self.run('cat /var/log/crowbar-compositor.log')

    def base_checks(self):
        """Run every Base boot check from `void_base.boot_checks()` and report each."""
        failed = []
        for name, user, command, ok in void_base.boot_checks():
            self.log(f'guest({user})$ {command}' if user != 'root' else f'guest$ {command}')
            result = self.guest.ssh(command, check=False, timeout=120, user=user)
            output = (result.stdout + result.stderr).rstrip()
            if output:
                self.log(output)
            passed = result.returncode == 0 and ok(result.stdout)
            detail = '' if passed else (f'exit {result.returncode}' if result.returncode else output)
            if not self.record(name, passed, detail):
                failed.append(name)
        if failed:
            raise AssertionError('Base boot checks failed: ' + ', '.join(failed))

    def diagnostics(self):
        commands = {
            'processes.txt': 'ps -eo pid,ppid,uid,gid,args; sv status /var/service/*',
            'compositor.log': 'cat /var/log/crowbar-compositor.log',
            'shell.log': 'cat /var/log/crowbar-shell.log',
            'launcher.log': 'cat /var/log/crowbar-launcher.log',
            'unlock.log': 'cat /var/log/crowbar-unlock.log',
            'lever.log': 'cat /var/log/crowbar-lever.log',
            'apps.log': 'cat /var/log/crowbar-apps.log',
            'kernel.log': 'dmesg | tail -200',
        }
        for name, command in commands.items():
            try:
                r = self.guest.ssh(command, check=False, timeout=6)
                (self.work / name).write_text(r.stdout + r.stderr)
            except (OSError, subprocess.TimeoutExpired) as error:
                (self.work / name).write_text(str(error))
        try:
            if self.qmp is None:
                self.qmp = self.guest.qmp()
            self.capture('final-or-failure')
        except (OSError, RuntimeError, ValueError) as error:
            self.log(f'Could not capture final display: {error}')

    def checks(self, built):
        self.qmp = self.guest.qmp()
        self.ready()
        shell_log = self.shell_log
        # The session starts locked, and only the unlock helper's word, on the
        # person's password, unlocks it. The lock screen takes the password
        # at once, with no chord first. Seen before Base's checks, which
        # restart the compositor's service.
        self.wait('the session starts locked', lambda: self.snapshot().get('locked') and 'SHELL lock open=true' in shell_log())
        self.capture('00-locked-at-boot')
        self.check('the session starts locked, behind the lock screen',
                   self.snapshot()['locked'] and 'SHELL lock open=true' in shell_log())
        self.still('a still desktop, locked at boot, commits nothing and draws nothing')
        locks = shell_log().count('SHELL lock open=true')
        self.base_checks()
        self.wait("the shell's lock screen is back after Base's checks restart the compositor",
                  lambda: shell_log().count('SHELL lock open=true') > locks
                  and self.snapshot().get('connected') and self.snapshot().get('locked'), timeout=60)
        self.check('seatd, bluetoothd, and six desktop services supervised',
                   all(line.startswith('run: ') for line in self.run('sv status seatd bluetoothd crowbar-compositor crowbar-shell crowbar-launcher crowbar-unlock crowbar-lever crowbar-pipewire').splitlines()))
        digests = self.run('sha256sum ' + ' '.join('/usr/local/bin/' + n for n in built))
        self.check('guest binaries match this build', all(f'{h}  /usr/local/bin/{n}' in digests for n, h in built.items()))
        for name in built:
            linked = self.run('ldd /usr/local/bin/' + name)
            self.check(name + ' linked libraries resolve', '=> /' in linked and 'not found' not in linked)
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
        self.click(680, 935)  # Terminal dock entry (Applications · Files · Chromium · Terminal · Settings).
        terminal = self.wait('terminal launched', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'foot'), None))
        self.check('Terminal launch through the shell', bool(terminal))
        self.wait('launched window focus', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('a window launched from the shell takes the keyboard',
                   self.snapshot()['active']['address'] == terminal['address'])
        self.key('meta_l')
        self.wait('Super opens the launcher', lambda: 'SHELL launcher open=true' in shell_log())
        self.wait('the launcher takes the keyboard without a click', lambda: not self.snapshot()['active']['address'])
        self.capture('01-launcher')
        self.key('meta_l')
        self.wait('Super closes the launcher', lambda: 'SHELL launcher open=false' in shell_log())
        self.check('Super tapped alone opens and closes the launcher',
                   'SHELL launcher open=true' in shell_log() and 'SHELL launcher open=false' in shell_log())
        self.wait('closing the launcher restores Terminal focus',
                  lambda: self.snapshot()['active']['address'] == terminal['address'])
        launcher_opens = shell_log().count('SHELL launcher open=true')
        self.key('meta_l')
        self.wait('Super reopens the launcher', lambda: shell_log().count('SHELL launcher open=true') > launcher_opens)
        self.wait('the launcher takes the keyboard without a click', lambda: not self.snapshot()['active']['address'])
        self.type('studiosettings')
        self.capture('01-launcher-filtered')
        # Chromium sorts ahead of Studio Settings when the query is empty.
        # Enter must launch Studio Settings from the filtered list.
        self.key('ret')
        settings_class = 'com.crowbaros.StudioSettings'
        settings = self.wait('the launcher query starts Studio Settings',
                             lambda: next((c for c in self.snapshot()['clients'] if c['class'] == settings_class), None))
        self.check('typing without a click filters the launcher to Studio Settings', bool(settings))
        self.wait('Studio Settings takes the keyboard', lambda: self.snapshot()['active']['address'] == settings['address'])
        self.key('ctrl', 'q')
        self.wait('Studio Settings closes after the launcher check',
                  lambda: not any(c['class'] == settings_class for c in self.snapshot()['clients']))
        self.click(680, 935)  # Return focus to Terminal before checking the lock.
        self.wait('Terminal has the keyboard again', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.key('meta_l', 'l')
        self.wait('Super+L locks', lambda: self.snapshot().get('locked') and 'SHELL lock open=true' in shell_log())
        self.capture('01-locked')
        self.check('Super+L locks the session and shows the lock screen',
                   self.snapshot()['locked'] and 'SHELL lock open=true' in shell_log())
        # With the right password typed, the chord's Enter would send it, if
        # it reached the lock screen.
        self.type('crowbar')
        self.key('ctrl', 'alt', 'ret')
        time.sleep(1)
        self.check('Ctrl+Alt+Enter does not unlock, nor send a typed password', self.snapshot()['locked'])
        self.key('esc')
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
        terminal = self.compositor_restart()
        self.click(720, 935)
        settings = self.wait('Settings launched', lambda: next((c for c in self.snapshot()['clients'] if c['class'] == settings_class), None))
        self.check('Settings launch through the shell', bool(settings))
        self.capture('02-settings-before')
        self.click(680, 935)
        self.wait('Terminal focus', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('dock focuses an existing app without duplicating it',
                   len([c for c in self.snapshot()['clients'] if c['class'] == 'foot']) == 1)
        self.click(80, 12)
        self.wait('workspace 2', lambda: self.snapshot()['workspace'] == 2)
        self.check('workspace switch clears focus on hidden windows', not self.snapshot()['active']['address'])
        self.click(60, 12)
        self.wait('workspace 1', lambda: self.snapshot()['workspace'] == 1)
        self.bar_follows_workspaces()
        self.click(720, 935)
        self.wait('Settings focus', lambda: self.snapshot()['active']['address'] == settings['address'])
        # Settings initially focuses Dark. Tab, Space chooses Light, Ctrl+S saves.
        self.key('tab')
        self.key('spc')
        self.wait('Light preview', lambda: 'event=preview theme=Light' in self.app_log())
        self.check('preview has not written preferences', self.preferences() is None)
        self.key('ctrl', 's')
        saved = self.wait('Settings Save', lambda: self.preferences())
        self.check('Save persists Light appearance', saved['theme'] == 'light')
        self.wait('shell follows saved appearance', lambda: 'SHELL appearance theme=Light' in self.guest.ssh('cat /var/log/crowbar-shell.log', check=False).stdout)
        self.capture('03-settings-saved')
        self.key('tab')
        self.key('spc')
        self.wait('Warm Paper preview', lambda: 'event=preview theme=WarmPaper' in self.app_log())
        self.key('ctrl', 'z')
        self.wait('Undo acknowledgement', lambda: 'STUDIO event=undo' in self.app_log())
        self.check('Undo keeps saved preferences unchanged', self.preferences() == saved)
        self.key('ctrl', 'q')
        self.wait('Settings closes cleanly', lambda: not any(c['class'] == settings['class'] for c in self.snapshot()['clients']))
        self.click(720, 935)
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
        self.still('a still desktop, unlocked with apps open, commits nothing and draws nothing')
        self.display_unplugged()
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
        self.wait('new boot ID after clean reboot', lambda: (r := self.guest.ssh('cat /proc/sys/kernel/random/boot_id', check=False, timeout=6)).returncode == 0 and r.stdout.strip() != boot_id, timeout=180)
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
        self.click(720, 935)
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
        # Verify Network, Sound, and Bluetooth pages open cleanly in Light theme
        person = self.person()[0]
        self.click(136, 252)  # Network sidebar button
        self.wait('Settings Network page loads', lambda: 'STUDIO event=network-loaded ' in self.app_log()[log_offset:])
        net_log = self.app_log()[log_offset:]
        self.check('Settings Network page loads Ethernet and calm Wi-Fi unavailable state',
                   'STUDIO event=network-loaded ethernet=1 wifi=unavailable' in net_log
                   and 'wifi-scan-failed' not in net_log)
        self.capture('05a-settings-network-light')
        self.click(136, 300)  # Sound sidebar button
        self.wait('Settings Sound page loads PipeWire', lambda: 'STUDIO event=sound-loaded ' in self.app_log()[log_offset:])
        self.check('Settings Sound page loads live PipeWire output',
                   bool(re.search(r'STUDIO event=sound-loaded sinks=[1-9]', self.app_log()[log_offset:])))
        # Verify Sound reflects external PipeWire volume changes and mutates PipeWire via UI
        sync_before = self.app_log()[log_offset:].count('STUDIO event=sound-synced ')
        self.run(f"su -s /bin/sh {person} -c 'XDG_RUNTIME_DIR=/run/user/1003 wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.65'")
        self.wait('Settings Sound syncs external PipeWire volume change',
                  lambda: self.app_log()[log_offset:].count('STUDIO event=sound-synced ') > sync_before)
        self.check('Settings Sound reflects live external PipeWire volume changes', True)
        self.capture('05b-settings-sound-light')
        update_before = self.app_log()[log_offset:].count('STUDIO event=sound-updated ')
        self.click(289, 368)  # Output volume "-" step button (-5% -> 0.60)
        self.wait('Settings Sound volume click updates PipeWire',
                  lambda: self.app_log()[log_offset:].count('STUDIO event=sound-updated ') > update_before)
        vol_after = self.run(f"su -s /bin/sh {person} -c 'XDG_RUNTIME_DIR=/run/user/1003 wpctl get-volume @DEFAULT_AUDIO_SINK@'").strip()
        self.check('Settings Sound mutates live PipeWire volume from the UI',
                   'Volume: 0.60' in vol_after, vol_after)
        mute_before = self.app_log()[log_offset:].count('STUDIO event=sound-updated ')
        self.click(948, 368)  # Output Mute toggle button
        self.wait('Settings Sound mute click updates PipeWire',
                  lambda: self.app_log()[log_offset:].count('STUDIO event=sound-updated ') > mute_before)
        muted_after = self.run(f"su -s /bin/sh {person} -c 'XDG_RUNTIME_DIR=/run/user/1003 wpctl get-volume @DEFAULT_AUDIO_SINK@'").strip()
        self.check('Settings Sound toggles live PipeWire mute from the UI',
                   '[MUTED]' in muted_after, muted_after)
        self.run(f"su -s /bin/sh {person} -c 'XDG_RUNTIME_DIR=/run/user/1003 wpctl set-mute @DEFAULT_AUDIO_SINK@ 0 && XDG_RUNTIME_DIR=/run/user/1003 wpctl set-volume @DEFAULT_AUDIO_SINK@ 1.00'")
        self.click(136, 348)  # Bluetooth sidebar button
        self.wait('Settings Bluetooth page loads', lambda: 'STUDIO event=bluetooth-loaded ' in self.app_log()[log_offset:])
        self.check('Settings Bluetooth page shows calm unavailable state without adapter',
                   'STUDIO event=bluetooth-loaded state=unavailable' in self.app_log()[log_offset:])
        self.capture('05c-settings-bluetooth-light')
        # Preview Dark appearance, then capture Displays, Network, Sound, and Bluetooth in Dark theme
        self.click(136, 156)
        self.click(320, 350)
        self.wait('Dark preview before Displays capture', lambda: 'event=preview theme=Dark' in self.app_log()[log_offset:])
        self.click(136, 204)
        time.sleep(0.3)
        self.capture('06-settings-displays-dark')
        self.click(136, 252)
        time.sleep(0.3)
        self.capture('06a-settings-network-dark')
        self.click(136, 300)
        time.sleep(0.3)
        self.capture('06b-settings-sound-dark')
        self.click(136, 348)
        time.sleep(0.3)
        self.capture('06c-settings-bluetooth-dark')
        # Verify Files is configured as the default directory handler and launches natively from the dock
        dir_handler = self.run(f"su -s /bin/sh {person} -c 'xdg-mime query default inode/directory'").strip()
        self.check('Files is configured as the default directory handler',
                   dir_handler == 'com.crowbaros.Files.desktop',
                   f'inode/directory={dir_handler!r}')
        self.click(600, 935)  # Files dock entry
        files_app = self.wait(
            'Files launched through the shell',
            lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'com.crowbaros.Files'), None),
            timeout=20,
        )
        self.check('Files launches natively on Fulcrum from the dock', bool(files_app))
        self.wait('Files takes focus', lambda: self.snapshot()['active']['address'] == files_app['address'], timeout=15)
        self.capture('06b-files')
        self.run('pkill -x crowbaros-files; true')
        self.wait('Files closes',
                  lambda: not any(c['class'] == 'com.crowbaros.Files' for c in self.snapshot()['clients']))
        # Verify Chromium is configured as the default browser and launches natively from the dock
        default_browser = self.run(f"su -s /bin/sh {person} -c 'xdg-settings get default-web-browser'").strip()
        https_handler = self.run(f"su -s /bin/sh {person} -c 'xdg-mime query default x-scheme-handler/https'").strip()
        self.check('Chromium is configured as the default browser',
                   default_browser == 'chromium.desktop' and https_handler == 'chromium.desktop',
                   f'browser={default_browser!r} https={https_handler!r}')
        self.click(640, 935)  # Chromium dock entry
        browser = self.wait(
            'Chromium launched through the shell',
            lambda: next((c for c in self.snapshot()['clients'] if 'chromium' in c['class'].lower()), None),
            timeout=30,
        )
        self.check('Chromium launches natively on Fulcrum from the dock', bool(browser))
        self.wait('Chromium takes focus', lambda: self.snapshot()['active']['address'] == browser['address'], timeout=15)
        time.sleep(1.5)
        self.capture('07-chromium')
        self.agent_surface()
        self.agent_reached_lever_idle()

    def agent_surface(self):
        """The summon gesture, Super+Space, and the Agent's surface (#294): a
        drawer on the right of the display the pointer is on. It runs last,
        since it plugs a second display in. What the shell and Fulcrum log
        says what each did, and a pixel of the drawer's ground says what the
        display shows there."""
        shown = 'agent surface shown output='
        opened, away, back = (f'SHELL agent-surface {what}' for what in ('up=true', 'up=false', 'keyboard=true'))
        shell = lambda text: self.shell_log().count(text)
        active = lambda: self.snapshot()['active']['address']
        preferences = self.preferences()
        ground = (0x11, 0x11, 0x11) if preferences['studio'] else CANVAS[preferences['theme']]
        drawn = lambda name, head=0, point=DRAWER_POINT: like(self.pixel(name, *point, head=head), ground)
        self.run('pkill -x chromium; true')
        self.wait('Chromium closes',
                  lambda: not any('chromium' in c['class'].lower() for c in self.snapshot()['clients']))
        self.click(680, 935)  # Terminal dock entry.
        terminal = self.wait('Terminal launched for the Agent\'s surface',
                             lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'foot'), None))
        self.wait('Terminal has the keyboard', lambda: active() == terminal['address'])

        def summon(waits_for, output='Virtual-1', keyboard=False):
            """Super+Space, once the shell has done what the gesture asks.
            Fulcrum logs the display it named and where the keyboard was."""
            told = f'summon output={output} keyboard={str(keyboard).lower()}'
            gestures, before = self.count(told), shell(waits_for)
            self.key('meta_l', 'spc')
            self.wait(f'Super+Space: {waits_for}', lambda: shell(waits_for) > before)
            return self.count(told) == gestures + 1

        # Up: on the right of the only display, its full height, a third of
        # its width, with the keyboard.
        self.check('nothing of the Agent\'s surface shows before it is summoned',
                   not drawn('10-before-summon') and not shell(opened))
        shows = self.count(shown)
        told = summon(opened)
        self.wait('the Agent\'s surface shows', lambda: self.count(shown) > shows)
        self.wait('the Agent\'s surface takes the keyboard', lambda: not active())
        placed = self.run(f"grep -e '{shown}' /var/log/crowbar-compositor.log | tail -1").strip()
        self.check('Super+Space brings the Agent\'s surface up on the right of the display the pointer is on, '
                   'its full height and a third of its width',
                   told and 'output=Virtual-1 at=853,0 size=427x960' in placed and drawn('11-agent-surface'), placed)
        # That it went to the drawer, Fulcrum says with the gesture that
        # puts the drawer away, below.
        self.check('summoning takes the keyboard from the window that had it', not active())
        self.key('esc')
        time.sleep(1)
        self.check('Escape does not put the Agent\'s surface away', not shell(away) and drawn('11-after-escape'))
        self.still('a still desktop with the Agent\'s surface up commits nothing and draws nothing')

        # A click on a window gives it the keyboard, and the drawer stays.
        self.click(150, 300)
        clicked = self.wait('the clicked window takes the keyboard', active)
        self.check('a click on a window gives it the keyboard and leaves the Agent\'s surface up',
                   not shell(away) and drawn('11-window-clicked'))
        told = summon(back)
        self.wait('the keyboard returns to the Agent\'s surface', lambda: not active())
        self.check('with the keyboard in a window, Super+Space returns it to the Agent\'s surface',
                   told and not shell(away))
        told = summon(away, keyboard=True)
        self.wait('the window that had the keyboard has it back', lambda: active() == clicked)
        self.check('summoning gave the Agent\'s surface the keyboard: with it there, Super+Space puts the surface '
                   'away, and the window that had the keyboard has it back',
                   told and active() == clicked and not drawn('12-put-away'))

        # Over a fullscreen window, which covers the bar: its frame shows at
        # the display's top edge, where the bar's ground was.
        self.click(680, 935)
        self.wait('Terminal has the keyboard', lambda: active() == terminal['address'])
        self.check('the bar shows above a window that is not fullscreen', drawn('13-before-fullscreen', point=(300, 1)))
        self.key('meta_l', 'f')
        self.wait('Terminal goes fullscreen', lambda: not drawn('13-fullscreen', point=(300, 1)))
        self.check('a fullscreen window covers where the Agent\'s surface comes up', not drawn('13-fullscreen'))
        shows = self.count(shown)
        summon(opened)
        self.wait('the Agent\'s surface shows over the fullscreen window', lambda: self.count(shown) > shows)
        self.wait('the Agent\'s surface takes the keyboard', lambda: not active())
        self.check('the Agent\'s surface comes up over a fullscreen window, which stays fullscreen',
                   drawn('14-over-fullscreen') and not drawn('14-over-fullscreen', point=(300, 1)))
        # The close control, at the right of the drawer's header.
        closes = shell(away)
        self.click(1253, 24)
        self.wait('the close control puts the Agent\'s surface away', lambda: shell(away) > closes)
        self.wait('Terminal has the keyboard back', lambda: active() == terminal['address'])
        self.check('the close control puts the Agent\'s surface away, and the window that had the keyboard has it back',
                   not drawn('15-closed-by-its-control'))
        self.key('meta_l', 'f')
        self.wait('Terminal leaves fullscreen', lambda: drawn('15-after-fullscreen', point=(300, 1)))

        # Super tapped alone is still the launcher's.
        launcher, surfaces = shell('SHELL launcher open=true'), shell(opened)
        self.key('meta_l')
        self.wait('Super opens the launcher', lambda: shell('SHELL launcher open=true') > launcher)
        self.check('Super tapped alone still opens the launcher, and not the Agent\'s surface', shell(opened) == surfaces)
        closed = shell('SHELL launcher open=false')
        self.key('meta_l')
        self.wait('Super closes the launcher', lambda: shell('SHELL launcher open=false') > closed)

        # Locked, the gesture does nothing, and the drawer is as it was after:
        # up with the keyboard, and then put away.
        summon(opened)
        self.wait('the Agent\'s surface takes the keyboard', lambda: not active())
        for name, up in (('up', True), ('put away', False)):
            gestures = self.count('summon output=')
            logged = [shell(text) for text in (opened, away, back)]
            self.key('meta_l', 'l')
            self.wait('Super+L locks', lambda: self.snapshot().get('locked'))
            self.key('meta_l', 'spc')
            time.sleep(1.5)
            self.capture(f'16-locked-with-the-surface-{name.replace(" ", "-")}')
            self.check(f'Super+Space does nothing while the desktop is locked, with the Agent\'s surface {name}',
                       self.snapshot()['locked'] and self.count('summon output=') == gestures
                       and [shell(text) for text in (opened, away, back)] == logged)
            self.unlock()
            self.wait('the password unlocks', lambda: not self.snapshot().get('locked'))
            time.sleep(1)
            self.check(f'after unlock the Agent\'s surface is {name}, as it was',
                       [shell(text) for text in (opened, away, back)] == logged
                       and drawn(f'16-unlocked-with-the-surface-{name.replace(" ", "-")}') == up
                       and bool(active()) != up)
            if up:
                told = summon(away, keyboard=True)
                self.wait('Terminal has the keyboard back', lambda: active() == terminal['address'])
                self.check('after unlock the Agent\'s surface still has the keyboard, and gives it back', told)

        # A second display, to the right of the first: the drawer comes up
        # on whichever the pointer is on. QEMU's tablet covers both.
        connected = self.count('output connected name=Virtual-2')
        self.guest.plug_monitor(1, 1024, 768)
        self.wait('the second display comes up',
                  lambda: self.count('output connected name=Virtual-2') > connected)
        # QEMU gives both monitors one identity, and the layout keeps a place
        # by identity, so the second takes the place saved for the first,
        # over it. The person would move it in Studio Settings; so does this.
        self.displays({'op': 'configure_output', 'name': 'Virtual-2', 'x': 1280, 'y': 0})
        layout = {o['name']: (o['x'], o['y'], o['width'], o['height'], o['scale'])
                  for o in self.displays({'op': 'outputs'})['outputs']}
        self.check('the second display is to the right of the first',
                   layout == {'Virtual-1': (0, 0, 1280, 960, 120), 'Virtual-2': (1280, 0, 1024, 768, 120)}, str(layout))
        time.sleep(2)
        for output, (x, y), head, place, width, point in (
                ('Virtual-2', (1280 + 500, 300), 1, 'at=1963,0 size=341x768', 341, (1024 - 30, 600)),
                ('Virtual-1', (600, 400), 0, 'at=853,0 size=427x960', 427, DRAWER_POINT)):
            self.qmp.pointer(x / (1280 + 1024), y / 960)
            shows = self.count(shown)
            told = summon(opened, output=output)
            self.wait(f'the Agent\'s surface shows on {output}', lambda: self.count(shown) > shows)
            self.wait('the Agent\'s surface takes the keyboard', lambda: not active())
            placed = self.run(f"grep -e '{shown}' /var/log/crowbar-compositor.log | tail -1").strip()
            asked = f'SHELL agent-surface up=true output={output} width={width}'
            self.check(f'with two displays, Super+Space brings the Agent\'s surface up on {output}, where the pointer is',
                       told and f'output={output} {place}' in placed and asked in self.shell_log()
                       and drawn(f'17-summoned-on-{output}', head, point), placed)
            self.capture(f'17-other-display-while-on-{output}', 1 - head)
            summon(away, output=output, keyboard=True)
            self.wait('the Agent\'s surface is put away', lambda: not drawn(f'17-put-away-on-{output}', head, point))

    def agent_reached_lever_idle(self):
        """With Lever running from boot (ADR 0036) configured with OpenCode,
        summoning the Agent's drawer shows that the shell reached Lever and
        that an idle Lever has started no OpenCode."""
        self.key('meta_l', 'spc')
        view = self.wait('drawer opens connected to idle Lever',
                         lambda: (v := self.agent_view()) and drawer_reached_lever_idle(v) and v)
        time.sleep(0.5)
        self.capture('18-drawer-lever-idle')
        self.check('the drawer shows the Agent is reachable and Lever is idle with OpenCode configured',
                   drawer_reached_lever_idle(view)
                   and 'SHELL agent-socket connected=true' in self.shell_log(),
                   json.dumps(view))
        self.key('meta_l', 'spc')
        self.wait('drawer closes after Lever status check',
                  lambda: not self.agent_view()['drawer_up'])

    def compositor_restart(self):
        """The compositor's service restarts while the person uses the desktop.
        The desktop comes back locked, behind the lock screen, with no app
        holding the keyboard; the password unlocks it, and the Terminal the
        later checks use is launched again. Returns the new Terminal."""
        self.check('the desktop is unlocked before the compositor restarts', not self.snapshot()['locked'])
        shell = self.run('pgrep -x crowbaros-shell').split()
        compositor = self.run('pgrep -u crowbar-compositor').split()
        log_offset = len(self.compositor_log())
        locks = self.shell_log().count('SHELL lock open=true')
        self.run('sv restart crowbar-compositor')
        self.wait('a new compositor and a new shell',
                  lambda: self.run('pgrep -u crowbar-compositor').split() not in ([], compositor)
                  and self.run('pgrep -x crowbaros-shell').split() not in ([], shell)
                  and self.snapshot().get('connected'), timeout=60)
        self.wait('the lock screen shows after the compositor restarts',
                  lambda: self.snapshot().get('locked') and self.shell_log().count('SHELL lock open=true') > locks)
        self.capture('01-locked-after-compositor-restart')
        self.check('a compositor restart comes back locked, behind the lock screen', self.snapshot()['locked'])
        # The new compositor locked as it started, and nothing unlocked it
        # before the person's password.
        since = self.compositor_log()[log_offset:]
        self.check('the new compositor starts locked, and nothing unlocks it first',
                   'session locked by=start' in since and 'session unlocked' not in since)
        # An app opens a window behind the lock, as one would that started
        # again on its own, and the person types: the lock screen has the
        # keyboard, and the app hears none of it.
        keylog = '/tmp/keylog-behind-the-lock.out'
        try:
            self.as_app('crowbar-probe keylog behind-the-lock', keylog)
            self.wait('a window opens behind the lock', lambda: 'window mapped' in self.run(f'cat {keylog}'))
            answered = self.shell_log().count('SHELL unlock ok=false')
            self.unlock('wrong')
            self.wait('the lock screen answers the wrong password',
                      lambda: self.shell_log().count('SHELL unlock ok=false') > answered)
            heard = self.run(f'cat {keylog}')
            self.check('an app behind the lock gets no keys the person types', self.snapshot()['locked']
                       and not re.search(r'\b(enter|key) ', heard), heard.strip())
        finally:
            self.run("pkill -f 'crowbar-probe [k]eylog'; true")
        self.unlock()
        self.wait('the password unlocks after the compositor restarts', lambda: not self.snapshot().get('locked'))
        self.check("the person's password unlocks the desktop after the compositor restarts",
                   'session unlocked' in self.compositor_log()[log_offset:])
        self.click(680, 935)  # Terminal dock entry.
        terminal = self.wait('Terminal launched after the compositor restarts',
                             lambda: next((c for c in self.snapshot()['clients'] if c['class'] == 'foot'), None))
        self.wait('the new Terminal takes the keyboard', lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('Terminal launches again after the compositor restarts',
                   len([c for c in self.snapshot()['clients'] if c['class'] == 'foot']) == 1)
        return terminal

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
        (self.work / 'identities.json').write_text(json.dumps(report, indent=2) + '\n')
        found = report['processes']
        person = void_base.PERSON_UID
        for name, uid in [('crowbar-compositor', 1000), ('crowbaros-shell', 1001)]:
            self.check(name + ' has the assigned non-root identity', name in found and found[name]['uid'] == [uid] * 4)
        for name in ('foot', 'crowbaros-studio-settings'):
            self.check(name + " runs as the person's account", name in found and found[name]['uid'] == [person] * 4)
            self.check(name + " is at the ordinary Wayland socket, in the person's runtime directory",
                       {f'XDG_RUNTIME_DIR=/run/user/{person}', 'WAYLAND_DISPLAY=wayland-0'} <= set(found[name]['env']))
        self.check("the person's terminal may join Wi-Fi, in netdev", report['netdev_gid'] in found['foot']['groups'])
        launcher = self.run("for p in $(pgrep -f '^/usr/local/bin/crowbar-launcher$'); do grep -E '^(Uid|Gid):' /proc/$p/status; done")
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
        # The compositor's control channel and its unlock socket are knocked
        # on as their own peers knock, with the frame that states the
        # version: the compositor would answer that from a peer it took for
        # the shell or the unlock helper, and a line it refuses from anyone.
        framed = ['/run/crowbar-trusted/compositor/control', '/run/crowbar-trusted/unlock/compositor']
        paths = framed + ['/run/crowbar-trusted/compositor/wayland', '/run/crowbar-launch/socket',
                          '/run/user/1001/crowbaros-shell/control.sock', '/run/crowbar-unlock/socket']
        code = """
import json, socket
results = []
for path in PATHS:
 s = socket.socket(socket.AF_UNIX); s.settimeout(2)
 try:
  s.connect(path); s.sendall(VERSION if path in FRAMED else b'{"op":"snapshot"}\\n')
  data = s.recv(1024)
  results.append(not data or b'"ok":false' in data.replace(b' ', b''))
 except (PermissionError, ConnectionResetError, BrokenPipeError): results.append(True)
 except TimeoutError: results.append(False)
 finally: s.close()
print(json.dumps(results))
""".replace('PATHS', repr(paths)).replace('FRAMED', repr(framed)).replace('VERSION', repr(guest.FULCRUM_VERSION_FRAME))
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
        # Fulcrum's Harness socket: serves the person's account with a snapshot,
        # and refuses root and the shell even with its path exposed.
        agent_dir = '/run/crowbar-trusted/agent'
        agent_sock = f'{agent_dir}/compositor'
        ask_agent = guest.ask_fulcrum(agent_sock, '{"op":"snapshot"}')
        person = self.person()[0]
        agent_snap = self.run(f'chpst -u {person}:{person} {ask_agent}')
        self.check("Fulcrum's Harness socket answers the person's account with a snapshot",
                   '"reply":"snapshot"' in agent_snap, agent_snap.strip())
        modes = json.loads(self.python(f'import os,json; print(json.dumps({{p:os.stat(p).st_mode & 0o7777 for p in ["{agent_dir}", "{agent_sock}"]}}))'))
        try:
            self.run(f'chmod o+x {agent_dir}; chmod o+rw {agent_sock}')
            as_root = self.run(ask_agent)
            as_shell = self.run(f'chpst -u crowbar-shell:crowbar-shell {ask_agent}')
        finally:
            self.python('import os; modes=' + repr(modes) + '; [os.chmod(p,m) for p,m in modes.items()]')
        self.check("Fulcrum's Harness socket refuses root and the shell, even with its path exposed",
                   'snapshot' not in as_root and 'snapshot' not in as_shell)

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
        output = lambda name: self.guest.ssh(f'cat /tmp/{name}.out', check=False).stdout
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

    def displays(self, request):
        """One request to the shell's display bridge, as Studio Settings
        makes it, and its reply."""
        code = ("import json, socket; s=socket.socket(socket.AF_UNIX); s.settimeout(3); "
                "s.connect('/run/crowbar-displays/socket'); "
                f"s.sendall({json.dumps(json.dumps(request) + chr(10))}.encode()); "
                "print(s.makefile('rb').readline().decode())")
        return json.loads(self.python(code, self.person()[0]))

    def display_unplugged(self):
        """The only display is unplugged and plugged in again (#271), by
        forcing its connector off and back to what it detects. The
        compositor closes the shell's layers, and the shell stops once,
        cleanly. Started again, it waits for a display without stopping, and
        draws when one comes. The display is at 1.5 first, as a 4K monitor
        may be, so the scale the shell is told falls as the display goes:
        GPUI once resized a window it had destroyed on that."""
        status = self.run('ls /sys/class/drm/card*-Virtual-1/status').strip()
        scale = self.displays({'op': 'outputs'})['outputs'][0]['scale']
        self.displays({'op': 'configure_output', 'name': 'Virtual-1', 'scale': 150})
        time.sleep(2)
        app = self.run('pgrep -x foot').split()
        shell_from, compositor_from = len(self.shell_log()), len(self.compositor_log())
        # Forcing the connector sends no hotplug event. A new size asked of
        # QEMU for the connector makes virtio-gpu send the kernel's, and the
        # compositor then finds the connector as it was forced. QEMU passes on
        # only a size that differs from the last, so the two differ.
        self.run(f'echo off > {status}')
        self.guest.plug_monitor(0, 1024, 768)
        self.wait('the display goes away',
                  lambda: 'output disconnected name=Virtual-1' in self.compositor_log()[compositor_from:])
        self.wait('the shell waits for a display',
                  lambda: 'SHELL waits for a display' in self.shell_log()[shell_from:])
        # Without a display for a while: a shell that stopped each time it
        # started would start several times in this.
        time.sleep(6)
        since = self.shell_log()[shell_from:]
        starts = since.count('SHELL display-bridge listening')
        self.check('with its display unplugged, the shell stops once without panicking, and waits',
                   'panicked' not in since and starts == 1
                   and self.guest.ssh('pgrep -x crowbaros-shell', check=False).stdout.strip(),
                   f'started {starts} times')
        before = self.layer_commits()
        self.run(f'echo detect > {status}')
        self.guest.plug_monitor(0, 1280, 960)
        self.wait('the display comes back',
                  lambda: 'output connected name=Virtual-1' in self.compositor_log()[compositor_from:])
        self.wait('the shell sees the display', lambda: 'SHELL display connected' in self.shell_log()[shell_from:])
        self.ready()
        # Fulcrum's next report shows the shell's layers committing again.
        drawn = self.wait('the shell draws on the display',
                          lambda: all(self.layer_commits().get(name, 0) > before.get(name, 0)
                                      for name in ('wallpaper', 'bar', 'dock')))
        self.displays({'op': 'configure_output', 'name': 'Virtual-1', 'scale': scale})
        time.sleep(2)
        self.capture('05-display-back')
        since = self.shell_log()[shell_from:]
        self.check('plugged in again, the shell draws its layers on it, and the app keeps running',
                   bool(drawn) and 'panicked' not in since and self.run('pgrep -x foot').split() == app
                   and since.count('SHELL display-bridge listening') == 1)

    def still(self, name):
        """Once a still desktop settles, it commits nothing and draws nothing
        (#269). Fulcrum writes its report after a commit or a frame, at most
        every 5 s, so a desktop that keeps drawing adds a report in 12 s."""
        # The clock changes what the bar and the lock screen show each minute,
        # so the 15 s looked at stay within one minute.
        second = int(self.run('date +%S'))
        if second > 40:
            time.sleep(62 - second)
        time.sleep(STILL_SETTLE)
        before = self.reports()
        time.sleep(STILL_WINDOW)
        after = self.reports()
        self.check(name, after == before,
                   f'{after - before} reports in {STILL_WINDOW} s')

    def layer_commits(self):
        """The commits each of the shell's layers has made, by name, as
        Fulcrum last reported them."""
        report = self.run("grep 'commits so far' /var/log/crowbar-compositor.log | tail -1 || true")
        return {name: int(count) for name, count in re.findall(r'layer crowbaros-(\w+): (\d+)', report)}

    def reports(self):
        """How many reports Fulcrum has written."""
        return int(self.run("grep -c 'commits so far' /var/log/crowbar-compositor.log || true").strip() or 0)

    def bar_follows_workspaces(self):
        """Super+1 to 9 show a workspace, and the bar draws it in the frame the
        workspace changes or the next (#273). Fulcrum logs how soon each
        layer drew after the workspaces changed."""
        drawn = []
        for workspace in ('2', '1', '2', '1'):
            seen = len(self.bar_drawn())
            self.key('meta_l', workspace)
            drawn.append(self.wait(f'the bar draws workspace {workspace}',
                                   lambda: self.bar_drawn()[seen:])[0])
        self.wait('workspace 1', lambda: self.snapshot()['workspace'] == 1)
        drawn.sort()
        median = (drawn[1] + drawn[2]) / 2
        self.check('the bar draws a workspace in the frame it changes or the next',
                   median <= BAR_FOLLOWS,
                   f'{", ".join(f"{ms:.1f}" for ms in drawn)} ms; at most {BAR_FOLLOWS:.1f} ms')

    def bar_drawn(self):
        """How soon the bar drew after each change to the workspaces, in ms."""
        log = self.guest.ssh('grep "layer crowbaros-bar drew" /var/log/crowbar-compositor.log',
                             check=False).stdout
        return [float(ms) for ms in re.findall(r'drew ([\d.]+) ms after the workspaces changed', log)]


# The ground of the shell's surfaces in each saved theme: the canvas of
# `apps/shell/src/theme.rs`, as red, green, and blue.
CANVAS = {'dark': (0x08, 0x0d, 0x12), 'light': (0xf4, 0xf7, 0xf9), 'warm-paper': (0xf7, 0xf2, 0xe8)}
# A point of the first display that the Agent's surface covers with its
# ground alone: in its right third, below its text and above its foot.
DRAWER_POINT = (1250, 700)


def like(colour, expected):
    """Whether two colours are the same to within what a renderer rounds."""
    return len(colour) == len(expected) == 3 and all(abs(a - b) <= 2 for a, b in zip(colour, expected))


# How long a desktop is given to settle, as GPUI keeps presenting for a second
# after input, and how long it is then watched.
STILL_SETTLE = 3
STILL_WINDOW = 12
# Two frames at the guest's 60 Hz: the frame the workspace changes, or the next.
BAR_FOLLOWS = 2 * 1000 / 60


QMP_PUNCTUATION = {
    ' ': ('spc',),
    '-': ('minus',),
    '_': ('shift', 'minus'),
    '.': ('dot',),
    ',': ('comma',),
    '/': ('slash',),
    '?': ('shift', 'slash'),
    ';': ('semicolon',),
    ':': ('shift', 'semicolon'),
    '=': ('equal',),
    '+': ('shift', 'equal'),
    "'": ('apostrophe',),
    '"': ('shift', 'apostrophe'),
}


def drawer_reached_lever_idle(view):
    """Whether the shell's Agent view snapshot shows the drawer up, connected
    to Lever, with Lever's version reported, OpenCode configured (`no_harness`
    false), and no harness process started while idle."""
    state = (view or {}).get('state') or {}
    return (
        bool((view or {}).get('drawer_up'))
        and not (view or {}).get('mark_overlay')
        and bool(state.get('configured'))
        and bool(state.get('connected'))
        and isinstance(state.get('lever_version'), str)
        and bool(state.get('lever_version'))
        and state.get('harness_started') is False
        and state.get('no_harness') is False
        and state.get('model') is None
        and state.get('turns') == []
        and state.get('mark') == 'idle'
    )


def qmp_keys_for_text(text):
    """Maps plain ASCII text to QMP key chord tuples for `Check.key(*seq)`."""
    chords = []
    for ch in text:
        if ch in QMP_PUNCTUATION:
            chords.append(QMP_PUNCTUATION[ch])
        elif 'a' <= ch <= 'z' or '0' <= ch <= '9':
            chords.append((ch,))
        elif 'A' <= ch <= 'Z':
            chords.append(('shift', ch.lower()))
        else:
            raise ValueError(f'unsupported character for QMP typing: {ch!r}')
    return chords


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    guest.add_arguments(parser)
    args = parser.parse_args()
    # What building and booting the image need, before the build spends its time.
    void_base.preflight(vm=True, build=True)
    vm = guest.Guest.create(guest.ROOT / '.lab/void-desktop-checks', 'crowbaros-desktop-check')
    check = Check(vm)
    check.log(f'Evidence and new guest disk: {vm.work}')
    try:
        build_log = vm.work / 'build.log'
        if args.build:
            check.log(f"Building the image with Base's build; its output goes to {build_log}")
        else:
            check.log(guest.NOT_EVIDENCE)
        built = guest.build_base(build_log, build=args.build)
        vm.start()
        vm.wait_ssh()
        check.checks(built)
    except BaseException as error:
        # Whatever stopped the run, a person's Ctrl-C included: a run that
        # did not reach its end did not pass, and keeps its disk.
        error_text = repr(error)
        if all(row['passed'] for row in check.results):
            check.results.append({'name': 'smoke run completed', 'passed': False, 'detail': error_text})
        check.log(f'FAIL: {error_text}')
        if not isinstance(error, Exception):
            raise
    finally:
        if vm.running():
            check.diagnostics()
            vm.stop()
        if check.qmp is not None:
            check.qmp.close()
        results = 'results.json' if args.build else 'results-not-evidence.json'
        (vm.work / results).write_text(json.dumps(check.results, indent=2) + '\n')
        failed = sum(not row['passed'] for row in check.results)
        if not args.build:
            check.log(guest.NOT_EVIDENCE)
        if not failed and not vm.discard_disk():
            check.log('The guest did not stop, so its disk is kept.')
        mark = '' if args.build else ' (NOT EVIDENCE)'
        check.log(f'RESULT{mark}: {len(check.results)-failed} passed, {failed} failed')
        check.log(f'{"Evidence" if args.build else "Not evidence"}: {vm.work}')
        check.logfile.close()
    return int(failed != 0)


if __name__ == '__main__':
    raise SystemExit(main())
