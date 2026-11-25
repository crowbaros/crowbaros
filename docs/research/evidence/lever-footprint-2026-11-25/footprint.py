"""Lever's footprint, measured in the guest as root: how long from runit
starting its service until the shell's account is answered version 1 and
`status` on the surface socket, and what the idle process holds in memory."""
import json
import os
import pwd
import re
import socket
import statistics
import struct
import subprocess
import sys
import time

SOCKET = "/run/crowbar-lever/surface"
VERSION_FRAME = bytes.fromhex("0000000d7b2276657273696f6e223a317d")
STATUS_FRAME = bytes.fromhex("0000000f7b226f70223a22737461747573227d")
SERVICE = "/var/service/crowbar-lever"
SHELL = pwd.getpwnam("crowbar-shell")


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        more = sock.recv(n - len(buf))
        if not more:
            break
        buf += more
    return buf


def answered():
    """Whether Lever answers version 1 and status within a second."""
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(1)
    try:
        s.connect(SOCKET)
        s.sendall(VERSION_FRAME)
        if recv_exact(s, len(VERSION_FRAME)) != VERSION_FRAME:
            return False
        s.sendall(STATUS_FRAME)
        hdr = recv_exact(s, 4)
        if len(hdr) < 4:
            return False
        (length,) = struct.unpack(">I", hdr)
        body = recv_exact(s, length)
        reply = json.loads(body.decode())
        return reply.get("reply") == "status" and reply.get("harness_started") is False
    except (OSError, ValueError):
        return False
    finally:
        s.close()


def pid():
    out = subprocess.run(["sv", "status", SERVICE], capture_output=True, text=True).stdout
    found = re.match(r"run: \S+ \(pid (\d+)\)", out)
    return int(found[1]) if found else None


def until_answered(act):
    """Seconds from `act` until the shell's account is answered. A child that
    is the shell's account, with no other group, asks every millisecond, and
    only once `act` is done: a Lever that `act` kills cannot answer for the
    one that follows it."""
    go, went = os.pipe()
    read, write = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(went)
        os.close(read)
        os.setgroups([])
        os.setgid(SHELL.pw_gid)
        os.setuid(SHELL.pw_uid)
        os.read(go, 1)
        deadline = time.monotonic() + 30
        while not answered():
            if time.monotonic() > deadline:
                os._exit(1)
            time.sleep(0.001)
        os.write(write, repr(time.monotonic()).encode())
        os._exit(0)
    os.close(go)
    os.close(write)
    started = time.monotonic()
    act()
    os.write(went, b"g")
    at = os.read(read, 64)
    os.waitpid(child, 0)
    if not at:
        raise SystemExit("Lever did not answer the shell's account within 30 seconds")
    return float(at) - started


def down():
    subprocess.run(["sv", "down", SERVICE], check=True)
    deadline = time.monotonic() + 30
    while pid() is not None or os.path.exists(SOCKET):
        if time.monotonic() > deadline:
            raise SystemExit("Lever or its socket was still there 30 seconds after sv down")
        time.sleep(0.01)


def up():
    subprocess.run(["sv", "up", SERVICE], check=True)


def age(process):
    """Seconds since the process started."""
    stat = open(f"/proc/{process}/stat").read()
    started = int(stat.rsplit(")", 1)[1].split()[19]) / os.sysconf("SC_CLK_TCK")
    return float(open("/proc/uptime").read().split()[0]) - started


def memory(process):
    rollup = open(f"/proc/{process}/smaps_rollup").read()
    return {name: int(re.search(rf"^{name}:\s+(\d+) kB", rollup, re.M)[1])
            for name in ("Rss", "Pss", "Pss_Anon", "Pss_File")}


def show(name, seconds):
    ms = sorted(round(s * 1000) for s in seconds)
    print(f"{name}: median {round(statistics.median(ms))} ms, least {ms[0]}, most {ms[-1]}, of {len(ms)}: {ms}")


if sys.argv[1] == "start":
    warm, cold, killed, paced = [], [], [], []
    for _ in range(10):
        time.sleep(1.5)
        down()
        warm.append(until_answered(up))
    for _ in range(5):
        time.sleep(1.5)
        down()
        os.sync()
        open("/proc/sys/vm/drop_caches", "w").write("3\n")
        cold.append(until_answered(up))
    for _ in range(5):
        time.sleep(1.5)
        process = pid()
        killed.append(until_answered(lambda: os.kill(process, 9)))
    time.sleep(1.5)
    for _ in range(5):
        down()
        paced.append(until_answered(up))
    show("sv up to answered", warm)
    show("sv up to answered, page cache dropped first", cold)
    show("kill -9 to answered", killed)
    show("sv up to answered, stopped within a second of its start", paced[1:])
elif sys.argv[1] == "memory":
    process = pid()
    values = memory(process)
    print(f"process {process}, up {age(process):.0f} s: " + ", ".join(
        f"{name} {kb / 1024:.2f} MiB" for name, kb in values.items()))
