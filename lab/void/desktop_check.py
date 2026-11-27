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
import base64
import json
import os
import re
import shlex
import subprocess
import struct
import time
import zlib

import guest

void_base = guest.void_base


def viewer_fixtures():
    """Real images and a PDF longer than the visible thumbnail rail."""
    width, height = 320, 200
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.extend((42 + x // 2, 68 + y // 2, 130) if x < 210 else (221, 163, 74))

    def chunk(kind, body):
        return struct.pack('!I', len(body)) + kind + body + struct.pack('!I', zlib.crc32(kind + body))

    png = (b'\x89PNG\r\n\x1a\n'
           + chunk(b'IHDR', struct.pack('!2I5B', width, height, 8, 2, 0, 0, 0))
           + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))
    pages = 12
    font = 3 + pages
    objects = [
        b'<< /Type /Catalog /Pages 2 0 R >>',
        (f'<< /Type /Pages /Kids [{" ".join(f"{n} 0 R" for n in range(3, font))}] '
         f'/Count {pages} >>').encode(),
    ]
    for index in range(pages):
        objects.append((f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 560] '
                        f'/Resources << /Font << /F1 {font} 0 R >> >> '
                        f'/Contents {font + 1 + index} 0 R >>').encode())
    objects.append(b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>')
    for number in range(1, pages + 1):
        title = 'A quiet place to read' if number == 1 else f'The next page - {number}'
        stream = (f'0.15 0.25 0.38 rg 36 380 348 100 re f\n'
                  f'0.95 0.97 0.98 rg BT /F1 20 Tf 52 430 Td ({title}) Tj ET\n'
                  f'0.1 0.1 0.1 rg BT /F1 14 Tf 36 340 Td '
                  f'(CrowbarOS Viewer - page {number}) Tj ET\n'
                  f'BT /F1 14 Tf 36 300 Td (CrowbarOS makes room to read.) Tj ET\n').encode('ascii')
        objects.append(f'<< /Length {len(stream)} >>\nstream\n'.encode() + stream + b'endstream')
    pdf = bytearray(b'%PDF-1.4\n')
    offsets = [0]
    for number, obj in enumerate(objects, 1):
        offsets.append(len(pdf))
        pdf.extend(f'{number} 0 obj\n'.encode() + obj + b'\nendobj\n')
    xref = len(pdf)
    pdf.extend(f'xref\n0 {len(offsets)}\n0000000000 65535 f \n'.encode())
    for offset in offsets[1:]:
        pdf.extend(f'{offset:010} 00000 n \n'.encode())
    pdf.extend(f'trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
    # The second image has different dimensions, so sibling navigation cannot
    # pass merely by keeping the first image on screen.
    sibling = (b'\x89PNG\r\n\x1a\n'
               + chunk(b'IHDR', struct.pack('!2I5B', 200, 320, 8, 2, 0, 0, 0))
               + chunk(b'IDAT', zlib.compress((b'\0' + bytes((74, 163, 221)) * 200) * 320))
               + chunk(b'IEND', b''))
    return {'sample.png': png, 'sibling.png': sibling, 'sample.pdf': bytes(pdf)}


def viewer_state_code(home, directory, restore=False):
    """Keep the person's exact preferences while exercising wallpaper writes."""
    return f'''
from pathlib import Path
import json, shutil
folder = Path({home!r}) / '.config/crowbaros'
backup = Path({directory!r}) / 'original-state'
if {restore!r}:
    for name, existed in json.loads((backup / 'manifest.json').read_text()).items():
        target = folder / name
        if existed:
            folder.mkdir(parents=True, exist_ok=True)
            shutil.copy2(backup / name, target)
        else:
            target.unlink(missing_ok=True)
else:
    backup.mkdir()
    manifest = {{}}
    for name in ('appearance.json', 'wallpaper.rgba'):
        source = folder / name
        manifest[name] = source.exists()
        if source.exists():
            shutil.copy2(source, backup / name)
    (backup / 'manifest.json').write_text(json.dumps(manifest))
'''


def encode_recording(frames, destination):
    """Turn ordered QMP captures into a short, portable motion recording."""
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y',
                    '-framerate', '3', '-i', str(frames / 'frame-%03d.png'),
                    '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-movflags', '+faststart',
                    str(destination)], check=True, timeout=60)


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
        # What the drawer showed when it was waited for in vain, for the
        # evidence: a wait on it says only that it was not as asked.
        try:
            (self.work / 'timeout-agent-view.json').write_text(json.dumps(self.agent_view(), indent=1))
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
            pass
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

    def region(self, name, x, y, width, height, head=0):
        """Read a region, including content that separates equal backgrounds."""
        self.capture(name, head)
        image_width, image_height, rows = guest.png_pixels(self.work / f'{name}.png')
        if not (0 <= x < x + width <= image_width and 0 <= y < y + height <= image_height):
            raise ValueError('screenshot region is outside the display')
        return b''.join(row[3*x:3*(x+width)] for row in rows[y:y+height])

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

    def theme_studio_check(self, appearance):
        """Theme bundles, bounded backgrounds, native switching, and the app."""
        person, home = self.person()
        prefix = f'chpst -u {person} env HOME={home} crowbaros-theme-studio '
        directory = self.python('import tempfile; print(tempfile.mkdtemp(prefix="theme-studio-check-"))', person).strip()
        self.python(f'''
import shutil
from pathlib import Path
root=Path({home!r})/'.config'; backup=Path({directory!r})/'backup'; backup.mkdir()
for name in ['crowbaros','foot','gtk-3.0','gtk-4.0']:
    if (root/name).exists(): shutil.copytree(root/name,backup/name)
''', person)
        try:
            self.capture(f'themes-{appearance}-before')
            reply = self.run(guest.ask_fulcrum('/run/crowbar-trusted/unlock/compositor', '{"op":"set_theme","focused":"#112233","unfocused":"#445566","background":"#778899"}'))
            self.check('the unlock authority cannot change the desktop theme', '"reply":"error"' in reply)
            themes = json.loads(self.run(prefix + '--list-themes'))
            self.check('Theme Studio discovers seven built-in bundles', len(themes) == 7 and all(len(t['backgrounds']) >= 2 for t in themes))
            for theme in ['nord', 'nullkeel-light', 'nullkeel-dark']:
                offset = len(self.shell_log())
                self.run(prefix + '--apply-theme ' + theme)
                self.wait('shell applies ' + theme, lambda: ('SHELL theme=' + theme) in self.shell_log()[offset:])
                current = json.loads(self.run(f'cat {home}/.config/crowbaros/current-theme.json'))
                self.check('applying ' + theme + ' updates saved active colors', current['theme_id'] == theme)
            self.run(f'chpst -u {person} env HOME={home} foot --check-config')
            self.check('generated foot configuration passes foot --check-config', True)
            first='/usr/share/crowbaros-shell/wallpaper.png'
            second='/usr/share/crowbaros-shell/nightwatch.png'
            self.run(prefix + f'--extract {first} "Lab Palette" {appearance} {second}')
            self.run(prefix + '--apply-theme lab-palette')
            first_state=json.loads(self.run(f'cat {home}/.config/crowbaros/current-theme.json'))
            self.run(prefix + '--cycle-background')
            next_state=json.loads(self.run(f'cat {home}/.config/crowbaros/current-theme.json'))
            self.check('an extracted two-background theme cycles and keeps its palette', first_state['colors'] == next_state['colors'] and first_state['background'] != next_state['background'])
            self.wait('the shell receives extracted palette colors',lambda:'SHELL theme=lab-palette' in self.shell_log())
            time.sleep(2)
            self.click(1208,12)
            self.wait('Quick Theme Switcher opens',lambda:'SHELL theme-switcher open=true' in self.shell_log())
            self.capture(f'themes-{appearance}-switcher')
            self.key('shift','tab')
            self.key('ret')
            self.wait('Create theme opens Theme Studio as an ordinary app',lambda:self.snapshot()['active'].get('class')=='com.crowbaros.ThemeStudio')
            time.sleep(1)
            self.capture(f'themes-{appearance}-studio')
            self.type(first)
            self.click(380,464)
            time.sleep(1)
            self.click(490,412)
            self.key('ctrl','a')
            self.type(second)
            self.click(515,464)
            time.sleep(1)
            if appearance == 'light':
                self.click(430,520)
            self.capture(f'themes-{appearance}-extracted')
            self.key('ctrl','q')
            self.capture(f'themes-{appearance}-close-protection')
            self.check('closing an unsaved palette preserves its window',self.snapshot()['active'].get('class')=='com.crowbaros.ThemeStudio')
            self.key('esc')
            # Missing-name recovery focuses the native input, then Save applies.
            self.click(975,888)
            self.type('Native Palette')
            self.click(975,888)
            self.wait('Theme Studio saves and applies a named palette',lambda: self.python(
                f'import json; from pathlib import Path; print(json.loads(Path({home!r}+"/.config/crowbaros/current-theme.json").read_text())["theme_id"])',person).strip() == 'native-palette')
            saved=json.loads(self.run(prefix+'--list-themes'))
            self.check('the native creation flow saves both backgrounds',any(t['id']=='native-palette' and len(t['backgrounds'])==2 for t in saved))
            self.capture(f'themes-{appearance}-saved')
            self.key('ctrl','q')
            self.wait('Theme Studio closes',lambda:not any(c['class']=='com.crowbaros.ThemeStudio' for c in self.snapshot()['clients']))
            self.capture(f'themes-{appearance}-after')
        finally:
            self.python(f'''
import shutil
from pathlib import Path
root=Path({home!r})/'.config'; backup=Path({directory!r})/'backup'
for name in ['crowbaros','foot','gtk-3.0','gtk-4.0']:
    if (root/name).exists(): shutil.rmtree(root/name)
    if (backup/name).exists(): shutil.copytree(backup/name,root/name)
shutil.rmtree({directory!r})
''',person)
            time.sleep(2)

    def calculator_check(self, theme):
        """Real keyboard expressions and inter-client copying, in both themes."""
        self.capture(f'calc-{theme}-before')
        logfile = f'/tmp/calc-{theme}.log'
        app_class = 'com.crowbaros.Calc'
        self.as_app('env CROWBAR_CALC_CHECK=1 crowbaros-calc', logfile)
        self.wait('Calculator opens and takes keyboard focus',
                  lambda: self.snapshot()['active'].get('class') == app_class)
        for mode, expression, answer in [('Standard', '1280 * 1.25', '1600'),
                                          ('Scientific', 'sqrt(81) + log(100)', '11'),
                                          ('Programmer', '0xff XOR 0x0f', '240')]:
            self.key('alt', str(['Standard', 'Scientific', 'Programmer'].index(mode) + 1))
            self.key('ctrl', 'l')
            self.key('ctrl', 'a')
            self.type(expression)
            self.key('ret')
            self.wait(f'Calculator evaluates {mode} keyboard input in {theme}',
                      lambda: f'CALC mode={mode} width=64 result={answer}' in self.run('cat ' + logfile))
            self.capture(f'calc-{theme}-{mode.lower()}')
            self.key('ctrl', 'c')
            copied = self.clipboard('text/plain;charset=utf-8', app_class)
            self.check(f'Calculator copies the {mode} answer in {theme}', copied == answer.encode())
        # Pointer-only calculations exercise every mode's keypad and word widths.
        for point in [(138, 340), (632, 630), (424, 534), (424, 726), (632, 726)]:
            self.click(*point)
        self.wait('Programmer pointer keys evaluate an 8-bit NOT',
                  lambda: 'CALC mode=Programmer width=8 result=255' in self.run('cat ' + logfile))
        self.capture(f'calc-{theme}-programmer-pointer')
        for point in [(214, 151), (200, 688), (632, 348), (450, 490), (294, 444), (450, 640)]:
            self.click(*point)
        self.wait('Scientific pointer keys evaluate a square root',
                  lambda: 'CALC mode=Scientific width=8 result=3' in self.run('cat ' + logfile))
        for point in [(110, 151), (200, 592), (294, 490), (608, 540), (450, 490), (450, 540)]:
            self.click(*point)
        self.wait('Standard pointer keys evaluate an addition',
                  lambda: 'CALC mode=Standard width=8 result=5' in self.run('cat ' + logfile))
        kept = self.count('CALC mode=Standard width=8 result=5', logfile)
        self.key('alt', '3')
        self.click(800, 280)
        self.key('ret')
        self.wait('clicking history restores its expression and mode',
                  lambda: self.count('CALC mode=Standard width=8 result=5', logfile) > kept)
        self.capture(f'calc-{theme}-history')
        self.key('alt', '1')
        self.key('ctrl', 'l')
        self.key('ctrl', 'a')
        self.type('1/0')
        self.capture(f'calc-{theme}-error')
        self.key('ctrl', 'a')
        self.type('(2+3)*4')
        self.key('ret')
        self.wait('Calculator recovers after an invalid expression',
                  lambda: 'CALC mode=Standard width=8 result=20' in self.run('cat ' + logfile))
        self.capture(f'calc-{theme}-after')
        frames = self.work / f'calc-{theme}-typing-frames'
        frames.mkdir()
        self.key('ctrl', 'l')
        self.key('ctrl', 'a')
        for index, character in enumerate('1280*1.25'):
            self.type(character)
            self.capture(f'{frames.name}/frame-{index:03d}')
        encode_recording(frames, self.work / f'calc-{theme}-typing.mp4')
        for frame in frames.iterdir():
            frame.unlink()
        frames.rmdir()
        for expression, answer in [('1280 * 1.25', '1600'), ('0xff + 16', '271')]:
            opened = self.shell_log().count('SHELL launcher open=true')
            self.key('meta_l')
            self.wait('launcher opens for inline math',
                      lambda: self.shell_log().count('SHELL launcher open=true') > opened)
            self.type(expression)
            self.capture(f'calc-{theme}-launcher-{answer}')
            self.key('ret')
            self.wait('copying launcher math returns focus to Calculator',
                      lambda: self.snapshot()['active'].get('class') == app_class)
            copied = self.clipboard('text/plain;charset=utf-8', app_class)
            self.check(f'launcher copies {answer} in {theme}', copied == answer.encode())
        self.key('ctrl', 'q')
        self.wait('Calculator closes',
                  lambda: not any(c['class'] == app_class for c in self.snapshot()['clients']))

    def monitor_check(self, theme):
        """Exercise the real window as the person, using disposable child processes."""
        person, _ = self.person()
        self.run("logger -t monitor-check 'Monitor log visibility check'")
        self.wait('socklog records the Monitor fixture',
                  lambda: 'Monitor log visibility check' in self.run('tail -30 /var/log/socklog/everything/current'))
        sample = json.loads(self.run(f"su -s /bin/sh {person} -c 'crowbaros-monitor --sample'"))
        self.check(f'Monitor reads live metrics without root in {theme}',
                   sample['memory']['total'] > 0 and 'cpu' in sample['cpu']
                   and len(sample['processes']) > 0 and bool(sample['details']['storage']))
        self.check('Monitor reads runit status without access to its controls',
                   any(line.startswith('run: /var/service/crowbar-compositor:')
                       for line in sample['details']['services']))
        access = self.run(f"su -s /bin/sh {person} -c 'test -r /run/crowbar-monitor/services && test ! -w /run/crowbar-monitor/services && test ! -w /var/service/crowbar-compositor/supervise/control && echo read-only'")
        self.check('Monitor status grants read access without service control', access.strip() == 'read-only')
        self.check('Monitor reads recent socklog lines as the person',
                   'Monitor log visibility check' in sample['details']['logs'])
        self.capture(f'monitor-{theme}-before')
        logfile = f'/tmp/monitor-{theme}.log'
        self.as_app('env CROWBAR_MONITOR_CHECK=1 crowbaros-monitor', logfile)
        self.wait('Monitor opens and takes focus',
                  lambda: self.snapshot()['active'].get('class') == 'com.crowbaros.Monitor')
        log = lambda: self.run('cat ' + logfile)
        self.wait('Monitor receives live CPU deltas and memory',
                  lambda: 'MONITOR sample cpu=true memory=true' in log())
        self.capture(f'monitor-{theme}-after')
        # A short recording of real metric updates, kept with the run's captures.
        frames = []
        for frame in range(18):
            name = f'monitor-{theme}-motion-{frame:02d}'
            self.capture(name)
            frames.append(self.work / f'{name}.png')
            time.sleep(1 / 3)
        subprocess.run(['ffmpeg', '-y', '-loglevel', 'error', '-framerate', '3',
                        '-i', str(self.work / f'monitor-{theme}-motion-%02d.png'),
                        '-c:v', 'libx264', '-pix_fmt', 'yuv420p',
                        str(self.work / f'monitor-{theme}-motion.mp4')], check=True)
        for frame in frames:
            frame.unlink()
        self.key('alt', '2')
        self.wait('Monitor opens Processes', lambda: 'MONITOR page=processes' in log())
        self.capture(f'monitor-{theme}-processes')
        for force in (False, True):
            pidfile = f'/tmp/monitor-child-{theme}-{int(force)}.pid'
            self.as_app('sh -c ' + shlex.quote(f'echo $$ > {pidfile}; exec sleep 120'),
                        '/tmp/monitor-child.log')
            pid = self.wait('Monitor test child starts',
                            lambda: self.guest.ssh('cat ' + pidfile, check=False).stdout.strip())
            alive = lambda: self.guest.ssh('kill -0 ' + pid, check=False).returncode == 0
            try:
                # Wait for a subsequent sample so the newly created process is present.
                before = log().count('MONITOR sample')
                self.wait('Monitor refreshes its process list', lambda: log().count('MONITOR sample') > before)
                self.key('ctrl', 'f')
                self.key('ctrl', 'a')
                self.type(pid)
                self.key('esc')
                self.key('down')
                if force:
                    self.key('ctrl', 'k')
                    self.wait('Force Quit asks for confirmation', lambda: 'confirmation=true' in log())
                    self.capture(f'monitor-{theme}-force-confirmation')
                    self.key('esc')
                    self.check('Cancel Force Quit leaves the child running', alive())
                    self.key('ctrl', 'k')
                    self.key('ctrl', 'ret')
                else:
                    self.key('ctrl', 'e')
                self.wait('Monitor ends its selected owned child', lambda: not alive())
                self.check(f'Monitor {"force quits" if force else "terminates"} an owned process in {theme}', True)
            finally:
                self.guest.ssh('kill ' + pid, check=False)
        self.key('ctrl', 'f')
        self.key('ctrl', 'a')
        self.type('1')
        self.key('esc')
        self.key('down')
        self.capture(f'monitor-{theme}-system-process')
        before = len(log())
        self.key('ctrl', 'k')
        self.wait('Monitor refuses Force Quit for a system process',
                  lambda: 'MONITOR page=processes confirmation=false' in log()[before:])
        self.check('System PID 1 remains running',
                   self.guest.ssh('kill -0 1', check=False).returncode == 0)
        self.key('alt', '3')
        self.wait('Monitor opens Services & Storage', lambda: 'MONITOR page=services' in log())
        self.wait('Monitor receives read-only system details', lambda: 'details=true' in log())
        self.capture(f'monitor-{theme}-services')
        self.key('ctrl', 'q')
        self.wait('Monitor closes', lambda: not any(c['class'] == 'com.crowbaros.Monitor'
                                                  for c in self.snapshot()['clients']))

    def editor_check(self, theme):
        person, _ = self.person()
        handler = self.run(f"su -s /bin/sh {person} -c 'xdg-mime query default text/plain'").strip()
        self.check(f'Editor handles plain text in {theme}', handler == 'com.crowbaros.Editor.desktop', handler)
        path = self.python("""
import tempfile
with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as note:
    note.write('Notes from the desktop')
    print(note.name)
""", person).strip()
        self.capture(f'editor-{theme}-before')
        self.as_app('xdg-open ' + shlex.quote(path), '/tmp/editor-open.log')
        self.wait('Editor opens the note and takes focus',
                  lambda: self.snapshot()['active'].get('class') == 'com.crowbaros.Editor')
        self.key('ctrl', 'a')
        self.type('saved note')
        self.key('ctrl', 's')
        self.wait('Editor saves the edited note', lambda: self.run('cat ' + shlex.quote(path)) == 'saved note')
        self.capture(f'editor-{theme}-after')
        self.key('ctrl', 'w')
        self.wait('Editor closes the saved note',
                  lambda: not any(c['class'] == 'com.crowbaros.Editor' for c in self.snapshot()['clients']))
        self.check(f'Editor opens, edits, saves and closes a file in {theme}', True)

    def viewer_check(self, theme):
        person, home = self.person()
        directory = self.python('import tempfile; print(tempfile.mkdtemp(prefix="viewer-check-"))', person).strip()
        self.python(viewer_state_code(home, directory), person)
        try:
            self.viewer_documents(theme, directory)
        finally:
            self.python(viewer_state_code(home, directory, restore=True), person)
            self.python(f'import shutil; shutil.rmtree({directory!r})', person)

    def clipboard(self, mime, app_class='com.crowbaros.Viewer'):
        """Read the compositor's actual selection as another ordinary client."""
        person, home = self.person()
        encoded = self.python(f'''
import base64, os, subprocess
env = dict(os.environ, HOME={home!r}, XDG_RUNTIME_DIR='/run/user/{void_base.PERSON_UID}',
           WAYLAND_DISPLAY='wayland-0')
result = subprocess.run(['wl-paste', '--no-newline', '--type', {mime!r}],
                        env=env, check=True, stdout=subprocess.PIPE, timeout=12)
print(base64.b64encode(result.stdout).decode('ascii'))
''', person)
        # wl-paste's fallback reader briefly maps its own surface to receive
        # the selection. Return the keyboard with a real click after it exits.
        self.click(640, 480)
        self.wait('the app regains focus after the clipboard reader',
                  lambda: self.snapshot()['active'].get('class') == app_class)
        return base64.b64decode(encoded.strip())

    def viewer_pan(self, theme):
        self.key('meta_l', 'f')
        self.key('ctrl', '0')
        self.key('ctrl', 'equal')
        self.key('ctrl', 'equal')
        frames = self.work / f'viewer-image-{theme}-motion'
        frames.mkdir()
        self.qmp.pointer(640 / 1280, 480 / 960)
        self.button(True)
        try:
            for frame in range(18):
                step = max(0, min(frame - 2, 12))
                self.qmp.pointer((640 - step * 24) / 1280, (480 - step * 12) / 960)
                self.qmp.screendump(frames / f'frame-{frame:03d}.png', 0)
        finally:
            self.button(False)
        _, _, before = guest.png_pixels(frames / 'frame-000.png')
        _, _, after = guest.png_pixels(frames / 'frame-017.png')
        self.check(f'Viewer pans the zoomed image in {theme}',
                   before[480][640 * 3:641 * 3] != after[480][640 * 3:641 * 3])
        encode_recording(frames, self.work / f'viewer-image-{theme}-motion.mp4')
        self.key('ctrl', '0')
        self.key('meta_l', 'f')

    def viewer_documents(self, theme, directory):
        person, home = self.person()
        for name, contents in viewer_fixtures().items():
            path = directory + '/' + name
            encoded = base64.b64encode(contents).decode('ascii')
            self.python(f'import base64; from pathlib import Path; '
                        f'Path({path!r}).write_bytes(base64.b64decode({encoded!r}))', person)
        for mime in ('image/png', 'application/pdf'):
            handler = self.run(f'su -s /bin/sh {person} -c '
                               + shlex.quote('xdg-mime query default ' + mime)).strip()
            self.check(f'Viewer handles {mime} in {theme}', handler == 'com.crowbaros.Viewer.desktop', handler)
        for tool in ('pdfinfo', 'pdftoppm', 'pdftotext'):
            linked = self.run('ldd /usr/bin/' + tool)
            self.check(tool + ' linked libraries resolve', '=> /' in linked and 'not found' not in linked)
        for kind, filename in (('image', 'sample.png'), ('pdf', 'sample.pdf')):
            path = directory + '/' + filename
            info = json.loads(self.run(f'su -s /bin/sh {person} -c '
                                      + shlex.quote('crowbaros-viewer --inspect ' + shlex.quote(path))))
            self.check(f'Viewer reads the {kind} fixture as the person',
                       info.get('kind') == kind and
                       ((info.get('width'), info.get('height')) == (320, 200)
                        if kind == 'image' else info.get('pages') == 12), info)
            self.capture(f'viewer-{kind}-{theme}-before')
            logfile = f'/tmp/viewer-{kind}-{theme}.log'
            self.as_app('xdg-open ' + shlex.quote(path), logfile)
            self.wait(f'Viewer opens the {kind} and takes focus',
                      lambda: self.snapshot()['active'].get('class') == 'com.crowbaros.Viewer')
            self.wait(f'Viewer renders the {kind}',
                      lambda: f'VIEWER loaded kind={kind}' in self.run('cat ' + logfile))
            self.key('ctrl', 'equal')
            self.key('ctrl', '0')
            if kind == 'image':
                loaded = self.count('VIEWER loaded kind=image', logfile)
                self.key('right')
                self.wait('Viewer opens the next sibling image',
                          lambda: self.snapshot()['active'].get('title') == 'sibling.png — Viewer'
                          and self.count('VIEWER loaded kind=image', logfile) > loaded)
                self.key('ctrl', 'c')
                copied = self.wait('Viewer puts the sibling image on the clipboard',
                                   lambda: self.clipboard('image/png'))
                self.check(f'Viewer copies the sibling image pixels in {theme}',
                           copied.startswith(b'\x89PNG\r\n\x1a\n')
                           and struct.unpack('!2I', copied[16:24]) == (200, 320))
                loaded = self.count('VIEWER loaded kind=image', logfile)
                self.key('left')
                self.wait('Viewer returns to the previous sibling image',
                          lambda: self.snapshot()['active'].get('title') == 'sample.png — Viewer'
                          and self.count('VIEWER loaded kind=image', logfile) > loaded)
                self.key('ctrl', 'shift', 'w')
                self.wait('Viewer saves the image as wallpaper',
                          lambda: 'VIEWER wallpaper=saved' in self.run('cat ' + logfile))
                self.wait('the shell displays Viewer wallpaper',
                          lambda: 'SHELL wallpaper=custom' in self.shell_log())
                self.viewer_pan(theme)
                self.key('ctrl', 'r')
                self.key('ctrl', 'i')
            else:
                self.key('ctrl', 'g')
                self.type('2')
                self.key('ret')
                self.wait('Viewer renders the requested PDF page',
                          lambda: 'VIEWER page=2' in self.run('cat ' + logfile))
                first_page = self.count('VIEWER page=1', logfile)
                self.qmp.pointer(550 / 1280, 450 / 960)
                for down in (True, False):
                    self.qmp.command('input-send-event', events=[
                        {'type': 'btn', 'data': {'down': down, 'button': 'wheel-up'}}])
                self.wait('scrolling above page two follows the actual PDF row height',
                          lambda: self.count('VIEWER page=1', logfile) > first_page)
                self.check(f'Viewer follows continuous PDF scrolling in {theme}', True)
                self.key('ctrl', 'f')
                self.type('CrowbarOS')
                self.key('ret')
                self.wait('Viewer finds text in the PDF',
                          lambda: 'VIEWER search matches=24' in self.run('cat ' + logfile))
                self.key('esc')
                self.key('f3')
                self.wait('Viewer advances to the next occurrence on the same page',
                          lambda: 'VIEWER search match=2 page=1 ' in self.run('cat ' + logfile))
                before_previous = self.count('VIEWER search match=1 page=1 ', logfile)
                self.key('shift', 'f3')
                self.wait('Viewer returns to the preceding occurrence',
                          lambda: self.count('VIEWER search match=1 page=1 ', logfile) > before_previous)
                # A successful text search must also draw the selectable text.
                # Fullscreen gives the fixed-width inspector a known position.
                self.key('meta_l', 'f')
                def text_panel_drawn():
                    name = f'viewer-pdf-{theme}-selection'
                    self.capture(name)
                    width, height, rows = guest.png_pixels(self.work / f'{name}.png')
                    ink = (216, 229, 236) if theme == 'dark' else (8, 13, 18)
                    count = sum(all(abs(pixel[c] - ink[c]) < 30 for c in range(3))
                                # Below the heading and keyboard hint: those
                                # alone must not hide an empty text field.
                                for y in range(220, min(400, height))
                                for x in range(width - 300, width - 32)
                                for pixel in [rows[y][x * 3:x * 3 + 3]])
                    return count > 50
                self.wait('Viewer draws selectable PDF text', text_panel_drawn)
                self.check(f'Viewer draws selectable PDF text in {theme}', True)
                # Search opens the panel. Close and reopen it with Ctrl+T to
                # focus its read-only field, then exercise keyboard selection.
                self.key('ctrl', 't')
                self.key('ctrl', 't')
                self.key('ctrl', 'a')
                self.key('ctrl', 'c')
                copied = self.wait('Viewer puts selected PDF text on the clipboard',
                                   lambda: self.clipboard('text/plain;charset=utf-8'))
                self.check(f'Viewer copies selected PDF text in {theme}',
                           b'CrowbarOS Viewer - page 1' in copied
                           and b'CrowbarOS makes room to read.' in copied)
                self.key('esc')
                self.qmp.pointer(70 / 1280, 460 / 960)
                for _ in range(45):
                    for down in (True, False):
                        self.qmp.command('input-send-event', events=[
                            {'type': 'btn', 'data': {'down': down, 'button': 'wheel-down'}}])
                    time.sleep(0.04)
                self.wait('Viewer renders a distant thumbnail without visiting its page',
                          lambda: 'VIEWER thumbnail page=12' in self.run('cat ' + logfile))
                self.capture(f'viewer-pdf-{theme}-thumbnails')
                self.click(70, 860)
                self.wait('the last thumbnail navigates to the last PDF page',
                          lambda: 'VIEWER page=12' in self.run('cat ' + logfile))
                self.check(f'Viewer browses and opens distant PDF thumbnails in {theme}', True)
                self.key('meta_l', 'f')
            time.sleep(0.3)
            self.capture(f'viewer-{kind}-{theme}-after')
            self.key('ctrl', 'q')
            self.wait(f'Viewer closes the {kind}',
                      lambda: not any(c['class'] == 'com.crowbaros.Viewer' for c in self.snapshot()['clients']))
            self.check(f'Viewer opens and navigates the {kind} in {theme}', True)
            if kind == 'image':
                expected = (221, 163, 74)
                self.wait('Viewer wallpaper reaches the desktop',
                          lambda: like(self.pixel(f'viewer-wallpaper-{theme}', 1250, 480), expected))
                self.run('sv restart crowbar-shell')
                self.ready()
                self.wait('Viewer wallpaper survives a shell restart',
                          lambda: like(self.pixel(f'viewer-wallpaper-{theme}-restarted', 1250, 480), expected))
                self.check(f'Viewer wallpaper persists after the app and shell close in {theme}', True)
                self.as_app('xdg-open ' + shlex.quote(path), logfile + '-restore')
                self.wait('Viewer opens to restore the built-in wallpaper',
                          lambda: self.snapshot()['active'].get('class') == 'com.crowbaros.Viewer')
                self.wait('Viewer finishes reopening the image',
                          lambda: 'VIEWER loaded kind=image' in self.run('cat ' + logfile + '-restore'))
                builtin = self.shell_log().count('SHELL wallpaper=builtin')
                self.key('ctrl', 'alt', 'w')
                self.wait('Viewer restores the built-in wallpaper',
                          lambda: 'VIEWER wallpaper=restored' in self.run('cat ' + logfile + '-restore'))
                self.wait('the shell restores its built-in wallpaper',
                          lambda: self.shell_log().count('SHELL wallpaper=builtin') > builtin)
                self.check(f'Viewer removes the wallpaper override in {theme}',
                           self.python(f'from pathlib import Path; '
                                       f'print((Path({home!r}) / ".config/crowbaros/wallpaper.rgba").exists())',
                                       person).strip() == 'False')
                self.key('ctrl', 'q')
                self.wait('Viewer closes after restoring wallpaper',
                          lambda: not any(c['class'] == 'com.crowbaros.Viewer' for c in self.snapshot()['clients']))

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
        self.editor_check('dark')
        self.monitor_check('dark')
        self.viewer_check('dark')
        self.calculator_check('dark')
        self.theme_studio_check('dark')
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
        # Quick Settings and Calendar popovers in Dark theme, verifying focus restoration.
        qs_opens = shell_log().count('SHELL quick-settings open=true')
        self.click(1259, 12)  # Top-right Quick Settings control
        self.wait('Quick Settings opens from the bar',
                  lambda: shell_log().count('SHELL quick-settings open=true') > qs_opens)
        self.wait('Quick Settings takes keyboard focus', lambda: not self.snapshot()['active']['address'])
        time.sleep(0.5)
        self.capture('01a-quick-settings-dark')
        self.key('esc')
        self.wait('Escape closes Quick Settings',
                  lambda: 'SHELL quick-settings open=false' in shell_log())
        self.wait('closing Quick Settings restores Terminal focus',
                  lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('Quick Settings opens from the bar and Escape returns focus to the previous window',
                   'SHELL quick-settings open=true' in shell_log()
                   and 'SHELL quick-settings open=false' in shell_log()
                   and self.snapshot()['active']['address'] == terminal['address'])
        cal_opens = shell_log().count('SHELL calendar open=true')
        self.click(640, 12)  # Center clock in the bar
        self.wait('Calendar opens from the bar clock',
                  lambda: shell_log().count('SHELL calendar open=true') > cal_opens)
        self.wait('Calendar takes keyboard focus', lambda: not self.snapshot()['active']['address'])
        self.key('right')
        self.key('left')
        time.sleep(0.3)
        self.capture('01b-calendar-dark')
        self.click(200, 500)  # Click outside popover to dismiss
        self.wait('clicking outside closes Calendar',
                  lambda: 'SHELL calendar open=false' in shell_log())
        self.wait('closing Calendar restores Terminal focus',
                  lambda: self.snapshot()['active']['address'] == terminal['address'])
        self.check('Calendar opens from the bar clock, navigates months, and click-outside returns focus',
                   'SHELL calendar open=true' in shell_log()
                   and 'SHELL calendar open=false' in shell_log()
                   and self.snapshot()['active']['address'] == terminal['address'])
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
        # Persist non-default values without applying them. Only startup
        # restoration can make these live after the compositor is recreated.
        saved_input = {'schema_version': 1, 'layout': 'gb', 'repeat_rate': 40,
                       'repeat_delay': 300, 'pointer_speed': 4, 'natural_scroll': True}
        saved_power = {'schema_version': 1, 'lock_after_secs': 600, 'suspend_after_secs': 1800}
        original_input = self.displays({'op': 'input_config'})
        original_power = self.displays({'op': 'power_config'})
        original_files = json.loads(self.python(f"""
import json, pathlib
folder = pathlib.Path({home!r}) / '.config/crowbaros'
folder.mkdir(parents=True, exist_ok=True)
original = {{}}
for name, settings in {dict(input=saved_input, power=saved_power)!r}.items():
    path = folder / (name + '.json')
    original[name] = path.read_text() if path.exists() else None
    path.write_text(json.dumps(settings))
    path.chmod(0o600)
print(json.dumps(original))
""", person))
        boot_id = self.run('cat /proc/sys/kernel/random/boot_id').strip()
        self.run("nohup sh -c 'sleep 1; reboot' >/dev/null 2>&1 &")
        self.wait('new boot ID after clean reboot', lambda: (r := self.guest.ssh('cat /proc/sys/kernel/random/boot_id', check=False, timeout=6)).returncode == 0 and r.stdout.strip() != boot_id, timeout=180)
        self.ready()
        for kind, expected, original in [('input', saved_input, original_input),
                                         ('power', saved_power, original_power)]:
            live = self.displays({'op': kind + '_config'})
            self.check(f'saved {kind} settings take effect after a clean reboot before Settings opens',
                       all(live.get(key) == value for key, value in expected.items()
                           if key != 'schema_version'))
            restore = {key: original[key] for key in expected if key != 'schema_version'}
            self.displays({'op': 'configure_' + kind, **restore})
        self.python(f"""
import pathlib
folder = pathlib.Path({home!r}) / '.config/crowbaros'
for name, original in {original_files!r}.items():
    path = folder / (name + '.json')
    if original is None:
        path.unlink(missing_ok=True)
    else:
        path.write_text(original)
""", person)
        self.wait('the session starts locked after reboot', lambda: self.snapshot().get('locked'))
        self.unlock()
        self.wait('the password unlocks after reboot', lambda: not self.snapshot().get('locked'))
        self.check('saved preferences survive clean reboot', self.preferences() == saved)
        self.wait('the shell follows the saved appearance after the reboot',
                  lambda: self.shell_log().count('SHELL appearance theme=Light') > followed)
        self.check('the shell follows the saved appearance after the reboot', True)
        self.check("the file in the person's home survives the reboot",
                   self.run(f'cat {kept}').strip() == token)
        # Verify Quick Settings, OSD auto-dismiss, Calendar, and zero idle frames in Light theme
        qs_light = self.shell_log().count('SHELL quick-settings open=true')
        self.click(1259, 12)
        self.wait('Quick Settings opens in Light theme',
                  lambda: self.shell_log().count('SHELL quick-settings open=true') > qs_light)
        time.sleep(0.5)
        self.capture('04a-quick-settings-light')
        osd_before = self.shell_log().count('SHELL osd show=true')
        osd_off_before = self.shell_log().count('SHELL osd show=false')
        self.click(964, 207)  # Quick Settings volume "-" button (-5% -> 95%)
        self.wait('Quick Settings volume adjustment triggers OSD pill',
                  lambda: self.shell_log().count('SHELL osd show=true') > osd_before)
        time.sleep(0.2)
        self.capture('04b-osd-light')
        self.wait('OSD pill auto-dismisses',
                  lambda: self.shell_log().count('SHELL osd show=false') > osd_off_before, timeout=10)
        qs_vol = self.run(f"su -s /bin/sh {person} -c 'XDG_RUNTIME_DIR=/run/user/1003 wpctl get-volume @DEFAULT_AUDIO_SINK@'").strip()
        self.check('Quick Settings volume control mutates PipeWire and shows auto-dismissing OSD pill',
                   'Volume: 0.95' in qs_vol, qs_vol)
        self.run(f"su -s /bin/sh {person} -c 'XDG_RUNTIME_DIR=/run/user/1003 wpctl set-volume @DEFAULT_AUDIO_SINK@ 1.00'")
        self.key('esc')
        self.wait('Quick Settings closes in Light theme',
                  lambda: self.shell_log().count('SHELL quick-settings open=false') > qs_light)
        cal_light = self.shell_log().count('SHELL calendar open=true')
        self.click(640, 12)
        self.wait('Calendar opens in Light theme',
                  lambda: self.shell_log().count('SHELL calendar open=true') > cal_light)
        time.sleep(0.3)
        self.capture('04c-calendar-light')
        self.key('esc')
        self.wait('Calendar closes in Light theme',
                  lambda: self.shell_log().count('SHELL calendar open=false') > cal_light)
        self.still('a still desktop after Quick Settings, Calendar, and OSD auto-dismiss commits nothing and draws nothing')
        self.editor_check('light')
        self.monitor_check('light')
        self.viewer_check('light')
        self.calculator_check('light')
        self.theme_studio_check('light')
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
        self.click(136, 396)  # Input sidebar button
        self.wait('Settings Input page loads', lambda: 'STUDIO event=input-loaded ' in self.app_log()[log_offset:])
        self.check('Settings Input page loads live configuration from Fulcrum',
                   'STUDIO event=input-loaded live=true' in self.app_log()[log_offset:])
        self.capture('05d-settings-input-light')
        self.click(136, 444)  # Power sidebar button
        self.wait('Settings Power page loads', lambda: 'STUDIO event=power-loaded ' in self.app_log()[log_offset:])
        self.check('Settings Power page loads live idle timers from Fulcrum',
                   'STUDIO event=power-loaded live=true' in self.app_log()[log_offset:])
        self.capture('05e-settings-power-light')
        self.click(136, 492)  # Default Apps sidebar button
        self.wait('Settings Default Apps page loads', lambda: 'STUDIO event=defaults-loaded ' in self.app_log()[log_offset:])
        self.check('Settings Default Apps page loads all six categories',
                   'STUDIO event=defaults-loaded count=6' in self.app_log()[log_offset:])
        self.capture('05f-settings-defaults-light')
        self.click(136, 540)  # About / System sidebar button
        self.wait('Settings About page loads system summary', lambda: 'STUDIO event=system-loaded ' in self.app_log()[log_offset:])
        self.check('Settings About page reports live host summary',
                   'STUDIO event=system-loaded hostname=' in self.app_log()[log_offset:])
        self.capture('05g-settings-about-light')
        # Preview Dark appearance, then capture all Settings pages in Dark theme
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
        self.click(136, 396)
        time.sleep(0.3)
        self.capture('06d-settings-input-dark')
        self.click(136, 444)
        time.sleep(0.3)
        self.capture('06e-settings-power-dark')
        self.click(136, 492)
        time.sleep(0.3)
        self.capture('06f-settings-defaults-dark')
        self.click(136, 540)
        time.sleep(0.3)
        self.capture('06g-settings-about-dark')
        # Verify Files is configured as the default directory handler and launches natively from the dock
        dir_handler = self.run(f"su -s /bin/sh {person} -c 'xdg-mime query default inode/directory'").strip()
        self.check('Files is configured as the default directory handler',
                   dir_handler == 'com.crowbaros.Files.desktop',
                   f'inode/directory={dir_handler!r}')
        # Verify user mimeapps.list overrides xdg-mime queries immediately and restores cleanly
        person_home = self.person()[1]
        mime_roundtrip = json.loads(self.python(f"""
import json, os, pathlib, subprocess
home = {person_home!r}
cfg = pathlib.Path(home) / '.config/mimeapps.list'
had = cfg.exists()
orig = cfg.read_text() if had else None
env = dict(os.environ, HOME=home)
try:
    cfg.parent.mkdir(parents=True, exist_ok=True)
    cfg.write_text('[Default Applications]\\ntext/plain=foot.desktop\\n')
    after = subprocess.check_output(['xdg-mime', 'query', 'default', 'text/plain'], text=True, env=env).strip()
    print(json.dumps({{'after': after}}))
finally:
    if had:
        cfg.write_text(orig)
    elif cfg.exists():
        cfg.unlink()
""", person))
        self.check('user mimeapps.list updates xdg-mime default queries immediately',
                   mime_roundtrip['after'] == 'foot.desktop',
                   str(mime_roundtrip))
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
        self.agent_drawer_thread()

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
        # A themed terminal can have the drawer's exact ground. Give this
        # visual probe a distinct background before testing their stacking.
        self.type(AGENT_TERMINAL_SETUP)
        self.key('ret')
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
            # The empty output and drawer may share the theme's canvas.
            # Compare the header's content against the pre-summon display.
            region = ((1280 if head == 0 else 1024) - width + 16, 12, 180, 130)
            baseline = self.region(f'17-before-{output}', *region, head=head)
            shows = self.count(shown)
            told = summon(opened, output=output)
            self.wait(f'the Agent\'s surface shows on {output}', lambda: self.count(shown) > shows)
            self.wait('the Agent\'s surface takes the keyboard', lambda: not active())
            placed = self.run(f"grep -e '{shown}' /var/log/crowbar-compositor.log | tail -1").strip()
            asked = f'SHELL agent-surface up=true output={output} width={width}'
            self.check(f'with two displays, Super+Space brings the Agent\'s surface up on {output}, where the pointer is',
                       told and f'output={output} {place}' in placed and asked in self.shell_log()
                       and drawn(f'17-summoned-on-{output}', head, point)
                       and self.region(f'17-content-on-{output}', *region, head=head) != baseline, placed)
            self.capture(f'17-other-display-while-on-{output}', 1 - head)
            summon(away, output=output, keyboard=True)
            self.wait('the Agent\'s surface is put away',
                      lambda: self.region(f'17-put-away-on-{output}', *region, head=head) == baseline)

    def agent_reached_lever_idle(self):
        """With Lever running from boot (ADR 0036) configured with OpenCode,
        summoning the Agent's drawer shows that the shell reached Lever and
        that an idle Lever has started no OpenCode."""
        self.key('meta_l', 'spc')
        view = self.wait('drawer opens connected to idle Lever',
                         lambda: (v := self.agent_view()) and drawer_reached_lever_idle(v) and v)
        time.sleep(0.5)
        self.capture('18-drawer-lever-idle')
        self.check('the drawer shows the Agent is reachable, Lever is idle with OpenCode configured, and first summon asks where Models come from',
                   drawer_reached_lever_idle(view)
                   and view['state'].get('models_chosen') is False
                   and any(c['id'] == 'choose-free-models' for c in view.get('controls', []))
                   and any(c['id'] == 'sign-in' for c in view.get('controls', []))
                   and not any(c['id'] == 'send' for c in view.get('controls', []))
                   and 'SHELL agent-socket connected=true' in self.shell_log(),
                   json.dumps(view))
        self.key('meta_l', 'spc')
        self.wait('drawer closes after Lever status check',
                  lambda: not self.agent_view()['drawer_up'])

    def agent_drawer_thread(self):
        """End-to-end drawer thread on Lever + OpenCode against the local stub
        Model (#376, #378): a new thread in Ask first raising a request with
        the exact command, which waits with the drawer put away and the
        desktop locked while the mark shows waiting, and is answered once in
        the drawer; a no refused with the Model told; Full access and the
        starting mode from the drawer's mode menu, where the same command
        runs with no request; desktop context without window or clipboard
        contents, command output in an act, streamed answer, work continuing
        behind a put-away drawer with bar and fullscreen marks, Stop ending
        the turn and its spawned commands, failed turn reason, shell restart
        redrawing the thread from Lever, a command the person's own
        OpenCode settings refuse refused in Ask first with no request, and
        the person's own OpenCode settings left as they made them and free of
        Lever's additions."""
        person, home = self.person()
        click_v1 = lambda x, y: (self.qmp.pointer(x / (1280 + 1024), y / 960), self.qmp.click())
        window_secret = 'WINSECRET_' + os.urandom(6).hex()
        clipboard_secret = 'CLIPSECRET_' + os.urandom(6).hex()
        tmp = '/tmp/crowbar-drawer-check'
        self.run(f'rm -rf {tmp} && mkdir -p {tmp}')
        stub_script = {
            'models': ['stub-1', 'spare-1'],
            'rules': [
                {
                    'match': 'show-markdown',
                    'reply': (
                        '## What I found\n\n'
                        'The check ran **twice** and `echo` printed its mark. See '
                        '[the lab guide](https://example.org/lab) for more.\n\n'
                        '1. Build the image\n'
                        '2. Boot a copy of it\n'
                        '   - under UEFI\n\n'
                        '```sh\nlab/void/desktop_check.py\n```\n\n'
                        '> A turn that ends folds its acts into one line.\n\n'
                        'MARKDOWN_OK'
                    ),
                },
                {
                    'match': 'run-cmd',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': 'echo DRAWER_CMD_OK && id -un',
                            'description': 'Echo marker and current person',
                        },
                    },
                    'after_tool': 'Command finished on {model}: {tool_output}',
                },
                {
                    'match': 'no-cmd',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': 'echo NO_CMD_RAN',
                            'description': 'A command the person refuses',
                        },
                    },
                    'after_tool': 'Should not run: {tool_output}',
                },
                {
                    'match': 'slow-bg',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': 'sleep 4 && echo BG_DONE_OK',
                            'description': 'Run background sleep command',
                        },
                    },
                    'after_tool': 'Background finished: {tool_output}',
                },
                {
                    'match': 'recovery-slow',
                    'tool_call': {'name': 'shell', 'arguments': {
                        'command': 'sleep 15 && echo RECOVERY_DONE',
                        'description': 'Keep working across a desktop restart',
                    }},
                    'after_tool': 'Recovered: {tool_output}',
                },
                {
                    'match': 'long-stop',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': "sh -c 'sleep 45 & wait'",
                            'description': 'Long command to be stopped',
                        },
                    },
                    'after_tool': 'Should not finish',
                },
                {
                    'match': 'denied-cmd',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': 'echo DENIED_$((40 + 2))',
                            'description': 'A command the person\'s own settings refuse',
                        },
                    },
                    'after_tool': 'Refused: {tool_output}',
                },
                {
                    # The Model's answer that calls the command is held, so
                    # the turn still works when the person sends again.
                    'match': 'steer-hold',
                    'hold_until': f'{tmp}/steer-release',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': 'echo STEER_HELD_RAN',
                            'description': 'A command the turn is held on',
                        },
                    },
                    'after_tool': 'Should not answer: the person said more',
                },
                {
                    'match': 'steer-also',
                    'reply': 'STEER_TAKEN_OK after {tool_output}',
                },
                {
                    'match': 'make-fail',
                    'error': {
                        'status': 400,
                        'message': 'Stub model declined this turn',
                    },
                },
                {
                    'match': '<background-result',
                    'reply': 'The subagent finished its work.',
                },
                {
                    'match': 'subagent-child-read',
                    # The subagent asks only once its parent's turn is done,
                    # so the check sees a subagent outlive its turn and ask.
                    'hold_until': f'{tmp}/subagent-parent-done',
                    'tool_call': {
                        'name': 'read',
                        'arguments': {
                            'path': f'{home}/.env',
                        },
                    },
                    'after_tool': 'The env file says: {tool_output}',
                },
                {
                    'match': 'subagent-check',
                    'tool_call': {
                        'name': 'subagent',
                        'arguments': {
                            'agent': 'general',
                            'description': 'Inspect env file',
                            'prompt': 'Please run subagent-child-read to read the env file.',
                        },
                    },
                    'after_tool': 'Started a subagent to inspect the env file.',
                },
                {
                    'match': 'hand-session-check',
                    # Held, so the session is listed while its first turn works.
                    'hold_until': f'{tmp}/hand-release',
                    'tool_call': {
                        'name': 'shell',
                        'arguments': {
                            'command': 'echo HAND_SESSION_CMD_OK',
                            'description': 'Echo marker from terminal session',
                        },
                    },
                    'after_tool': 'Terminal command output: {tool_output}',
                },
                {
                    'match': 'hand-followup',
                    'reply': 'Continued from drawer on {model}.',
                },
            ],
            'default_reply': 'Stub ({model}): {prompt}',
        }
        try:
            # Put unique secrets in the terminal window and live clipboard to
            # prove neither crosses into desktop context or reaches the Model.
            foot = next((c for c in self.snapshot()['clients'] if c['class'] == 'foot'), None)
            if foot is not None:
                click_v1(680, 935)
                self.wait('foot focused for window secret',
                          lambda: self.snapshot()['active']['address'] == foot['address'])
                self.type(f'echo {window_secret}')
                self.key('ret')
                time.sleep(0.3)
            self.as_app(f'crowbar-probe copy {clipboard_secret}', f'{tmp}/clip.out')
            self.wait('clipboard secret copied',
                      lambda: 'copied' in self.guest.ssh(f'cat {tmp}/clip.out', check=False).stdout)
            if foot is not None:
                click_v1(680, 935)
                self.wait('foot refocused before summoning drawer',
                          lambda: self.snapshot()['active']['address'] == foot['address'])

            # Start stub Model before the first-summon choice is answered so we
            # can hold that no request reaches it until the person chooses.
            self.run(f"cat > {tmp}/script.json <<'EOF'\n{json.dumps(stub_script, indent=2)}\nEOF")
            self.run(
                f"setsid -f python3 /usr/local/lib/crowbar-lab/stub-model.py "
                f"--script {tmp}/script.json --port-file {tmp}/port --log {tmp}/stub.jsonl "
                f"--forbid {shlex.quote(window_secret)} --forbid {shlex.quote(clipboard_secret)} "
                f">{tmp}/stub.out 2>&1 </dev/null"
            )
            port = self.wait('stub Model port ready',
                             lambda: self.guest.ssh(f'cat {tmp}/port', check=False).stdout.strip())

            def control(view, cid):
                return next((c for c in view.get('controls', []) if c['id'] == cid), None)

            def click_control(cid, timeout=20):
                c = self.wait(f'the drawer shows {cid}', lambda: control(self.agent_view(), cid), timeout=timeout)
                click_v1(c['x'] + c['w'] / 2, c['y'] + c['h'] / 2)

            def waiting_request(view):
                return next((r for t in view['state']['turns'] for r in t.get('requests', [])
                             if r['state'] == 'waiting'), None)

            def listed(view):
                """The list open above the drawer's message box, or none."""
                return view.get('list') or {}

            def arrow_to(key, target, what='highlighted'):
                """Presses an arrow key until the open list is on `target`. A
                list goes round its ends, so a key the guest repeats, held as
                long as the lab holds one, still comes to it."""
                for _ in range(24):
                    if listed(self.agent_view()).get(what) == target:
                        return True
                    self.key(key)
                    time.sleep(0.3)
                return listed(self.agent_view()).get(what) == target

            def empty_the_box():
                """Takes out what the message box holds, a character at a time."""
                for _ in self.agent_view().get('draft', ''):
                    self.key('backspace')
                self.wait('the message box is empty', lambda: self.agent_view().get('draft') == '')

            def stub_requests():
                raw = self.guest.ssh(f'cat {tmp}/stub.jsonl 2>/dev/null || true', check=False).stdout
                return [json.loads(line) for line in raw.splitlines() if line.strip()]

            def refresh_lever_models():
                """Runs the exact `models` refresh `crowbar-agent login` sends on
                Lever's person socket when sign-in ends."""
                script = (
                    "import json, socket, struct\n"
                    f"s = socket.socket(socket.AF_UNIX); s.settimeout(60)\n"
                    f"s.connect('/run/user/{void_base.PERSON_UID}/crowbar-lever/person')\n"
                    "def tx(obj):\n"
                    "    b = json.dumps(obj).encode()\n"
                    "    s.sendall(struct.pack('>I', len(b)) + b)\n"
                    "def rx():\n"
                    "    h = b''\n"
                    "    while len(h) < 4: h += s.recv(4 - len(h))\n"
                    "    n = struct.unpack('>I', h)[0]\n"
                    "    b = b''\n"
                    "    while len(b) < n: b += s.recv(n - len(b))\n"
                    "    return json.loads(b)\n"
                    "tx({'version': 1}); rx()\n"
                    "tx({'op': 'models', 'refresh': True}); print(json.dumps(rx()))\n"
                )
                return json.loads(self.run(f"chpst -u {person} python3 -c {shlex.quote(script)}"))

            def write_opencode_cfg(include_spare):
                providers = {
                    'stub': {
                        'npm': '@ai-sdk/openai-compatible',
                        'name': 'Stub',
                        'options': {'baseURL': f'http://127.0.0.1:{port}/v1', 'apiKey': 'stub-key'},
                        'models': {'stub-1': {'name': 'Stub One'}},
                    },
                }
                enabled = ['stub']
                if include_spare:
                    # A second provider, so the Model picker has a rail.
                    providers['spare'] = {
                        'npm': '@ai-sdk/openai-compatible',
                        'name': 'Spare',
                        'options': {'baseURL': f'http://127.0.0.1:{port}/v1', 'apiKey': 'stub-key'},
                        'models': {'spare-1': {'name': 'Spare One'}},
                    }
                    enabled.append('spare')
                cfg = {
                    'provider': providers,
                    'model': 'stub/stub-1',
                    'enabled_providers': enabled,
                    # A refusal of the person's own, which Ask first must keep.
                    'permission': {'bash': {'echo DENIED_*': 'deny'}},
                }
                self.run(f"install -d -o {person} -g {person} -m 700 {home}/.config/opencode")
                self.run(f"cat > {home}/.config/opencode/opencode.json <<'EOF'\n{json.dumps(cfg, indent=2)}\nEOF")
                self.run(f"chown {person}:{person} {home}/.config/opencode/opencode.json")

            # 0. First summon (#379): with no Model chosen, the drawer shows the
            # choice, sends nothing, and no request reaches the stub Model.
            self.key('meta_l', 'spc')
            first = self.wait(
                'first summon shows the Model source choice',
                lambda: (v := self.agent_view()) and v['drawer_up']
                and control(v, 'choose-free-models') and control(v, 'sign-in') and v,
            )
            time.sleep(0.4)
            self.capture('18a-drawer-first-summon')
            self.type('hello before choice')
            self.key('ret')
            time.sleep(0.5)
            after_type = self.agent_view()
            self.check(
                'with no Model chosen, the drawer shows the choice, sends nothing, and no request reaches the stub Model',
                first['state'].get('models_chosen') is False
                and not control(first, 'send')
                and after_type['state'].get('turns') == []
                and not after_type['state'].get('sending')
                and after_type['state'].get('harness_started') is False
                and stub_requests() == [],
                json.dumps({'state': after_type['state'], 'stub_requests': len(stub_requests())}),
            )

            # Choosing free hosted Models persists in Lever's settings and can be
            # changed back from the drawer.
            click_control('choose-free-models')
            free_on = self.wait(
                'choosing free hosted Models enables the setting and opens the composer',
                lambda: (v := self.agent_view()) and v['state'].get('free_hosted_models') is True
                and v['state'].get('models_chosen') is True and control(v, 'send') and v,
            )
            cli_free_on = self.run(
                f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} '
                f'crowbar-agent free-models'
            ).strip()
            self.type('/free-models off')
            self.key('ret')
            free_off = self.wait(
                'changing free hosted Models off from the drawer updates Lever and returns to the choice',
                lambda: (v := self.agent_view()) and v['state'].get('free_hosted_models') is False
                and v['state'].get('models_chosen') is False and control(v, 'sign-in') and v,
            )
            cli_free_off = self.run(
                f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} '
                f'crowbar-agent free-models'
            ).strip()
            self.check(
                "choosing free hosted Models is kept in Lever's settings and can be changed later from the drawer",
                free_on['state'].get('free_hosted_models') is True
                and 'on' in cli_free_on
                and free_off['state'].get('free_hosted_models') is False
                and 'off' in cli_free_off
                and stub_requests() == [],
                f'on={cli_free_on!r} off={cli_free_off!r}',
            )

            # Sign in opens Base's desktop entry in foot via crowbar-launcher,
            # and when sign-in ends the drawer shows the new Models with no
            # restart of Lever or the shell (including adding a second provider
            # when the catalog is already non-empty).
            lever_pid_before = self.run('pgrep -x crowbar-lever').strip()
            shell_pid_before = self.run('pgrep -x crowbaros-shell').strip()
            click_control('sign-in')
            sign_in_win = self.wait(
                "Sign in opens Base's sign-in desktop entry in foot",
                lambda: next((c for c in self.snapshot()['clients']
                              if c.get('class') == 'com.crowbaros.AgentSignIn'), None),
                timeout=20,
            )
            sign_in_proc = self.wait(
                'OpenCode sign-in is running in foot',
                lambda: self.guest.ssh("pgrep -af 'crowbar-agent login|opencode auth login'", check=False).stdout.strip(),
                timeout=15,
            )
            time.sleep(0.5)
            self.capture('18b-drawer-sign-in-terminal')
            self.check(
                "the drawer's Sign in control opens Base's desktop entry in foot on OpenCode's sign-in",
                bool(sign_in_win) and bool(sign_in_proc) and stub_requests() == [],
                f'win={sign_in_win!r} proc={sign_in_proc[:120]!r}',
            )
            # Complete sign-in for the lab stub: first with one provider, then
            # adding a second provider, refreshing Lever without restarting
            # Lever or the shell.
            write_opencode_cfg(include_spare=False)
            self.guest.ssh("pkill -f 'opencode auth login'; pkill -f 'crowbar-agent login'; true", check=False)
            self.wait(
                'sign-in terminal closes after sign-in ends',
                lambda: not any(c.get('class') == 'com.crowbaros.AgentSignIn'
                                for c in self.snapshot()['clients']),
                timeout=15,
            )
            refresh_lever_models()
            after_first_signin = self.wait(
                'drawer shows the new Model live with no restart of Lever or the shell',
                lambda: (v := self.agent_view()) and v['state'].get('models_chosen') is True
                and [m['id'] for m in v['state'].get('models', [])] == ['stub/stub-1']
                and control(v, 'send') and v,
                timeout=45,
            )
            # Adding a second provider refreshes an already non-empty catalog live.
            write_opencode_cfg(include_spare=True)
            refresh_lever_models()
            after_second_signin = self.wait(
                'drawer shows the added provider live in its non-empty catalog',
                lambda: (v := self.agent_view())
                and {m['id'] for m in v['state'].get('models', [])} == {'stub/stub-1', 'spare/spare-1'} and v,
                timeout=45,
            )
            time.sleep(0.3)
            self.capture('18c-drawer-after-sign-in')
            lever_pid_after = self.run('pgrep -x crowbar-lever').strip()
            shell_pid_after = self.run('pgrep -x crowbaros-shell').strip()
            self.check(
                'when sign-in ends, the drawer shows the new Models with no restart of Lever or the shell',
                lever_pid_after == lever_pid_before
                and shell_pid_after == shell_pid_before
                and [m['id'] for m in after_first_signin['state'].get('models', [])] == ['stub/stub-1']
                and {m['id'] for m in after_second_signin['state'].get('models', [])} == {'stub/stub-1', 'spare/spare-1'}
                and stub_requests() == [],
                f'lever={lever_pid_before}->{lever_pid_after} shell={shell_pid_before}->{shell_pid_after}',
            )
            settings_before = self.run(f'sha256sum {home}/.config/opencode/opencode.json').split()[0]
            # Close the drawer and refocus foot before starting the thread checks
            # so turn 1's desktop context captures foot as the active window.
            click_control('close')
            self.wait('drawer closes before thread check', lambda: not self.agent_view()['drawer_up'])
            if foot is not None:
                click_v1(680, 935)
                self.wait('foot refocused before thread check',
                          lambda: self.snapshot()['active']['address'] == foot['address'])

            # 1. A new thread starts in Ask first: the command waits on a
            # request with the exact command, through the drawer put away
            # and the desktop locked, with the mark showing waiting, and runs
            # once it is answered once in the drawer.
            self.key('meta_l', 'spc')
            self.wait('drawer opens for thread check', lambda: self.agent_view()['drawer_up'])
            self.key('ctrl', 'n')
            self.wait('new thread composer opens', lambda: not self.agent_view()['state']['list_view'])
            self.type('please run-cmd')
            self.key('ret')
            # The request is in the thread once the drawer has drawn it,
            # with a control for each answer.
            asked = self.wait(
                'the command raises a request in the thread',
                lambda: (v := self.agent_view()) and waiting_request(v)
                and control(v, f"answer:{waiting_request(v)['request']}:once") and v,
                timeout=45,
            )
            time.sleep(0.5)
            self.capture('19a-drawer-request-command')
            request = waiting_request(asked)
            self.check(
                'a new thread starts in Ask first, and its command waits on a request with the exact command and '
                'the answers once, always, and no',
                asked['state'].get('mode') == 'ask-first'
                and request['kind'] == 'command'
                and request['detail'] == 'echo DRAWER_CMD_OK && id -un'
                and [a['answer'] for a in request['answers']] == ['once', 'always', 'no']
                # An act's detail is its command until it has run: none has finished.
                and not any(a.get('state') == 'done' for a in asked['state']['turns'][0].get('acts', []))
                and bool(control(asked, f"answer:{request['request']}:once")),
                json.dumps(request),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer put away while the request waits', lambda: not self.agent_view()['drawer_up'])
            time.sleep(0.5)
            self.capture('19b-bar-mark-waiting')
            away = self.agent_view()
            self.check('with the drawer put away, the mark shows waiting',
                       away['state']['mark'] == 'waiting' and not away['drawer_up'], away['state']['mark'])
            self.key('meta_l', 'l')
            self.wait('Super+L locks with a request waiting', lambda: self.snapshot().get('locked'))
            time.sleep(1.5)
            locked = self.agent_view()
            self.unlock()
            self.wait('the password unlocks', lambda: not self.snapshot().get('locked'))
            time.sleep(1)
            unlocked = self.agent_view()
            self.check(
                'a request waits through the drawer put away and the desktop locked, and the drawer does not come '
                'up on its own',
                waiting_request(locked) and waiting_request(unlocked)
                and waiting_request(unlocked)['request'] == request['request']
                and not locked['drawer_up'] and not unlocked['drawer_up']
                and unlocked['state']['mark'] == 'waiting',
                json.dumps(unlocked['state']['mark']),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer opens on the waiting request',
                      lambda: (v := self.agent_view()) and v['drawer_up']
                      and control(v, f"answer:{request['request']}:once"))
            time.sleep(0.5)
            self.capture('19c-drawer-request-after-lock')
            click_control(f"answer:{request['request']}:once")
            view1 = self.wait(
                'turn 1 finishes with command output and answer',
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 1
                and v['state']['turns'][0]['state']['kind'] == 'done'
                and v,
                timeout=45,
            )
            time.sleep(0.5)
            self.capture('19-drawer-thread-answer')
            turn1 = view1['state']['turns'][0]
            thread_id = view1['state']['thread']
            stub_raw = self.run(f'cat {tmp}/stub.jsonl')
            stub_entries = [json.loads(line) for line in stub_raw.splitlines() if line.strip()]
            tool_prompts = [e['prompt'] for e in stub_entries if e.get('has_tools') and 'please run-cmd' in e.get('prompt', '')]
            ctx1 = turn1.get('context') or {}
            cmd_acts = [a for a in turn1.get('acts', []) if a.get('state') == 'done' and 'DRAWER_CMD_OK' in (a.get('detail') or '')]
            self.check(
                'drawer message reaches stub Model with desktop context and no window or clipboard contents',
                bool(tool_prompts)
                and '<desktop-context>' in tool_prompts[0]
                and '[Desktop context' in tool_prompts[0]
                and 'app_id: foot' in tool_prompts[0]
                and 'Virtual-1' in tool_prompts[0]
                and not any(e.get('forbidden_found') for e in stub_entries)
                and window_secret not in stub_raw
                and clipboard_secret not in stub_raw
                and (ctx1.get('active') or {}).get('app_id') == 'foot',
                f'prompts={len(tool_prompts)} ctx_active={ctx1.get("active")}',
            )
            self.check(
                'drawer shows the streamed answer and the command act output',
                bool(thread_id)
                and bool(cmd_acts)
                and person in (cmd_acts[0].get('detail') or '')
                and 'DRAWER_CMD_OK' in turn1.get('answer', ''),
                json.dumps(turn1),
            )

            self.check(
                'answering once runs the command, and the thread records the request answered',
                (turn1.get('requests') or [{}])[0].get('state') == 'answered'
                and (turn1.get('requests') or [{}])[0].get('answer') == 'once',
                json.dumps(turn1.get('requests')),
            )

            # Typing here without clicking the composer also verifies that
            # answering a permission request restores the composer's focus.
            # 1b. No: the command is refused, the harness ends the turn as it
            # has it, and the Model is told on the next turn.
            self.type('please no-cmd')
            self.key('ret')
            refusal = self.wait('the second command raises a request',
                                lambda: (v := self.agent_view()) and len(v['state']['turns']) >= 2
                                and waiting_request(v) and v, timeout=45)
            # Answered from the keyboard: Alt and a digit take the answer at
            # that place of the request that has waited longest. No is third.
            self.key('alt', '3')
            refused = self.wait(
                'turn 2 ends after the person said no',
                lambda: (v := self.agent_view()) and v['state']['turns'][1]['state']['kind'] != 'working' and v,
                timeout=30,
            )
            time.sleep(0.4)
            self.capture('19d-drawer-request-refused')
            turn2 = refused['state']['turns'][1]
            self.check(
                'no refuses the command: it does not run, and the thread says so',
                (turn2.get('requests') or [{}])[0].get('answer') == 'no'
                and turn2['state']['kind'] in ('stopped', 'done')
                and not any(a.get('state') == 'done' for a in turn2.get('acts', []))
                and 'Should not run' not in turn2.get('answer', ''),
                json.dumps(turn2),
            )

            # The message box holds many lines, and keeps what was typed and
            # not sent while the drawer is put away.
            self.type('line one')
            self.key('shift', 'ret')
            self.type('line two')
            held = self.wait('Shift+Enter begins a new line',
                             lambda: (v := self.agent_view()) and v.get('draft') == 'line one\nline two' and v)
            self.key('meta_l', 'spc')
            self.wait('drawer put away with words in the message box', lambda: not self.agent_view()['drawer_up'])
            self.key('meta_l', 'spc')
            kept = self.wait('the drawer comes back with the words',
                             lambda: (v := self.agent_view()) and v['drawer_up'] and v.get('draft') and v)
            time.sleep(0.4)
            self.capture('19e-drawer-draft-kept')
            self.check(
                'the message box holds many lines, Shift+Enter sends nothing, and what was typed and not sent is '
                'there when the drawer is summoned again',
                kept.get('draft') == 'line one\nline two' and len(held['state']['turns']) == 2
                and len(kept['state']['turns']) == 2,
                json.dumps(kept.get('draft')),
            )
            empty_the_box()

            # 1c. The thread is driven from the message box. A slash and the
            # start of a name find a command of the harness's, and taking it
            # puts it in the message box for the person to add to and send.
            self.type('/rev')
            found = self.wait("typing finds a command of the harness's",
                              lambda: (v := self.agent_view()) and listed(v).get('highlighted') == 'command:review'
                              and control(v, 'command:review') and v, timeout=60)
            time.sleep(0.4)
            self.capture('20-drawer-harness-command')
            self.key('ret')
            filled = self.wait('taking it puts it in the message box',
                               lambda: (v := self.agent_view()) and v.get('draft') == '/review ' and v)
            self.check(
                "a command the harness offers is found by its name, and taking it puts it in the message box and "
                'sends nothing',
                listed(found)['items'][0] == 'command:review' and not filled.get('list')
                and len(filled['state']['turns']) == 2 and not filled['state'].get('sending'),
                json.dumps({'found': listed(found)['items'], 'draft': filled.get('draft')}),
            )
            empty_the_box()
            # A slash alone lists the drawer's commands and then the
            # harness's own.
            self.type('/')
            commands = self.wait(
                "a slash lists the commands, the harness's among them",
                lambda: (v := self.agent_view()) and listed(v).get('kind') == 'commands'
                and 'command:compact' in listed(v).get('items', []) and control(v, 'command:mode') and v,
                timeout=60,
            )
            time.sleep(0.4)
            self.capture('20a-drawer-commands')
            items = listed(commands)['items']
            own = ['command:model', 'command:effort', 'command:mode', 'command:default-mode',
                   'command:free-models', 'command:sign-in', 'command:new',
                   'command:threads', 'command:archive', 'command:delete']
            self.check(
                "a slash lists the drawer's commands, then the commands the harness offers, and the keys start "
                'on the first',
                items[:len(own)] == own
                and {'command:compact', 'command:review', 'command:init'} <= set(items[len(own):])
                and listed(commands).get('highlighted') == 'command:model'
                and [c['name'] for c in commands['state'].get('thread_commands') or commands['state']['commands']]
                == [i.split(':', 1)[1] for i in items[len(own):]],
                json.dumps(items),
            )
            # Typing narrows the list. Escape closes it and keeps what was typed.
            self.type('mo')
            narrowed = self.wait('typing narrows the list',
                                 lambda: (v := self.agent_view()) and listed(v).get('items', [])[:3]
                                 == ['command:model', 'command:mode', 'command:default-mode'] and v)
            self.key('esc')
            closed = self.wait('Escape closes the list', lambda: (v := self.agent_view()) and not v.get('list') and v)
            self.check('typing narrows the list of commands, and Escape closes it and keeps what was typed',
                       'command:new' not in listed(narrowed)['items'] and closed.get('draft') == '/mo'
                       and closed['drawer_up'],
                       json.dumps({'narrowed': listed(narrowed)['items'], 'draft': closed.get('draft')}))
            # The next key brings the list back. Down and Enter take /mode,
            # which lists the modes the harness offers in the same place.
            self.type('d')
            self.wait('the list comes back as the person types on',
                      lambda: listed(self.agent_view()).get('highlighted') == 'command:model')
            self.check('Down moves through the list of commands', arrow_to('down', 'command:mode'))
            self.key('ret')
            modes = self.wait("/mode lists the harness's modes",
                              lambda: (v := self.agent_view()) and listed(v).get('kind') == 'modes'
                              and len(listed(v).get('items', [])) == 3 and control(v, 'choice:full-access') and v,
                              timeout=60)
            time.sleep(0.4)
            self.capture('20b-drawer-mode-list')
            self.check(
                'taking /mode lists Ask first, Full access, and Plan, and no others, starting on the mode the '
                'thread is in',
                listed(modes)['items'] == ['choice:ask-first', 'choice:full-access', 'choice:plan']
                and listed(modes)['highlighted'] == 'choice:ask-first'
                and modes.get('draft') == '/mode '
                and [m['id'] for m in modes['state'].get('modes', [])] == ['ask-first', 'full-access', 'plan'],
                json.dumps(listed(modes)),
            )
            self.check('Down moves through the list of choices', arrow_to('down', 'choice:full-access'))
            self.key('ret')
            picked = self.wait('the thread is in Full access',
                               lambda: (v := self.agent_view()) and v['state'].get('mode') == 'full-access'
                               and not v.get('list') and v)
            self.check('picking a mode from the list sets the thread to it and empties the message box',
                       picked.get('draft') == '' and not control(picked, 'mode'), json.dumps(picked.get('draft')))
            # A command typed whole needs no list.
            self.type('/default-mode full-access')
            self.key('ret')
            self.wait('new threads start in Full access',
                      lambda: self.agent_view()['state'].get('default_mode') == 'full-access')
            # One the thread cannot take says why, and changes nothing.
            self.type('/mode nonesuch')
            self.key('ret')
            refused = self.wait('a mode the harness does not offer is refused',
                                lambda: (v := self.agent_view()) and v['state'].get('error') and v)
            self.check('a command the thread cannot take says why and changes nothing',
                       'No mode by that name' in refused['state']['error']
                       and refused['state'].get('mode') == 'full-access' and refused.get('draft') == '/mode nonesuch',
                       refused['state']['error'])
            empty_the_box()

            # 1d. /model lists the Models the harness offers: the providers
            # on a rail, the Models of one, and a search through all of them.
            self.type('/model ')
            picker = self.wait(
                '/model lists the Models of the provider the thread is at',
                lambda: (v := self.agent_view()) and listed(v).get('kind') == 'models'
                and listed(v).get('items') == ['choice:stub/stub-1'] and control(v, 'provider:spare') and v,
                timeout=60,
            )
            time.sleep(0.4)
            self.capture('20c-drawer-model-picker')
            self.check(
                "the Model picker opens on the thread's Model, with each provider on the rail",
                listed(picker).get('provider') == 'stub'
                and listed(picker).get('highlighted') == 'choice:stub/stub-1'
                and bool(control(picker, 'provider:stub'))
                and {(m['id'], m['name'], m.get('provider')) for m in picker['state']['models']}
                == {('stub/stub-1', 'Stub One', 'stub'), ('spare/spare-1', 'Spare One', 'spare')},
                json.dumps({'list': listed(picker), 'models': picker['state']['models']}),
            )
            arrow_to('right', 'spare', 'provider')
            other = self.wait('Right moves to the next provider',
                              lambda: (v := self.agent_view()) and listed(v).get('provider') == 'spare' and v)
            self.type('one')
            found = self.wait('a search looks through every provider',
                              lambda: (v := self.agent_view()) and len(listed(v).get('items', [])) == 2 and v)
            time.sleep(0.4)
            self.capture('20d-drawer-model-search')
            self.check(
                'the rail moves between providers, and a search looks through all of them with no rail',
                listed(other).get('items') == ['choice:spare/spare-1']
                and set(listed(found)['items']) == {'choice:stub/stub-1', 'choice:spare/spare-1'}
                and not control(found, 'provider:spare') and listed(found).get('provider') is None,
                json.dumps({'other': listed(other), 'found': listed(found)}),
            )
            empty_the_box()
            self.type('/model spare')
            self.wait('the search finds one Model', lambda: listed(self.agent_view()).get('items') == ['choice:spare/spare-1'])
            self.key('ret')
            modeled = self.wait('the thread has the Model picked',
                                lambda: (v := self.agent_view()) and v['state'].get('model') == 'spare/spare-1' and v)
            # The Model has effort levels, as OpenCode gives a Model of this
            # kind: the drawer lists them next, on the one the thread is at.
            levels = self.wait("the picked Model's effort levels are listed next",
                               lambda: (v := self.agent_view()) and listed(v).get('kind') == 'efforts'
                               and listed(v).get('items') and v, timeout=30)
            time.sleep(0.4)
            self.capture('20e-drawer-effort-list')
            self.check(
                'the drawer shows the thread on the Model picked as soon as Lever says so, and then lists that '
                "Model's effort levels, on the one the thread is at",
                modeled['state'].get('model') == 'spare/spare-1' and modeled['drawer_up']
                and levels.get('draft') == '/effort '
                and listed(levels)['items'] == ['choice:low', 'choice:medium', 'choice:high', 'choice:default']
                and listed(levels)['highlighted'] == 'choice:default'
                and levels['state'].get('effort') == 'default',
                json.dumps(listed(levels)),
            )
            self.check('Up moves to the level before', arrow_to('up', 'choice:high'))
            self.key('ret')
            self.wait('the thread is at the effort picked',
                      lambda: (v := self.agent_view()) and v['state'].get('effort') == 'high' and not v.get('list'))
            self.type('please run-cmd again')
            self.key('ret')
            full = self.wait(
                'turn 3 runs the command in Full access',
                lambda: (v := self.agent_view()) and len(v['state']['turns']) >= 3
                and v['state']['turns'][2]['state']['kind'] == 'done' and v,
                timeout=45,
            )
            turn3 = full['state']['turns'][2]
            stub_after = [json.loads(line) for line in self.run(f'cat {tmp}/stub.jsonl').splitlines() if line.strip()]
            told = [e for e in stub_after if e.get('has_tools') and 'please run-cmd again' in e.get('prompt', '')]
            self.check(
                'in Full access the same command runs with no request, and the Model was told the person said no',
                not turn3.get('requests')
                and any('DRAWER_CMD_OK' in (a.get('detail') or '') for a in turn3.get('acts', []))
                and bool(told)
                and any('declined' in out for out in told[0].get('history_tool_outputs', [])),
                json.dumps({'turn': turn3, 'told': told[:1]}),
            )
            self.check('the turn after a Model and an effort are picked runs on that Model, at that effort',
                       bool(told) and told[0].get('model') == 'spare-1' and 'spare-1' in turn3.get('answer', '')
                       and told[0].get('effort') == 'high' and full['state'].get('effort') == 'high',
                       json.dumps({'model': told[0].get('model') if told else None,
                                   'effort': told[0].get('effort') if told else None,
                                   'thread': full['state'].get('effort'), 'answer': turn3.get('answer')}))
            self.check('the drawer\'s choices stick: the thread is in Full access and so is the starting mode',
                       full['state'].get('mode') == 'full-access' and full['state'].get('default_mode') == 'full-access')

            # 2. Put the drawer away while a turn works; verify work keeps going
            # and the mark shows working and done in the bar and over fullscreen.
            self.type('please slow-bg')
            self.key('ret')
            self.wait('turn 4 starts working',
                      lambda: (v := self.agent_view()) and v['state']['mark'] == 'working' and len(v['state']['turns']) >= 4)
            self.key('meta_l', 'spc')
            self.wait('drawer put away while turn 4 works',
                      lambda: not self.agent_view()['drawer_up'])
            bg_bar = self.agent_view()
            self.capture('20-bar-mark-working')
            self.check('putting the drawer away stops nothing and the bar mark shows working',
                       not bg_bar['drawer_up'] and bg_bar['state']['mark'] == 'working',
                       json.dumps(bg_bar['state']['mark']))

            def overlay_region(name):
                self.capture(name)
                _, _, rows = guest.png_pixels(self.work / f'{name}.png')
                return b''.join(rows[y][1150 * 3:1276 * 3] for y in range(4, 40))

            self.key('meta_l', 'f')
            fs_working = self.wait(
                'fullscreen mark overlay shows working',
                lambda: (v := self.agent_view()) and v['mark_overlay'] and v['state']['mark'] == 'working' and v,
                timeout=15,
            )
            time.sleep(0.3)
            working_overlay_px = overlay_region('21-fullscreen-mark-working')
            fs_done = self.wait(
                'turn 4 finishes behind the put-away drawer over fullscreen',
                lambda: (v := self.agent_view())
                and v['mark_overlay']
                and v['state']['mark'] == 'done'
                and len(v['state']['turns']) >= 4
                and v['state']['turns'][3]['state']['kind'] == 'done'
                and v,
                timeout=45,
            )
            done_overlay_repainted = self.wait(
                'fullscreen mark overlay repaints from Working to Done',
                lambda: overlay_region('22-fullscreen-mark-done') != working_overlay_px,
                timeout=5,
            )
            self.check(
                'the mark shows working and done over a fullscreen window while the drawer is put away',
                bool(fs_working) and fs_done['mark_overlay'] and fs_done['state']['mark'] == 'done'
                and bool(done_overlay_repainted)
                and 'BG_DONE_OK' in fs_done['state']['turns'][3].get('answer', ''),
                json.dumps(fs_done['state']['turns'][3]),
            )
            self.key('meta_l', 'f')
            self.wait('mark overlay hides after leaving fullscreen',
                      lambda: not self.agent_view()['mark_overlay'])
            time.sleep(0.3)
            self.capture('22b-bar-mark-done')
            self.key('meta_l', 'spc')
            reopened = self.wait('drawer reopens and acknowledges done mark',
                                 lambda: (v := self.agent_view()) and v['drawer_up'] and v['state']['mark'] == 'idle' and v)
            self.check('summoning the drawer clears the done mark to idle', reopened['state']['mark'] == 'idle')

            # 3. Stop ends the turn and every command it started.
            self.type('please long-stop')
            self.key('ret')
            self.wait(
                'long command is running for Stop check',
                lambda: self.guest.ssh("pgrep -af '[s]leep 45'", check=False).stdout.strip()
                and self.agent_view()['state']['mark'] == 'working',
                timeout=30,
            )
            time.sleep(0.3)
            self.capture('23-drawer-working-stop')
            # /stop stops the turn: the drawer takes a command while the
            # thread works, though it takes no message.
            working = self.agent_view()
            self.type('/stop')
            self.key('ret')
            stopped_view = self.wait(
                'turn 5 ends as stopped after /stop',
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 5
                and v['state']['turns'][4]['state']['kind'] == 'stopped'
                and v['state']['mark'] == 'idle'
                and v,
                timeout=20,
            )
            time.sleep(0.5)
            self.capture('24-drawer-stopped')
            leftover = self.guest.ssh("pgrep -af '[s]leep 45'", check=False).stdout.strip()
            self.check(
                '/stop ends the turn as stopped and kills every command it started, and the Stop control was '
                'there while it worked',
                stopped_view['state']['turns'][4]['state']['kind'] == 'stopped' and not leftover
                and bool(control(working, 'stop')) and stopped_view.get('draft') == '',
                f'leftover={leftover!r}',
            )

            # 4. Failed turn shows why in the drawer.
            self.type('please make-fail')
            self.key('ret')
            failed_view = self.wait(
                'turn 6 ends as failed with reason',
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 6
                and v['state']['turns'][5]['state']['kind'] == 'failed'
                and v,
                timeout=30,
            )
            time.sleep(0.4)
            self.capture('25-drawer-failed')
            fail_reason = failed_view['state']['turns'][5]['state'].get('message', '')
            self.check('a failed turn shows why it failed in the drawer',
                       'Stub model declined this turn' in fail_reason,
                       fail_reason)

            # 5. After the shell restarts, the drawer reconnects and redraws the thread from Lever.
            self.key('meta_l', 'spc')
            self.wait('drawer put away before shell restart', lambda: not self.agent_view()['drawer_up'])
            old_shell_pid = self.run('pgrep -x crowbaros-shell').strip()
            self.run('sv restart crowbar-shell')
            self.wait(
                'restarted shell reconnects and restores the thread from Lever',
                lambda: self.guest.ssh('pgrep -x crowbaros-shell', check=False).stdout.strip() not in ('', old_shell_pid)
                and (v := self.agent_view())
                and v['state'].get('connected')
                and v['state'].get('thread') == thread_id
                and len(v['state'].get('turns', [])) == 6
                and v,
                timeout=30,
            )
            self.key('meta_l', 'spc')
            restored = self.wait('drawer opens with restored thread after shell restart',
                                 lambda: (v := self.agent_view()) and v['drawer_up'] and v)
            time.sleep(0.5)
            self.capture('26-drawer-after-shell-restart')
            self.check(
                'after the shell restarts, the drawer reconnects and redraws the thread from Lever',
                restored['state']['thread'] == thread_id
                and len(restored['state']['turns']) == 6
                and 'DRAWER_CMD_OK' in restored['state']['turns'][0].get('answer', '')
                and (restored['state']['turns'][0].get('context') or {}).get('active', {}).get('app_id') == 'foot'
                and (restored['state']['turns'][0].get('requests') or [{}])[0].get('answer') == 'once'
                and (restored['state']['turns'][1].get('requests') or [{}])[0].get('answer') == 'no'
                and restored['state'].get('mode') == 'full-access'
                and restored['state']['turns'][4]['state']['kind'] == 'stopped'
                and restored['state']['turns'][5]['state']['kind'] == 'failed',
                json.dumps(restored['state']['thread']),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer closed after restart check', lambda: not self.agent_view()['drawer_up'])

            # 6. Ask first keeps the person's own refusals: a command their
            # settings deny is refused, and no request offers to allow it.
            denied = self.guest.ssh(
                f"cd {home} && timeout 60 su -s /bin/sh {person} -c "
                f"\"/usr/local/bin/crowbar-agent --mode ask-first 'please denied-cmd'\"",
                check=False,
                timeout=90,
            )
            self.log(f'denied-cmd rc={denied.returncode}\n{denied.stdout}{denied.stderr}')
            self.check(
                'in Ask first, a command the person\'s own settings refuse is refused, with no request to allow it',
                denied.returncode == 0
                and '[request]' not in denied.stderr
                and 'DENIED_42' not in denied.stdout
                and 'Refused:' in denied.stdout,
                f'rc={denied.returncode} out={denied.stdout.strip()[:200]!r} err={denied.stderr.strip()[:300]!r}',
            )

            # 7. The person's own OpenCode settings are as they made them,
            # and a session they start by hand has none of Lever's additions.
            settings_after = self.run(f'sha256sum {home}/.config/opencode/opencode.json').split()[0]
            by_hand = self.run(
                f'cd {home} && chpst -u {person}:{person} env -i HOME={home} PATH=/usr/local/bin:/usr/bin:/bin '
                f'OPENCODE_DISABLE_MODELS_FETCH=1 opencode debug config',
                timeout=60,
            )
            self.check(
                'the person\'s own OpenCode settings are unchanged, and a session started by hand gets none of '
                'Lever\'s additions',
                settings_after == settings_before
                and '"stub"' in by_hand
                and 'crowbar-ask-first' not in by_hand,
                f'before={settings_before[:12]} after={settings_after[:12]} ask-first-in-hand-config='
                f'{"crowbar-ask-first" in by_hand}',
            )
            # 8. Concurrent terminal/drawer work and thread lifecycle.
            self.wait('drawer hidden before terminal thread', lambda: not self.agent_view()['drawer_up'])
            self.as_app('sh -c ' + shlex.quote('cd /tmp && crowbar-agent "please long-stop terminal"'), f'{tmp}/terminal-thread.out')
            terminal_view = self.wait('terminal-started thread drives hidden Mark',
                lambda: (v := self.agent_view()) and v['state']['mark'] == 'working'
                and any(t['cwd'] == '/tmp' and t['working'] for t in v['state']['threads']) and v, timeout=30)
            terminal_id = next(t['thread'] for t in terminal_view['state']['threads'] if t['cwd'] == '/tmp' and t['working'])
            self.check('terminal-started work drives the Mark with the drawer hidden', not terminal_view['drawer_up'])
            self.key('meta_l', 'spc')
            self.wait('drawer summoned before list', lambda: self.agent_view()['drawer_up'])
            self.key('ctrl', 'l')
            self.wait('thread list opens', lambda: self.agent_view()['state']['list_view'])
            self.capture('27-thread-list-working')
            self.key('meta_l', 'spc')
            self.wait('list put away', lambda: not self.agent_view()['drawer_up'])
            self.key('meta_l', 'spc')
            self.wait('summon returns to thread list', lambda: self.agent_view()['drawer_up'])
            self.check('summon preserves the thread-list view', self.agent_view()['state']['list_view'])
            self.key('ctrl', 'n')
            self.wait('new composer while another thread works', lambda: not self.agent_view()['state']['list_view'] and self.agent_view()['state']['thread'] is None)
            self.type('please run-cmd second')
            self.key('ret')
            second = self.wait('second drawer thread finishes alongside terminal work',
                lambda: (v := self.agent_view()) and len(v['state']['turns']) == 1
                and v['state']['turns'][0]['state']['kind'] == 'done' and v, timeout=30)
            second_id = second['state']['thread']
            # The new thread starts with the Model last picked. Its turn has
            # ended, so its acts are folded into one line, which opens.
            folded = self.wait('the ended turn folds its acts',
                               lambda: (v := self.agent_view()) and control(v, 'acts:1') and v)
            act_id = second['state']['turns'][0]['acts'][0]['id']
            click_control('acts:1')
            opened = self.wait('the fold opens', lambda: (v := self.agent_view()) and control(v, f'act:{act_id}') and v)
            turn = second['state']['turns'][0]
            self.check(
                'a new thread starts with the Model last picked, and an ended turn folds its acts into one line '
                'that opens',
                second['state'].get('model') == 'spare/spare-1'
                and not control(folded, f'act:{act_id}') and bool(control(opened, f'act:{act_id}'))
                and isinstance(turn.get('started'), int) and turn.get('ended', 0) >= turn['started'],
                json.dumps({'model': second['state'].get('model'), 'started': turn.get('started'),
                            'ended': turn.get('ended')}),
            )
            self.type('please show-markdown')
            self.key('ret')
            marked = self.wait('the answer in Markdown arrives',
                               lambda: (v := self.agent_view()) and len(v['state']['turns']) == 2
                               and v['state']['turns'][1]['state']['kind'] == 'done' and v, timeout=30)
            time.sleep(0.5)
            self.capture('28a-thread-markdown')
            self.check('an answer written in Markdown is kept as it was written, for the drawer to lay out',
                       'MARKDOWN_OK' in marked['state']['turns'][1].get('answer', '')
                       and '```sh' in marked['state']['turns'][1]['answer'],
                       marked['state']['turns'][1].get('answer', '')[:80])
            # Sending a harness slash command from the drawer (with desktop
            # context attached) runs the harness's command instead of sending
            # literal `/init` as plain text.
            stub_count_before_cmd = len([
                line for line in self.run(f'cat {tmp}/stub.jsonl').splitlines() if line.strip()
            ])
            self.type('/in')
            self.wait('typing /in finds OpenCode /init',
                      lambda: (v := self.agent_view()) and listed(v).get('highlighted') == 'command:init' and v,
                      timeout=30)
            self.key('ret')
            self.wait('taking /init puts it in the message box',
                      lambda: self.agent_view().get('draft') == '/init ')
            self.key('ret')
            ran_cmd = self.wait('the harness slash command turn finishes in the thread',
                                lambda: (v := self.agent_view()) and len(v['state']['turns']) == 3
                                and v['state']['turns'][2]['state']['kind'] == 'done' and v, timeout=45)
            stub_after_cmd = [
                json.loads(line)
                for line in self.run(f'cat {tmp}/stub.jsonl').splitlines()[stub_count_before_cmd:]
                if line.strip()
            ]
            cmd_prompts = [e.get('prompt', '') for e in stub_after_cmd if e.get('has_tools')]
            self.check(
                'sending a harness slash command from the drawer executes it as a command and shows its turn',
                ran_cmd['state']['turns'][2].get('message') == '/init'
                and bool(cmd_prompts)
                and any('AGENTS.md' in p and p.strip() != '/init' for p in cmd_prompts),
                json.dumps({'turn': ran_cmd['state']['turns'][2], 'prompts': [p[:120] for p in cmd_prompts]}),
            )
            # 7c. A message sent while the turn works is steered into it: the
            # pinned OpenCode takes it at its next step, and the Model's next
            # call has it, in the same turn.
            self.type('/model')
            self.wait('the Model picker opens', lambda: listed(self.agent_view()).get('kind') is not None)
            self.key('esc')
            empty_the_box()
            self.wait('Lever says the harness can steer a turn',
                      lambda: self.agent_view()['state'].get('steering') is True)
            self.type('please steer-hold')
            self.key('ret')
            self.wait('the Model is held on the command the turn calls',
                      lambda: any('steer-hold' in e.get('prompt', '') and e.get('decision') == 'tool_call'
                                  for e in stub_requests()), timeout=45)
            working = self.wait('turn 4 works', lambda: (v := self.agent_view()) and len(v['state']['turns']) == 4
                                and v['state']['turns'][3]['state']['kind'] == 'working' and v)
            self.check('the drawer takes a message while the turn works', working['state'].get('steering') is True)
            self.type('please steer-also')
            self.key('ret')
            taken = self.wait('the message is steered into turn 4',
                              lambda: (v := self.agent_view()) and len(v['state']['turns']) == 4
                              and v['state']['turns'][3].get('steered') and v, timeout=30)
            time.sleep(0.4)
            self.capture('27c-drawer-message-steered')
            self.run(f'touch {tmp}/steer-release')
            steered = self.wait('turn 4 ends with the steered message answered',
                                lambda: (v := self.agent_view()) and v['state']['turns'][3]['state']['kind'] == 'done'
                                and v, timeout=45)
            turn4 = steered['state']['turns'][3]
            heard = [e for e in stub_requests() if e.get('has_tools') and 'please steer-also' in e.get('prompt', '')]
            self.check(
                'a message sent while the turn works is taken into that turn, shown where it was taken, and the '
                'Model hears it in the same turn, after the command it called',
                len(steered['state']['turns']) == 4
                and [m['text'] for m in turn4.get('steered', [])] == ['please steer-also']
                and taken.get('draft') == ''
                and 'STEER_TAKEN_OK' in turn4.get('answer', '')
                and 'Should not answer' not in turn4.get('answer', '')
                and bool(heard) and any('STEER_HELD_RAN' in out for out in heard[0].get('history_tool_outputs', [])),
                json.dumps({'turn': turn4, 'heard': heard[:1]}),
            )
            self.check('new drawer thread starts at home while terminal thread keeps its cwd',
                second_id != terminal_id and second['state']['mark'] == 'working'
                and any(t['thread'] == second_id and t['cwd'] == home for t in second['state']['threads'])
                and any(t['thread'] == terminal_id and t['working'] for t in second['state']['threads']))
            self.key('ctrl', 'l')
            self.wait('list shows both threads', lambda: self.agent_view()['state']['list_view'])
            self.capture('28-thread-list-concurrent')
            self.check('thread list shows both concurrent states',
                {t['thread']: t['state'] for t in self.agent_view()['state']['threads']}.get(second_id) == 'done'
                and {t['thread']: t['state'] for t in self.agent_view()['state']['threads']}.get(terminal_id) == 'working')
            # Open the newest row using keyboard focus: New thread, then its row.
            self.key('tab')
            self.key('tab')
            self.key('ret')
            self.wait('keyboard opens newest thread record', lambda: not self.agent_view()['state']['list_view'] and self.agent_view()['state']['thread'] == second_id)
            self.capture('29-thread-opened-from-list')
            self.type('/archive')
            self.key('ret')
            self.wait('archive returns to list and hides the thread', lambda: self.agent_view()['state']['list_view']
                and any(t['thread'] == second_id and t['archived'] for t in self.agent_view()['state']['threads']))
            self.check('drawer archive hides the thread while keeping its record',
                'DRAWER_CMD_OK' in self.run(f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --resume {second_id} </dev/null'))
            self.key('meta_l', 'spc')
            self.wait('list hidden while terminal thread stops', lambda: not self.agent_view()['drawer_up'])
            self.run(f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --stop {terminal_id}')
            self.wait('terminal Stop drives hidden Mark to Done', lambda: self.agent_view()['state']['mark'] == 'done', timeout=20)
            self.check('terminal Stop updates the hidden Mark and list state',
                any(t['thread'] == terminal_id and t['state'] == 'stopped' for t in self.agent_view()['state']['threads']))
            self.run(f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --delete {terminal_id}')
            self.wait('terminal delete removes thread from live list', lambda: not any(t['thread'] == terminal_id for t in self.agent_view()['state']['threads']))
            self.check('terminal delete removes the record and updates the drawer list',
                terminal_id not in self.run(f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --list'))
            self.key('meta_l', 'spc')
            self.wait('list reopens after terminal deletion', lambda: self.agent_view()['drawer_up'] and self.agent_view()['state']['list_view'])
            click_control(f'thread:{thread_id}')
            self.wait('remaining record opens', lambda: not self.agent_view()['state']['list_view'] and self.agent_view()['state']['thread'] == thread_id)
            self.type('/delete')
            self.key('ret')
            self.wait('/delete asks before it deletes', lambda: control(self.agent_view(), 'confirm-delete'))
            time.sleep(0.3)
            self.capture('30-thread-delete-confirmation')
            self.key('esc')
            self.check('canceling drawer deletion retains the thread', self.agent_view()['state']['thread'] == thread_id)
            self.type('/delete')
            self.key('ret')
            self.wait('/delete asks again', lambda: control(self.agent_view(), 'confirm-delete'))
            self.key('tab')
            self.key('tab')
            self.key('ret')
            self.wait('confirmed drawer deletion removes the record', lambda: self.agent_view()['state']['list_view']
                and not any(t['thread'] == thread_id for t in self.agent_view()['state']['threads']), timeout=20)
            self.capture('31-thread-list-after-delete')
            self.check('drawer deletion asks first and removes the confirmed record',
                thread_id not in self.run(f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --list'))

            # 8b. Subagents (#380): a real OpenCode subagent outlives its parent
            # turn, raises a request to read a .env file, keeps the thread
            # working and the mark waiting, is not listed as a thread, and opens
            # in the drawer with its prompt, act, and answer.
            active_before = len([t for t in self.agent_view()['state']['threads'] if not t.get('archived')])
            list_before = len([
                line for line in self.run(
                    f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --list'
                ).splitlines()
                if line.strip()
            ])
            self.run(
                f"printf 'SUBAGENT_ENV_CHECK=crowbar_subagent_ok\\n' > {home}/.env "
                f"&& chown {person}:{person} {home}/.env && chmod 600 {home}/.env"
            )
            self.key('ctrl', 'n')
            self.wait('new thread composer for subagent check',
                      lambda: not self.agent_view()['state']['list_view'] and self.agent_view()['state']['thread'] is None)
            self.type('please subagent-check')
            self.key('ret')
            self.wait(
                'subagent outlives its turn',
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 1
                and v['state']['turns'][0]['state']['kind'] == 'done'
                and bool(v['state']['turns'][0].get('subagents'))
                and v['state']['turns'][0]['subagents'][0]['state']['kind'] == 'working',
                timeout=60,
            )
            # A mode change while the subagent works goes to OpenCode's live
            # session first: OpenCode answers it at once, and nothing stops.
            mode_thread = self.agent_view()['state']['thread']
            mode_took = []
            for mode in ('full-access', 'ask-first'):
                began = time.monotonic()
                self.run(
                    f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} '
                    f'crowbar-agent --resume {mode_thread} --mode {mode}'
                )
                mode_took.append(round(time.monotonic() - began, 2))
                self.wait(f'the drawer shows {mode} while the subagent works',
                          lambda: self.agent_view()['state']['mode'] == mode, timeout=10)
            mode_after = self.agent_view()
            self.check(
                'a mode change while a subagent works is taken by the live session at once and stops nothing',
                all(took < 3 for took in mode_took)
                and mode_after['state']['mode'] == 'ask-first'
                and mode_after['state']['turns'][0]['subagents'][0]['state']['kind'] == 'working',
                json.dumps({'took': mode_took, 'mode': mode_after['state']['mode']}),
            )
            self.run(f'touch {tmp}/subagent-parent-done')
            sub_waiting = self.wait(
                'subagent outlives its turn and raises a request in the thread',
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 1
                and v['state']['turns'][0]['state']['kind'] == 'done'
                and bool(v['state']['turns'][0].get('subagents'))
                and v['state']['turns'][0]['subagents'][0]['state']['kind'] == 'working'
                and (r := waiting_request(v)) is not None
                and r.get('subagent') == v['state']['turns'][0]['subagents'][0]['subagent']
                and control(v, f"answer:{r['request']}:once")
                and control(v, f"subagent:{v['state']['turns'][0]['subagents'][0]['subagent']}")
                and v,
                timeout=60,
            )
            time.sleep(0.5)
            self.capture('31a-subagent-request')
            sub_thread_id = sub_waiting['state']['thread']
            sub_before = sub_waiting['state']['turns'][0]['subagents'][0]
            sub_id = sub_before['subagent']
            sub_req = waiting_request(sub_waiting)
            self.key('meta_l', 'spc')
            self.wait('drawer put away while subagent waits', lambda: not self.agent_view()['drawer_up'])
            sub_away = self.agent_view()
            self.key('meta_l', 'spc')
            self.wait('drawer returns to waiting subagent',
                      lambda: (v := self.agent_view()) and v['drawer_up']
                      and control(v, f"answer:{sub_req['request']}:once"))
            sub_list_out = self.run(
                f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} crowbar-agent --list'
            )
            active_threads = [t for t in sub_waiting['state']['threads'] if not t.get('archived')]
            sub_thread_entry = next((t for t in active_threads if t['thread'] == sub_thread_id), None)
            sub_turn_blob = json.dumps({
                'turn': sub_waiting['state']['turns'][0],
                'threads': active_threads,
            })
            self.check(
                'a subagent outliving its turn keeps the thread working and the mark waiting, is not listed as a '
                'thread, and exposes no harness names',
                sub_waiting['state']['turns'][0]['state']['kind'] == 'done'
                and sub_before['state']['kind'] == 'working'
                and sub_req.get('subagent') == sub_id
                and sub_away['state']['mark'] == 'waiting'
                and len(active_threads) == active_before + 1
                and sub_thread_entry is not None
                and sub_thread_entry['working'] is True
                and sub_thread_entry.get('subagents') == 1
                and sub_thread_id in sub_list_out
                and '1 subagent' in sub_list_out
                and len([line for line in sub_list_out.splitlines() if line.strip()]) == list_before + 1
                and 'ses_' not in sub_turn_blob
                and 'childSessionId' not in sub_turn_blob
                and 'opencode' not in sub_turn_blob.lower(),
                json.dumps({'active_threads': active_threads, 'list': sub_list_out.strip(), 'subagent': sub_before}),
            )
            click_control(f"answer:{sub_req['request']}:once")
            sub_done = self.wait(
                'subagent finishes after its request is answered in the drawer',
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 1
                and bool(v['state']['turns'][0].get('subagents'))
                and v['state']['turns'][0]['subagents'][0]['state']['kind'] == 'done'
                and not any(t['thread'] == sub_thread_id and t['working'] for t in v['state']['threads'])
                and v,
                timeout=60,
            )
            sub_after = sub_done['state']['turns'][0]['subagents'][0]
            sub_act_id = sub_after['acts'][0]['id'] if sub_after.get('acts') else None
            click_control(f'subagent:{sub_id}')
            sub_opened = self.wait(
                'opening the subagent line shows its act in the drawer',
                lambda: (v := self.agent_view())
                and (sub_act_id is None or control(v, f'act:{sub_act_id}'))
                and v,
                timeout=20,
            )
            time.sleep(0.5)
            self.capture('31b-subagent-opened')
            sub_resume_out = self.run(
                f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} '
                f'crowbar-agent --resume {sub_thread_id} </dev/null 2>&1'
            )
            sub_answers = '\n'.join(m['text'] for m in sub_after.get('messages', []) if m.get('role') != 'prompt')
            sub_prompts = '\n'.join(m['text'] for m in sub_after.get('messages', []) if m.get('role') == 'prompt')
            sub_done_req = next(
                (r for r in sub_done['state']['turns'][0].get('requests', []) if r['request'] == sub_req['request']),
                {},
            )
            self.check(
                'answering a subagent request lets it finish, opening it in the drawer shows its work, and '
                'crowbar-agent shows the subagent',
                sub_after['title'] == 'Inspect env file'
                and sub_after['state']['kind'] == 'done'
                and sub_done_req.get('state') == 'answered'
                and sub_done_req.get('answer') == 'once'
                and 'subagent-child-read' in sub_prompts
                and 'crowbar_subagent_ok' in sub_answers
                and bool(sub_act_id)
                and bool(control(sub_opened, f'act:{sub_act_id}'))
                and 'Inspect env file' in sub_resume_out
                and 'crowbar_subagent_ok' in sub_resume_out,
                json.dumps({'subagent': sub_after, 'req': sub_done_req, 'resume': sub_resume_out[:300]}),
            )
            # OpenCode wakes the parent session with the subagent's report, and
            # the Model's answer there is a turn the harness began, apart from
            # the person's, which Lever ends once the harness is quiet.
            woke = self.wait(
                "the harness's own turn after the subagent reported ends on its own",
                lambda: (v := self.agent_view())
                and len(v['state']['turns']) >= 2
                and v['state']['turns'][1].get('from_harness')
                and v['state']['turns'][1]['state']['kind'] == 'done'
                and not any(t['thread'] == sub_thread_id and t['working'] for t in v['state']['threads'])
                and v,
                timeout=60,
            )
            time.sleep(0.5)
            self.capture('31c-harness-turn')
            woke_turns = woke['state']['turns']
            self.check(
                "the harness's answer after a subagent reported is a turn of its own, apart from the person's",
                woke_turns[0]['answer'] == 'Started a subagent to inspect the env file.'
                and 'The subagent finished its work.' in woke_turns[1]['answer']
                and woke_turns[1]['message'] == '',
                json.dumps([{k: t.get(k) for k in ('turn', 'message', 'answer', 'from_harness')} for t in woke_turns]),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer closed after lifecycle checks', lambda: not self.agent_view()['drawer_up'])

            # 8c. Terminal session started by hand (#382): `opencode run` in a
            # folder against the stub Model becomes a thread in that folder
            # while its first turn still works, with Lever's own harness
            # stopped so discovery runs OpenCode once to list and then stops
            # it. It does not duplicate Lever threads or list subagents, and
            # once the store is quiet no listing starts again. It loads its
            # history on open and accepts a follow-up from the drawer.
            hand_dir = f'{tmp}/hand-session'
            self.run(f'install -d -o {person} -g {person} {hand_dir}')
            # Every OpenCode now is Lever's harness or under it; no session
            # by hand runs yet.
            self.run("ps -eo pid,ppid,args | grep '[o]pencode'; pkill -KILL -x opencode; true")
            self.wait("Lever's own OpenCode stopped before the hand-started session",
                      lambda: not self.guest.ssh('pgrep -x opencode', check=False).stdout.strip())
            threads_before_hand = list(self.agent_view()['state']['threads'])
            self.run(
                # Only the command goes to the background, so nothing holds ssh's output.
                f'cd {hand_dir} || exit 1; nohup chpst -u {person}:{person} env -i HOME={home} '
                f'XDG_CONFIG_HOME={home}/.config XDG_DATA_HOME={home}/.local/share '
                f'XDG_CACHE_HOME={home}/.cache XDG_STATE_HOME={home}/.local/state '
                f'PATH=/usr/local/bin:/usr/bin:/bin '
                f'OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_DISABLE_FREE_MODELS=1 '
                f'opencode run --model stub/stub-1 "please hand-session-check" '
                f'</dev/null >{tmp}/hand-run.log 2>&1 &'
            )
            hand_started = time.monotonic()
            hand_listed = self.wait(
                'session started by hand with opencode run appears in Lever thread list while its turn works',
                lambda: (v := self.agent_view())
                and any(t.get('cwd') == hand_dir for t in v['state']['threads'])
                and v,
                timeout=20,
            )
            hand_listed_after = time.monotonic() - hand_started
            turn_still_held = self.guest.ssh(f'test -e {tmp}/hand-release', check=False).returncode != 0
            self.run(f'touch {tmp}/hand-release')
            self.wait('opencode run started by hand finishes',
                      lambda: not self.guest.ssh("pgrep -f '[o]pencode run --model stub/stub-1'",
                                                 check=False).stdout.strip(),
                      timeout=90)
            self.log('opencode run output:\n' + self.guest.ssh(f'cat {tmp}/hand-run.log', check=False).stdout)
            # Once the store goes quiet, Lever starts no OpenCode to list it.
            time.sleep(5)
            listing_pids = json.loads(self.python("""
import json, time
from pathlib import Path
def stats():
    for path in Path('/proc').glob('[0-9]*/stat'):
        try:
            stat = path.read_text()
        except OSError:
            continue
        yield path.parent.name, stat[stat.index('(') + 1:stat.rindex(')')], stat[stat.rindex(')') + 2:].split()[1]
seen = set()
end = time.monotonic() + 8
while time.monotonic() < end:
    found = list(stats())
    levers = {pid for pid, name, _ in found if name == 'crowbar-lever'}
    seen.update(pid for pid, name, ppid in found if name == 'opencode' and ppid in levers)
    time.sleep(0.05)
print(json.dumps(sorted(seen)))
"""))
            threads_after_hand = self.agent_view()['state']['threads']
            hand_entry = next(t for t in threads_after_hand if t.get('cwd') == hand_dir)
            hand_thread_id = hand_entry['thread']
            self.check(
                'a session started by hand with opencode run becomes a thread in its folder while its turn '
                'works, without duplicating Lever threads, listing subagents, or starting OpenCode again on a '
                'quiet store',
                hand_thread_id.startswith('t-')
                and turn_still_held
                and 'ses_' not in json.dumps(threads_after_hand)
                and len(threads_after_hand) == len(threads_before_hand) + 1
                and not listing_pids,
                json.dumps({
                    'before': len(threads_before_hand),
                    'after': len(threads_after_hand),
                    'entry': hand_entry,
                    'listed_after_s': round(hand_listed_after, 1),
                    'turn_still_held': turn_still_held,
                    'listings_on_quiet_store': listing_pids,
                }),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer opens for hand-started session check', lambda: self.agent_view()['drawer_up'])
            self.key('ctrl', 'l')
            self.wait('thread list opens showing hand-started session', lambda: self.agent_view()['state']['list_view'])
            time.sleep(0.3)
            self.capture('31d-hand-session-listed')
            click_control(f'thread:{hand_thread_id}')
            hand_opened = self.wait(
                'opening the hand-started thread imports its history into Lever and the drawer',
                lambda: (v := self.agent_view())
                and not v['state']['list_view']
                and v['state']['thread'] == hand_thread_id
                and len(v['state'].get('turns', [])) >= 1
                and 'HAND_SESSION_CMD_OK' in v['state']['turns'][0].get('answer', '')
                and v,
                timeout=30,
            )
            time.sleep(0.4)
            self.capture('31e-hand-session-opened')
            self.type('please hand-followup')
            self.key('ret')
            hand_continued = self.wait(
                'follow-up sent from the drawer on the hand-started thread completes',
                lambda: (v := self.agent_view())
                and v['state']['thread'] == hand_thread_id
                and len(v['state'].get('turns', [])) >= 2
                and v['state']['turns'][1]['state']['kind'] == 'done'
                and v,
                timeout=45,
            )
            hand_turns = hand_continued['state']['turns']
            hand_resume_out = self.run(
                f'chpst -u {person} env HOME={home} XDG_RUNTIME_DIR=/run/user/{void_base.PERSON_UID} '
                f'crowbar-agent --resume {hand_thread_id} </dev/null 2>&1'
            )
            followup_reqs = [
                r for r in stub_requests()
                if 'hand-followup' in (r.get('prompt') or '')
            ]
            followup_saw_terminal_act = any(
                'HAND_SESSION_CMD_OK' in out
                for r in followup_reqs
                for out in (r.get('history_tool_outputs') or [])
            )
            self.check(
                'opening a hand-started thread imports its turn and acts into Lever and sending from the '
                'drawer continues the same session',
                'hand-session-check' in hand_turns[0].get('message', '')
                and 'HAND_SESSION_CMD_OK' in hand_turns[0].get('answer', '')
                and len(hand_turns[0].get('acts') or []) >= 1
                and 'Continued from drawer' in hand_turns[1].get('answer', '')
                and followup_saw_terminal_act
                and 'ses_' not in json.dumps(hand_turns)
                and 'HAND_SESSION_CMD_OK' in hand_resume_out
                and 'Continued from drawer' in hand_resume_out,
                json.dumps({
                    'turns': [{k: t.get(k) for k in ('turn', 'message', 'answer', 'state')} for t in hand_turns],
                    'acts0': len(hand_turns[0].get('acts') or []),
                    'followup_saw_terminal_act': followup_saw_terminal_act,
                }),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer closed after hand-started session check', lambda: not self.agent_view()['drawer_up'])

            self.agent_restart_recovery(tmp, home)

            # 9. With no Model reachable, a message is refused and the drawer says why.
            self.guest.ssh("pkill -f '[s]tub-model.py'; true", check=False)
            self.wait('stub Model stopped for unreachable check',
                      lambda: not self.guest.ssh("pgrep -f '[s]tub-model.py'", check=False).stdout.strip())
            self.key('meta_l', 'spc')
            self.wait('drawer opens for unreachable Model check', lambda: self.agent_view()['drawer_up'])
            self.key('ctrl', 'n')
            self.wait('new thread opens for unreachable Model check', lambda: not self.agent_view()['state']['list_view'])
            self.type('hello unreachable')
            self.key('ret')
            unreachable = self.wait(
                'unreachable Model reports failure reason in the drawer',
                lambda: (v := self.agent_view()) and v['state'].get('turns')
                and v['state']['turns'][0]['state']['kind'] == 'failed' and v,
                timeout=45,
            )
            time.sleep(0.3)
            self.capture('37-drawer-model-unreachable')
            unreachable_msg = unreachable['state']['turns'][0]['state'].get('message', '')
            self.check(
                'with no Model reachable, the drawer reports why the message could not be answered',
                'Could not reach the Model right now' in unreachable_msg,
                unreachable_msg,
            )
            # And with no provider configured at all (and free Models off), sending
            # is refused before a turn starts and the drawer says why.
            self.run(f'rm -f {home}/.config/opencode/opencode.json')
            refresh_lever_models()
            self.wait(
                'drawer receives empty Model catalog',
                lambda: (v := self.agent_view()) and v['state'].get('offer_known')
                and v['state'].get('models') == [],
                timeout=45,
            )
            turns_before = len(self.agent_view()['state']['turns'])
            self.type('hello with no model')
            self.key('ret')
            refused_view = self.wait(
                'message with no Model available is refused in the drawer',
                lambda: (v := self.agent_view()) and v['state'].get('missing_model') and v,
                timeout=15,
            )
            time.sleep(0.3)
            self.capture('37b-drawer-no-model-refused')
            self.check(
                'with no Model available, a message is refused and the drawer says why',
                len(refused_view['state']['turns']) == turns_before
                and 'No Model is available' in (refused_view['state'].get('missing_model') or ''),
                json.dumps({'turns': len(refused_view['state']['turns']), 'missing': refused_view['state'].get('missing_model')}),
            )
            self.key('meta_l', 'spc')
            self.wait('drawer closed after unreachable Model checks', lambda: not self.agent_view()['drawer_up'])

        finally:
            # Retain the synthetic conversation and service diagnostics before
            # restoring the guest's empty settings and state below.
            for name, path in [('agent-lever.log', f'{home}/.local/state/crowbaros/lever/lever.log'),
                               ('agent-model.jsonl', f'{tmp}/stub.jsonl')]:
                result = self.guest.ssh(f'cat {path}', check=False)
                (self.work / name).write_text(result.stdout)
            self.guest.ssh("pkill -f '[s]tub-model.py'; pkill -f '[c]rowbar-probe copy'; pkill -f '[s]leep 45'; true", check=False)
            self.guest.ssh(
                f"sv stop /var/service/crowbar-lever >/dev/null 2>&1 || true; "
                f"pkill -x opencode 2>/dev/null || true; "
                f"rm -rf {home}/.config/opencode {home}/.local/share/opencode {home}/.local/state/crowbaros/lever {home}/.env {tmp}; "
                f"sv start /var/service/crowbar-lever >/dev/null 2>&1 || true",
                check=False,
            )

    def agent_restart_recovery(self, tmp, home):
        """Exercise process deaths with Base's pinned OpenCode and the stub Model."""
        def lever(request):
            code = '''
import json, socket, struct
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect('/run/crowbar-lever/surface')
f = s.makefile('rb')
def read():
    size = struct.unpack('!I', f.read(4))[0]
    return json.loads(f.read(size))
def write(value):
    body = json.dumps(value).encode()
    s.sendall(struct.pack('!I', len(body)) + body)
write({'version': 1})
assert read() == {'version': 1}
write(REQUEST)
while True:
    reply = read()
    if 'reply' in reply:
        print(json.dumps(reply))
        break
'''.replace('REQUEST', repr(request))
            return json.loads(self.python(code, user='crowbar-shell'))

        def pids(root=None):
            # Compare live processes, excluding zombies. With a root, capture
            # only that harness's tree: OpenCode started by hand is separate.
            code = """
import json
from pathlib import Path
found = {'crowbar-lever': [], 'opencode': []}
parents = {}
for path in Path('/proc').glob('[0-9]*/stat'):
    try:
        stat = path.read_text()
        name = stat[stat.index('(') + 1:stat.rindex(')')]
        fields = stat[stat.rindex(')') + 2:].split()
        state = fields[0]
        parents[int(path.parent.name)] = int(fields[1])
        if name in found and state != 'Z':
            found[name].append(int(path.parent.name))
    except (OSError, ValueError):
        pass
root = ROOT_PID
if root is not None:
    family = {root}
    while True:
        children = {pid for pid, parent in parents.items() if parent in family} - family
        if not children:
            break
        family.update(children)
    found = {name: [pid for pid in ids if pid in family] for name, ids in found.items()}
print(json.dumps({name: sorted(ids) for name, ids in found.items()}))
""".replace('ROOT_PID', repr(root))
            return json.loads(self.python(code))

        def open_drawer():
            if not self.agent_view()['drawer_up']:
                self.key('meta_l', 'spc')
                self.wait('recovery drawer opens', lambda: self.agent_view()['drawer_up'])

        def finished(thread, count):
            v = self.agent_view()
            return (v['state']['thread'] == thread and len(v['state']['turns']) == count
                    and v['state']['turns'][-1]['state']['kind'] == 'done' and v)

        def send_text(text):
            self.type(text)
            self.key('ret')

        open_drawer()
        self.key('ctrl', 'n')
        self.wait('new recovery composer', lambda: self.agent_view()['state']['thread'] is None)
        send_text('please run-cmd recovery seed')
        seeded = self.wait('recovery seed finishes', lambda: (v := self.agent_view())
                           and len(v['state']['turns']) == 1
                           and v['state']['turns'][0]['state']['kind'] == 'done' and v, timeout=45)
        thread = seeded['state']['thread']
        processes = pids()
        self.check('recovery seed records an act and its answer',
                   'DRAWER_CMD_OK' in seeded['state']['turns'][0]['answer'])
        send_text('please recovery-slow shell')
        self.wait('work starts before shell death', lambda: self.agent_view()['state']['mark'] == 'working'
                  and 'sleep 15' in self.guest.ssh("pgrep -af '[s]leep 15'", check=False).stdout)
        self.capture('32-recovery-working-before-shell-restart')
        old_shell = self.run('pgrep -x crowbaros-shell').strip()
        self.run('sv restart crowbar-shell')
        restored = self.wait('new shell restores the working turn', lambda: (v := self.agent_view())
                            and v['state']['connected'] and v['state']['thread'] == thread
                            and len(v['state']['turns']) == 2
                            and self.run('pgrep -x crowbaros-shell').strip() != old_shell and v)
        self.check('a shell restart leaves Lever and OpenCode running and restores the same thread',
                   pids() == processes and restored['state']['turns'][0] == seeded['state']['turns'][0])
        completed = self.wait('work finishes after shell restart', lambda: finished(thread, 2), timeout=45)
        self.check('the shell redraws the completed turn with no fresh-session warning',
                   'RECOVERY_DONE' in completed['state']['turns'][-1]['answer']
                   and not completed['state']['turns'][-1]['notes'])
        open_drawer()
        self.capture('33-recovery-shell-restored')

        send_text('please recovery-slow compositor')
        self.wait('work starts before Fulcrum stops', lambda: self.agent_view()['state']['mark'] == 'working'
                  and 'sleep 15' in self.guest.ssh("pgrep -af '[s]leep 15'", check=False).stdout)
        locks = self.shell_log().count('SHELL lock open=true')
        old_shell = self.run('pgrep -x crowbaros-shell').strip()
        self.run('sv down crowbar-compositor')
        try:
            away = lever({'op': 'send', 'text': 'message while Fulcrum is away', 'cwd': home})
            self.check('the surface accepts a message while Fulcrum is away', away.get('reply') == 'sent', json.dumps(away))
            record = self.wait('the away message finishes', lambda: (r := lever({'op': 'resume', 'thread': away['thread']}))
                               and r.get('turns') and r['turns'][-1]['state'] == 'done' and r)
            self.check('a message while Fulcrum is away has no desktop context',
                       record['turns'][0].get('context') is None, json.dumps(record))
            self.check('Fulcrum stopping leaves Lever and OpenCode running', pids() == processes)
        finally:
            self.run('sv up crowbar-compositor')
        self.ready()
        self.wait('restarted Fulcrum locks the desktop', lambda: self.snapshot()['locked']
                  and self.run('pgrep -x crowbaros-shell').strip() != old_shell
                  and self.shell_log().count('SHELL lock open=true') > locks)
        # Let the new lock surface take keyboard focus before typing into it.
        time.sleep(0.5)
        self.key('esc')
        self.unlock()
        self.wait('desktop unlocks for recovery', lambda: not self.snapshot()['locked'])
        # Restore the test's two-output coordinates after hotplug discovery.
        self.displays({'op': 'configure_output', 'name': 'Virtual-2', 'x': 1280, 'y': 0})
        self.wait('work finishes across Fulcrum restart', lambda: finished(thread, 3), timeout=45)
        open_drawer()
        self.capture('34-recovery-compositor-restored')
        self.check('the same thread finishes after Fulcrum restarts',
                   pids() == processes and 'RECOVERY_DONE' in self.agent_view()['state']['turns'][-1]['answer']
                   and not self.agent_view()['state']['turns'][-1]['notes'])

        # Kill each process while it waits for the person. The old request
        # must not become actionable again, and reading must start no work.
        for target in ['Lever', 'OpenCode']:
            reply = lever({'op': 'set-mode', 'thread': thread, 'mode': 'ask-first'})
            self.check('recovery thread can use Ask first', reply.get('reply') != 'error', json.dumps(reply))
            self.wait('recovery thread uses Ask first', lambda: self.agent_view()['state']['mode'] == 'ask-first')
            count = len(self.agent_view()['state']['turns'])
            send_text(f'please run-cmd before {target} death')
            waiting = self.wait('recovery request waits', lambda: (v := self.agent_view())
                                and len(v['state']['turns']) == count + 1
                                and any(r['state'] == 'waiting' for r in v['state']['turns'][-1]['requests']) and v,
                                timeout=45)
            request = next(r for r in waiting['state']['turns'][-1]['requests'] if r['state'] == 'waiting')
            before = lever({'op': 'list', 'archived': True})['threads']
            old_lever = self.run('pgrep -x crowbar-lever').strip()
            old_harness = int(self.run(f'pgrep -P {old_lever} -x opencode').strip())
            old_harnesses = pids(root=old_harness)['opencode']
            self.check('the running harness is in its captured process tree', old_harness in old_harnesses)
            self.capture(f'35-recovery-before-{target.lower()}-death')
            if target == 'Lever':
                # `once` leaves the current process up but prevents runit from
                # racing the assertions with an automatic restart after SIGKILL.
                self.run('sv once crowbar-lever')
                self.run(f'kill -KILL {old_lever}')
                self.wait('OpenCode dies with Lever', lambda: not set(pids()['opencode']) & set(old_harnesses))
                self.run('sv up crowbar-lever')
            else:
                self.run(f'kill -KILL {old_harness}')
            recovered = self.wait(f'{target} death is shown as interrupted', lambda: (v := self.agent_view())
                                  and v['state']['connected'] and v['state']['thread'] == thread
                                  and len(v['state']['turns']) == count + 1
                                  and v['state']['turns'][-1]['state']['kind'] == 'interrupted' and v, timeout=45)
            self.check(f'{target} death withdraws the request and never repeats the turn',
                       all(r['state'] == 'withdrawn' for r in recovered['state']['turns'][-1]['requests'])
                       and not lever({'op': 'status'})['harness_started'])
            self.check(f'{target} death leaves every recorded thread readable',
                       all((r := lever({'op': 'resume', 'thread': t['thread']})).get('reply') == 'resumed'
                           and len(r['turns']) == t['turns'] for t in before))
            self.check(f'an old request cannot run after {target} death',
                       lever({'op': 'answer', 'thread': thread, 'request': request['request'], 'answer': 'once'}).get('code') == 'no-request')
            open_drawer()
            self.capture(f'36-recovery-after-{target.lower()}-death')
            send_text(f'continue after {target} restart')
            continued = self.wait(f'next message answers after {target} restart', lambda: finished(thread, count + 2), timeout=45)
            entries = [json.loads(line) for line in self.run(f'cat {tmp}/stub.jsonl').splitlines() if line.strip()]
            prompts = [e for e in entries if e.get('has_tools') and e.get('prompt', '').endswith(f'continue after {target} restart')]
            self.check(f'OpenCode loads the original session after {target} death, including earlier act output',
                       bool(prompts) and any('DRAWER_CMD_OK' in out for out in prompts[0].get('history_tool_outputs', []))
                       and not continued['state']['turns'][-1].get('notes'), json.dumps(prompts[:1]))
            self.check(f'{target} restarts on demand with no duplicate turns',
                       not set(pids()['opencode']) & set(old_harnesses)
                       and (target == 'Lever' or self.run('pgrep -x crowbar-lever').strip() == old_lever))
        self.key('meta_l', 'spc')
        self.wait('recovery drawer closes', lambda: not self.agent_view()['drawer_up'])

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
inp_before = ask({'op': 'input_config'})
inp_set = ask({'op': 'configure_input', 'layout': 'gb', 'repeat_rate': 40, 'repeat_delay': 300, 'pointer_speed': 4, 'natural_scroll': True})
inp_mid = ask({'op': 'input_config'})
inp_restore = ask({'op': 'configure_input', 'layout': inp_before['layout'], 'repeat_rate': inp_before['repeat_rate'], 'repeat_delay': inp_before['repeat_delay'], 'pointer_speed': inp_before['pointer_speed'], 'natural_scroll': inp_before['natural_scroll']})
inp_after = ask({'op': 'input_config'})
pwr_before = ask({'op': 'power_config'})
pwr_set = ask({'op': 'configure_power', 'lock_after_secs': 600, 'suspend_after_secs': 1800})
pwr_mid = ask({'op': 'power_config'})
pwr_restore = ask({'op': 'configure_power', 'lock_after_secs': pwr_before['lock_after_secs'], 'suspend_after_secs': pwr_before['suspend_after_secs']})
pwr_after = ask({'op': 'power_config'})
bad_input = ask({'op': 'configure_input', 'evil': True})
print(json.dumps({
 'snap_refused': snap.get('reply') == 'error',
 'lock_refused': lock.get('reply') == 'error',
 'bad_input_refused': bad_input.get('reply') == 'error',
 'has_outputs': before.get('reply') == 'outputs' and len(before.get('outputs', [])) >= 1,
 'scaled_ok': scaled.get('reply') == 'ok' and mid['outputs'][0]['scale'] == 150,
 'restored_ok': restored.get('reply') == 'ok' and after['outputs'][0]['scale'] == orig_scale,
 'input_ok': (
  inp_before.get('reply') == 'input_config'
  and inp_set.get('reply') == 'ok'
  and inp_mid.get('layout') == 'gb'
  and inp_mid.get('repeat_rate') == 40
  and inp_mid.get('natural_scroll') is True
  and inp_restore.get('reply') == 'ok'
  and inp_after.get('layout') == inp_before['layout']
 ),
 'power_ok': (
  pwr_before.get('reply') == 'power_config'
  and pwr_set.get('reply') == 'ok'
  and pwr_mid.get('lock_after_secs') == 600
  and pwr_mid.get('suspend_after_secs') == 1800
  and pwr_restore.get('reply') == 'ok'
  and pwr_after.get('lock_after_secs') == pwr_before['lock_after_secs']
 ),
}))
"""
        report = json.loads(self.python(code, self.person()[0]))
        self.check("display bridge refuses non-display compositor ops from the person's apps",
                   report['snap_refused'] and report['lock_refused'] and report['bad_input_refused'])
        self.check('display bridge serves outputs and applies scale changes',
                   report['has_outputs'] and report['scaled_ok'] and report['restored_ok'])
        self.check('settings bridge applies and restores live input configuration through Fulcrum',
                   report['input_ok'])
        self.check('settings bridge applies and restores live power idle timers through Fulcrum',
                   report['power_ok'])
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
AGENT_TERMINAL_SETUP = r"printf '\033]11;#302040\007'"


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
    '\\': ('backslash',),
    ']': ('bracket_right',),
    '#': ('shift', '3'),
    '*': ('shift', '8'),
    '(': ('shift', '9'),
    ')': ('shift', '0'),
    '^': ('shift', '6'),
    '%': ('shift', '5'),
    '<': ('shift', 'comma'),
    '>': ('shift', 'dot'),
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
