#!/usr/bin/python3
"""The launcher service: it starts the apps the shell asks for, as the person.

runit starts it as root. It binds its socket in a directory root owns, so no
app can take the socket's place, and then becomes the person's account, with
its groups, before it reads a request. Every app it starts runs as the person,
with their home, at the compositor's socket for apps (ADR 0032), as an app
would on any desktop. It takes requests only from the shell's user ID. Its
other request reads the person's saved appearance for the shell.
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
PERSON_UID = 1003
# The compositor's socket for apps, as Base links it into the person's
# runtime directory.
WAYLAND_DISPLAY = 'wayland-0'
MAX_REQUEST = 4096


def peer_uid(connection):
    return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))[1]


def desktop_path(value):
    path = Path(value)
    if not path.is_absolute() or path.suffix != '.desktop':
        raise ValueError('Only a desktop entry, by its full path, can be launched')
    if not stat.S_ISREG(path.stat().st_mode):
        raise ValueError('A desktop entry must be a file')
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


def system_locale(path='/etc/locale.conf'):
    """The system's `LANG`, as Void's locale.conf sets it, or C.UTF-8."""
    try:
        for line in Path(path).read_text().splitlines():
            name, _, value = line.partition('=')
            if name.strip() == 'LANG' and value.strip():
                return value.strip().strip('"')
    except OSError:
        pass
    return 'C.UTF-8'


def app_environment(account):
    """What a login gives the person, and what the desktop adds."""
    return dict(HOME=account.pw_dir, USER=account.pw_name, LOGNAME=account.pw_name,
                SHELL=account.pw_shell or '/bin/sh',
                PATH=f'{account.pw_dir}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/usr/sbin:/sbin:/bin',
                LANG=system_locale(),
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
            ['/usr/bin/gio', 'launch', path],
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


def become(person):
    """Drops root for the person's account and its groups, for good."""
    os.setgroups(os.getgrouplist(person.pw_name, person.pw_gid))
    os.setresgid(person.pw_gid, person.pw_gid, person.pw_gid)
    os.setresuid(person.pw_uid, person.pw_uid, person.pw_uid)
    if os.getresuid() != (person.pw_uid,) * 3 or 0 in os.getgroups():
        raise SystemExit('The launcher could not become the person')


def serve():
    shell = pwd.getpwnam(SHELL)
    person = pwd.getpwuid(PERSON_UID)
    Path(SOCKET).unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as listener:
        # Root's socket in root's directory, which the shell's group may
        # enter: the person, and so every app, may neither replace nor
        # remove it. The peer's user ID is the check that counts.
        listener.bind(SOCKET)
        os.chown(SOCKET, 0, shell.pw_gid)
        os.chmod(SOCKET, 0o660)
        listener.listen(8)
        become(person)
        while True:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(2)
                try:
                    peer = peer_uid(connection)
                    line = b''
                    if peer == shell.pw_uid:
                        with connection.makefile('rb') as stream:
                            line = stream.readline(MAX_REQUEST + 1)
                    reply = answer(line, peer, shell.pw_uid, person)
                    connection.sendall(json.dumps(reply).encode() + b'\n')
                except OSError:
                    pass


if __name__ == '__main__':
    serve()
