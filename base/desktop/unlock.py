#!/usr/bin/python3
"""Root stand-in for the broker's unlock in the desktop on Base.

ADR 0020 has the broker end a locked session once the person has
authenticated. The broker is archived with the substrate vision, so until a
record says what replaces it, this helper is the compositor's unlock
authority, as Fulcrum's proposal has it: the compositor listens for it at a
socket of its own, checked against root's user ID, and once it does, the
shell's word no longer unlocks.

The shell's lock screen sends the password the person typed, after the
confirm chord. The helper checks it against the person's account in
/etc/shadow, through libcrypt, and only then tells the compositor to unlock.
The compositor still refuses unless the confirm chord opened the unlock entry.
A wrong password costs a pause before the next one is heard, one at a time.

`crowbar-unlock lock` asks the compositor to lock, as the suspend hook
does. `crowbar-unlock hash PASSWORD` prints a hash for /etc/shadow, as
the image build sets the lab's password.

Not the production broker: one account, one password, no record.
"""
import ctypes
import hmac
import json
import os
from pathlib import Path
import pwd
import socket
import struct
import sys
import time

SOCKET = '/run/crowbar-unlock/socket'
COMPOSITOR = '/run/crowbar-trusted/unlock/compositor'
SHELL = 'crowbar-shell'
PERSON = 'crowbar-person'
MAX_REQUEST = 4096
MAX_PASSWORD = 1024
WRONG_PAUSE = 2

libcrypt = ctypes.CDLL('libcrypt.so.1', use_errno=True)
libcrypt.crypt.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
libcrypt.crypt.restype = ctypes.c_char_p
libcrypt.crypt_gensalt.argtypes = [ctypes.c_char_p, ctypes.c_ulong, ctypes.c_char_p, ctypes.c_int]
libcrypt.crypt_gensalt.restype = ctypes.c_char_p


def peer_uid(connection):
    return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))[1]


def shadow_hash(account):
    with open('/etc/shadow', 'rb') as shadow:
        for line in shadow:
            fields = line.rstrip(b'\n').split(b':')
            if len(fields) > 1 and fields[0] == account.encode():
                return fields[1]
    return None


def password_matches(password):
    stored = shadow_hash(PERSON)
    # A locked or empty field matches nothing: `!`, `*`, or nothing at all.
    if not stored or stored[:1] in (b'!', b'*'):
        return False
    computed = libcrypt.crypt(password.encode(), stored)
    return computed is not None and hmac.compare_digest(computed, stored)


def tell_compositor(op):
    """Sends `lock` or `unlock` to the compositor, and returns its error, if
    any."""
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(COMPOSITOR)
        connection.sendall(json.dumps({'op': op}).encode() + b'\n')
        with connection.makefile('rb') as stream:
            line = stream.readline(MAX_REQUEST + 1)
    reply = json.loads(line)
    if reply.get('reply') == 'ok':
        return None
    return reply.get('message') or 'The compositor refused'


def unlock(request):
    password = request.get('password')
    if set(request) != {'password'} or not isinstance(password, str):
        raise ValueError('A request is the password, and nothing else')
    if len(password) > MAX_PASSWORD or not password_matches(password):
        time.sleep(WRONG_PAUSE)
        return {'ok': False, 'error': 'Wrong password'}
    error = tell_compositor('unlock')
    if error:
        return {'ok': False, 'error': error}
    return {'ok': True}


def serve():
    shell = pwd.getpwnam(SHELL)
    Path(SOCKET).unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(SOCKET)
        os.chown(SOCKET, 0, shell.pw_gid)
        os.chmod(SOCKET, 0o660)
        listener.listen(8)
        while True:
            connection, _ = listener.accept()
            with connection:
                connection.settimeout(2)
                try:
                    if peer_uid(connection) != shell.pw_uid:
                        raise PermissionError('Only the shell may ask to unlock')
                    with connection.makefile('rb') as stream:
                        line = stream.readline(MAX_REQUEST + 1)
                    if len(line) > MAX_REQUEST or not line.endswith(b'\n'):
                        raise ValueError('Request too large or incomplete')
                    request = json.loads(line)
                    if not isinstance(request, dict):
                        raise ValueError('Request must be an object')
                    reply = unlock(request)
                except (OSError, ValueError, TypeError) as error:
                    reply = {'ok': False, 'error': str(error)}
                print(f'UNLOCK ok={reply["ok"]}', flush=True)
                try:
                    connection.sendall(json.dumps(reply).encode() + b'\n')
                except OSError:
                    pass


def main(args):
    if args == ['lock']:
        error = tell_compositor('lock')
        if error:
            sys.exit(f'crowbar-unlock: {error}')
    elif len(args) == 2 and args[0] == 'hash':
        salt = libcrypt.crypt_gensalt(b'$6$', 0, None, 0)
        hashed = salt and libcrypt.crypt(args[1].encode(), salt)
        if not hashed:
            sys.exit('crowbar-unlock: libcrypt made no hash')
        print(hashed.decode())
    elif not args:
        serve()
    else:
        sys.exit('usage: crowbar-unlock [lock | hash PASSWORD]')


if __name__ == '__main__':
    main(sys.argv[1:])
