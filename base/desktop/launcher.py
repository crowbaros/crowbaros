#!/usr/bin/python3
"""The launcher service: it starts the apps the shell asks for, as the person.

It runs as the person's account, so every app it starts runs as them, with
their home, at the compositor's socket for apps (ADR 0032). Nothing in it runs
as root. It takes requests only from the shell's user ID, and starts only an
installed, root-owned desktop entry: no caller environment, inherited
descriptor, command string, or user ID is accepted. Its other request reads the
person's saved appearance for the shell, through bounded, no-follow directory
handles.
"""
import json
import os
from pathlib import Path
import pwd
import socket
import stat
import struct
import subprocess

SOCKET = '/run/crowbar-launch/socket'
SHELL = 'crowbar-shell'
# The compositor's socket for apps, as Base links it into the person's
# runtime directory.
WAYLAND_DISPLAY = 'wayland-0'
MAX_REQUEST = 4096


def peer_uid(connection):
    return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))[1]


def desktop_path(value):
    path = Path(value)
    if path.parent != Path('/usr/share/applications') or path.suffix != '.desktop':
        raise ValueError('Only installed desktop entries can be launched')
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
        raise ValueError('Desktop entry must be a root-owned regular file')
    return str(path)


def appearance(home, uid):
    descriptors = []
    try:
        directory = os.open(home, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptors.append(directory)
        for name in ('.config', 'crowbaros'):
            directory = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            descriptors.append(directory)
        fd = os.open('appearance.json', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        descriptors.append(fd)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != uid or info.st_nlink != 1:
            raise ValueError('Preferences must be a regular file owned only by the person')
        data = os.read(fd, 65537)
        if len(data) > 65536:
            raise ValueError('Preferences exceed 64 KiB')
        return json.loads(data)
    except FileNotFoundError:
        return None
    finally:
        for fd in reversed(descriptors):
            os.close(fd)


def app_environment(account):
    return dict(HOME=account.pw_dir, USER=account.pw_name, LOGNAME=account.pw_name,
                SHELL=account.pw_shell or '/bin/sh',
                PATH='/usr/local/bin:/usr/bin:/bin', LANG='C.UTF-8',
                XDG_RUNTIME_DIR=f'/run/user/{account.pw_uid}',
                XDG_CURRENT_DESKTOP='CrowbarOS', XDG_SESSION_TYPE='wayland',
                WAYLAND_DISPLAY=WAYLAND_DISPLAY,
                BROWSER='chromium',
                XCURSOR_THEME='Vanilla-DMZ', XCURSOR_SIZE='24',
                CROWBAR_DISPLAY_SOCKET='/run/crowbar-displays/socket',
                CROWBAR_SESSION='alpha-no-animations', RUST_LOG='info')


def dispatch(request, person):
    if request == {'op': 'appearance'}:
        return {'ok': True, 'preferences': appearance(person.pw_dir, person.pw_uid)}
    if set(request) != {'op', 'desktop'} or request['op'] != 'launch':
        raise ValueError('Unknown request')
    path = desktop_path(request['desktop'])
    with open('/var/log/crowbar-apps.log', 'ab', buffering=0) as log:
        result = subprocess.run(
            ['/usr/bin/setpriv', '--no-new-privs', '/usr/bin/gio', 'launch', path],
            close_fds=True, start_new_session=True,
            env=app_environment(person), cwd=person.pw_dir, stdin=subprocess.DEVNULL,
            stdout=log, stderr=log, timeout=8,
        )
    if result.returncode:
        raise ValueError('The application failed to launch')
    return {'ok': True}


def answer(line, peer, shell_uid, person):
    """The reply to one request line from a peer with this user ID."""
    try:
        if peer != shell_uid:
            raise PermissionError('Only the shell may make a request')
        if len(line) > MAX_REQUEST or not line.endswith(b'\n'):
            raise ValueError('Request too large or incomplete')
        request = json.loads(line)
        if not isinstance(request, dict):
            raise ValueError('Request must be an object')
        return dispatch(request, person)
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        return {'ok': False, 'error': str(error)}


def serve():
    shell_uid = pwd.getpwnam(SHELL).pw_uid
    person = pwd.getpwuid(os.getuid())
    if person.pw_uid == 0:
        raise SystemExit('The launcher runs as the person, never as root')
    Path(SOCKET).unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as listener:
        # The socket takes the shell's group from its directory; the peer's
        # user ID is the check that counts.
        listener.bind(SOCKET)
        os.chmod(SOCKET, 0o660)
        listener.listen(8)
        while True:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(2)
                try:
                    peer = peer_uid(connection)
                    line = b'' if peer != shell_uid else connection.makefile('rb').readline(MAX_REQUEST + 1)
                    reply = answer(line, peer, shell_uid, person)
                    connection.sendall(json.dumps(reply).encode() + b'\n')
                except OSError:
                    pass


if __name__ == '__main__':
    serve()
