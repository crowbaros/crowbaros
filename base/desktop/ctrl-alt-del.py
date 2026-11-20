#!/usr/bin/python3
"""Ctrl+Alt+Del on Base's desktop.

On a console the kernel sees Ctrl+Alt+Del and sends SIGINT to init, and runit
runs /etc/runit/ctrlaltdel, which reboots. The compositor takes the keyboard
from the kernel, so it sees the chord instead, and says so at this service's
socket. The service, as root, sends init the same signal. It takes the word
of the compositor's account alone, and only one line, `ctrl-alt-del`.
"""
import os
from pathlib import Path
import pwd
import signal
import socket
import struct

SOCKET = '/run/crowbar-ctrl-alt-del/socket'
COMPOSITOR = 'crowbar-compositor'
REQUEST = b'ctrl-alt-del\n'


def peer_uid(connection):
    return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))[1]


def heard(connection, compositor_uid):
    """Whether the connection is the compositor's, saying `ctrl-alt-del`."""
    if peer_uid(connection) != compositor_uid:
        return False
    request = b''
    while len(request) < len(REQUEST) and (part := connection.recv(len(REQUEST) - len(request))):
        request += part
    return request == REQUEST


def serve(reboot=lambda: os.kill(1, signal.SIGINT)):
    compositor = pwd.getpwnam(COMPOSITOR)
    Path(SOCKET).unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(SOCKET)
        os.chown(SOCKET, 0, compositor.pw_gid)
        os.chmod(SOCKET, 0o660)
        listener.listen(4)
        while True:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(2)
                try:
                    if heard(connection, compositor.pw_uid):
                        print('CTRL-ALT-DEL rebooting', flush=True)
                        reboot()
                    else:
                        print('CTRL-ALT-DEL refused', flush=True)
                except OSError as error:
                    print(f'CTRL-ALT-DEL {error}', flush=True)


if __name__ == '__main__':
    serve()
