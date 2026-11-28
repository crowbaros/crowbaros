#!/usr/bin/env python3
"""Native review fixture, run as the guest's shell UID in place of its connector.

Leaves Wayland untouched. The file /tmp/picker-fault selects one control request
(clipboard_history or clipboard_clear) to answer with an error, or with a delayed
normal reply when prefixed by delay:. No fault hook is shipped in the shell.
"""
import json
import os
from pathlib import Path
import socket
import signal
import sys
import struct
import subprocess
import threading
import time


def connect(name):
    channel = socket.socket(socket.AF_UNIX)
    channel.connect('/run/crowbar-trusted/compositor/' + name)
    return channel


def frame(channel):
    def exact(count):
        data = bytearray()
        while len(data) < count:
            part = channel.recv(count - len(data))
            if not part:
                raise EOFError
            data.extend(part)
        return bytes(data)
    header = exact(4)
    size = struct.unpack('>I', header)[0]
    if size > 4 * 1024 * 1024:
        raise ValueError('oversized fixture frame')
    return header + exact(size)


control = connect('control')
wayland = connect('wayland')
proxy, inherited = socket.socketpair()
env = dict(os.environ, WAYLAND_SOCKET=str(wayland.fileno()),
           CROWBAR_SHELL_CONTROL_FD=str(inherited.fileno()))
child = subprocess.Popen(['/usr/local/bin/crowbaros-shell'], env=env,
                         pass_fds=[wayland.fileno(), inherited.fileno()])
def terminate(_signal, _frame):
    child.terminate()
    sys.exit(0)


signal.signal(signal.SIGTERM, terminate)
wayland.close()
inherited.close()
output = threading.Lock()


def incoming():
    try:
        while True:
            data = frame(control)
            with output:
                proxy.sendall(data)
    except (EOFError, OSError):
        proxy.close()


threading.Thread(target=incoming, daemon=True).start()

def notify_refresh():
    previous = None
    while child.poll() is None:
        try:
            path = Path('/tmp/picker-fault')
            stamp = path.stat().st_mtime_ns
            if stamp != previous and path.read_text().strip() == 'clipboard_history':
                message = b'{"event":"clipboard_changed"}'
                with output:
                    proxy.sendall(struct.pack('>I', len(message)) + message)
            previous = stamp
        except FileNotFoundError:
            previous = None
        except OSError:
            return
        time.sleep(0.05)


threading.Thread(target=notify_refresh, daemon=True).start()
try:
    while True:
        data = frame(proxy)
        try:
            fault = Path('/tmp/picker-fault').read_text().strip()
        except FileNotFoundError:
            fault = ''
        op = json.loads(data[4:]).get('op')
        if op and fault == op:
            message = b'{"reply":"error","message":"Native review fixture refused the request"}'
            with output:
                proxy.sendall(struct.pack('>I', len(message)) + message)
        else:
            if op and fault == 'delay:' + op:
                time.sleep(0.4)
            control.sendall(data)
except (EOFError, OSError):
    pass
finally:
    control.close()
    child.wait()
