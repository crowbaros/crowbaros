#!/usr/bin/python3
"""Connect the separately supervised shell without inheriting seat access."""
import os
import pwd
import socket
import struct
import time

channels = []
for name in ('wayland', 'control'):
    for attempt in range(100):
        channel = socket.socket(socket.AF_UNIX)
        try:
            channel.connect('/run/crowbar-trusted/compositor/' + name)
            peer = struct.unpack('3i', channel.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if peer[1] != pwd.getpwnam('crowbar-compositor').pw_uid:
                raise PermissionError('Wrong compositor service identity')
            channel.set_inheritable(True)
            channels.append(channel)
            break
        except (FileNotFoundError, ConnectionRefusedError):
            channel.close()
            time.sleep(0.2)
    else:
        raise SystemExit('Compositor service did not become ready')
os.environ['WAYLAND_SOCKET'] = str(channels[0].fileno())
os.environ['CROWBAR_SHELL_CONTROL_FD'] = str(channels[1].fileno())
os.execv('/usr/local/bin/crowbaros-shell', ['crowbaros-shell'])
