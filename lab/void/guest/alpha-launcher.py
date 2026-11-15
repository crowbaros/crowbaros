#!/usr/bin/python3
"""Root stand-in for the Warden in the disposable Alpha lab (ADR 0016).

Not the production broker/Warden: all apps still share one UID and public
Wayland socket. Only the shell UID can request a root-owned desktop entry.
No caller environment, inherited descriptor, command string, or supplied UID
is accepted. Settings are read through bounded, no-follow directory handles.
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
APP = 'crowbar-app'
SHELL = 'crowbar-shell'
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
            raise ValueError('Preferences must be a regular file owned only by the app user')
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
    return dict(HOME=account.pw_dir, USER=APP, LOGNAME=APP,
                PATH='/usr/local/bin:/usr/bin:/bin', LANG='C.UTF-8',
                XDG_RUNTIME_DIR=f'/run/user/{account.pw_uid}',
                XDG_CURRENT_DESKTOP='CrowbarOS', XDG_SESSION_TYPE='wayland',
                WAYLAND_DISPLAY='/run/crowbar-display/wayland-1',
                CROWBAR_SESSION='alpha-no-animations', RUST_LOG='info')


def dispatch(request):
    app = pwd.getpwnam(APP)
    if request == {'op': 'appearance'}:
        return {'ok': True, 'preferences': appearance(app.pw_dir, app.pw_uid)}
    if set(request) != {'op', 'desktop'} or request['op'] != 'launch':
        raise ValueError('Unknown request')
    path = desktop_path(request['desktop'])
    with open('/var/log/crowbar-apps.log', 'ab', buffering=0) as log:
        result = subprocess.run(
            ['/usr/bin/setpriv', '--no-new-privs', '/usr/bin/gio', 'launch', path], user=app.pw_uid, group=app.pw_gid,
            extra_groups=[], close_fds=True, start_new_session=True,
            env=app_environment(app), cwd=app.pw_dir, stdin=subprocess.DEVNULL,
            stdout=log, stderr=log, timeout=8,
        )
    if result.returncode:
        raise ValueError('The application failed to launch')
    return {'ok': True}


def serve():
    shell_uid = pwd.getpwnam(SHELL).pw_uid
    Path(SOCKET).unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(SOCKET)
        os.chown(SOCKET, 0, pwd.getpwnam(SHELL).pw_gid)
        os.chmod(SOCKET, 0o660)
        listener.listen(8)
        while True:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(2)
                try:
                    if peer_uid(connection) != shell_uid:
                        raise PermissionError('Only the shell may request a launch')
                    with connection.makefile('rb') as stream:
                        line = stream.readline(MAX_REQUEST + 1)
                    if len(line) > MAX_REQUEST or not line.endswith(b'\n'):
                        raise ValueError('Request too large or incomplete')
                    request = json.loads(line)
                    if not isinstance(request, dict):
                        raise ValueError('Request must be an object')
                    reply = dispatch(request)
                except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
                    reply = {'ok': False, 'error': str(error)}
                try:
                    connection.sendall(json.dumps(reply).encode() + b'\n')
                except OSError:
                    pass


if __name__ == '__main__':
    serve()
