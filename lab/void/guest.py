"""Drives an isolated QEMU guest booted from Base's image under UEFI.

Both the compositor check and the desktop check use this module. Every run
owns a directory with its own copy of Base's disk, firmware variables, SSH
port, and Unix QMP and VNC sockets, and reaches the guest with Base's SSH key.
A run that passes deletes its disk.
The sockets are in a short directory of the run's own under the system's
temporary directory, since a Unix socket's path is at most 107 bytes and a
worktree's run directory can be longer than that.
"""
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

LAB = Path(__file__).resolve().parent
ROOT = LAB.parents[1]
if str(ROOT / "base") not in sys.path:
    sys.path.insert(0, str(ROOT / "base"))

import void_base  # noqa: E402

# A client of one of Fulcrum's own channels, for inside the guest: the shell's
# control channel or the unlock authority's socket. Each message is a frame, a
# length in four bytes with the most significant first and then JSON, and each
# end's first frame states the version (compositor/README.md, "The control
# channel"). It sends one request and prints its reply, passing over the
# events the shell is sent unasked. It prints nothing when the compositor
# closes the connection or states another version.
FULCRUM_CLIENT = """
import socket, struct, sys
def send(message):
    s.sendall(struct.pack(">I", len(message)) + message)
def read():
    length = f.read(4)
    if len(length) < 4:
        raise EOFError
    message = f.read(struct.unpack(">I", length)[0])
    if len(message) < struct.unpack(">I", length)[0]:
        raise EOFError
    return message
s = socket.socket(socket.AF_UNIX)
s.settimeout(5)
try:
    s.connect(sys.argv[1])
    f = s.makefile("rb")
    send(b'{"version":1}')
    if read() != b'{"version":1}':
        raise EOFError
    send(sys.argv[2].encode())
    while (message := read()).startswith(b'{"event":'):
        pass
    print(message.decode(errors="replace"))
except (OSError, EOFError):
    pass
"""
# The frame that states the version, for a check that only knocks.
FULCRUM_VERSION_FRAME = b'\x00\x00\x00\x0d{"version":1}'


def ask_fulcrum(path, request):
    """The guest command that sends `request` to Fulcrum's socket at `path`
    and prints the reply, or nothing if Fulcrum gives none."""
    return f"python3 -c {shlex.quote(FULCRUM_CLIENT)} {shlex.quote(path)} {shlex.quote(request)}"


class Qmp:
    """A QMP connection: QEMU's JSON control protocol."""

    def __init__(self, sock_path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(10)
        self.sock.connect(str(sock_path))
        self.file = self.sock.makefile("rw")
        self.read()  # greeting
        self.command("qmp_capabilities")

    def read(self):
        while True:
            line = self.file.readline()
            if not line:
                raise ConnectionError("QMP disconnected")
            message = json.loads(line)
            if "event" not in message:
                return message

    def command(self, name, **arguments):
        self.file.write(json.dumps({"execute": name, "arguments": arguments}) + "\n")
        self.file.flush()
        reply = self.read()
        if "error" in reply:
            raise RuntimeError(f"QMP {name}: {reply['error']}")
        return reply.get("return")

    def key(self, qcode, down):
        """One key goes down or up on the guest's keyboard."""
        self.command("input-send-event", events=[
            {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}])
        time.sleep(0.15)

    def pointer(self, x, y):
        """Moves the tablet. x and y are fractions of the whole desktop."""
        self.command("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": int(x * 0x7fff)}},
            {"type": "abs", "data": {"axis": "y", "value": int(y * 0x7fff)}}])
        time.sleep(0.3)

    def click(self):
        for down in (True, False):
            self.command("input-send-event", events=[
                {"type": "btn", "data": {"down": down, "button": "left"}}])
            time.sleep(0.15)

    def screendump(self, path, head):
        path = Path(path)
        path.unlink(missing_ok=True)
        self.command("screendump", filename=str(path), device="gpu0", head=head, format="png")

    def close(self):
        self.file.close()
        self.sock.close()


def png_pixels(path):
    """The pixels of a screendump: its width, its height, and its rows, each
    the bytes of its pixels, three a pixel. It reads what QEMU writes and no
    more: eight bits a channel, RGB or RGBA, not interlaced."""
    data = Path(path).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    at, packed, header = 8, b"", None
    while at < len(data):
        length, kind = struct.unpack(">I4s", data[at:at + 8])
        body = data[at + 8:at + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            packed += body
        at += 12 + length
    width, height, depth, colour, _, _, interlace = header
    if depth != 8 or colour not in (2, 6) or interlace:
        raise ValueError(f"{path}: a PNG of depth {depth}, colour type {colour}, "
                         f"interlace {interlace} is not one this reads")
    channels = 3 if colour == 2 else 4
    stride = width * channels
    raw = zlib.decompress(packed)
    rows, above = [], bytes(stride)
    for y in range(height):
        start = y * (stride + 1)
        kind, row = raw[start], bytearray(raw[start + 1:start + 1 + stride])
        # Each filter adds a prediction from the pixel to the left, the row
        # above, or both, as the PNG specification's section 9 has them.
        if kind == 1:
            for i in range(channels, stride):
                row[i] = (row[i] + row[i - channels]) & 255
        elif kind == 2:
            row = bytearray((a + b) & 255 for a, b in zip(row, above))
        elif kind == 3:
            for i in range(stride):
                left = row[i - channels] if i >= channels else 0
                row[i] = (row[i] + ((left + above[i]) >> 1)) & 255
        elif kind == 4:
            for i in range(stride):
                left = row[i - channels] if i >= channels else 0
                corner = above[i - channels] if i >= channels else 0
                estimate = left + above[i] - corner
                near = min((abs(estimate - left), 0, left), (abs(estimate - above[i]), 1, above[i]),
                           (abs(estimate - corner), 2, corner))
                row[i] = (row[i] + near[2]) & 255
        elif kind != 0:
            raise ValueError(f"{path}: row {y} has the unknown filter {kind}")
        above = bytes(row)
        rows.append(above if channels == 3 else
                    bytes(c for i in range(0, stride, 4) for c in above[i:i + 3]))
    return width, height, rows


def build_base(log_path, build=True):
    """Build Base's image with the lab account, logging output to `log_path`,
    and return each program's digest. Without `build`, the image and the
    programs are taken as the last build left them."""
    if not build:
        lack = [path for path in (void_base.BASE_DISK, *void_base.binaries().values())
                if not path.exists()]
        if lack:
            raise RuntimeError(f"--no-build needs the last build's {lack[0]}; run without it first")
        # The image must be one a check built, with the lab account: after a
        # person's own `void_base.py build`, the guest refuses the checks' login.
    else:
        with Path(log_path).open("w") as log:
            result = subprocess.run(
                [sys.executable, void_base.HERE / "void_base.py", "build", "--lab-account"],
                stdout=log, stderr=subprocess.STDOUT,
            )
        if result.returncode:
            raise RuntimeError(f"Base's build failed; see {log_path}")
    return {name: void_base.sha256(path) for name, path in void_base.binaries().items()}


# What a run made with `--no-build` says of itself, in its log and in the name
# of its results: it shows what the checks make of an image built earlier,
# from files that may have changed since, so it is no evidence for a pull
# request.
NOT_EVIDENCE = ("NOT EVIDENCE: --no-build booted the image the last build left, which this run "
                "did not build from the files as they are now")


def add_arguments(parser):
    """The options both checks take."""
    parser.add_argument("--no-build", dest="build", action="store_false",
                        help="boot the image the last build left, to try a change to the check "
                             "itself sooner. The run says it is not evidence, and a pull "
                             "request needs a run without this")


class Guest:
    """One isolated UEFI VM run on a copy of Base's disk."""

    def __init__(self, work, name, ssh_port, revision="", dirty=False):
        self.work = Path(work)
        self.name = name
        self.ssh_port = ssh_port
        self.revision = revision
        self.dirty = dirty
        self.disk = self.work / "disk.raw"
        self.nvram = self.work / "nvram.fd"
        self.pidfile = self.work / "qemu.pid"
        run = hashlib.sha256(os.fsencode(self.work.resolve())).hexdigest()[:16]
        self.sockets = Path(tempfile.gettempdir()) / f"crowbaros-{run}"
        self.qmp_sock = self.sockets / "qmp.sock"
        self.serial_log = self.work / "serial.log"
        self.known_hosts = self.work / "known_hosts"
        self.vnc = [self.sockets / f"vnc{n}.sock" for n in range(2)]
        self.key = void_base.KEY

    @classmethod
    def create(cls, runs_dir, name):
        runs_dir = Path(runs_dir)
        runs_dir.mkdir(parents=True, exist_ok=True)
        work = Path(tempfile.mkdtemp(prefix=time.strftime("%Y%m%dT%H%M%S-"), dir=runs_dir))
        with socket.socket() as free_port:
            free_port.bind(("127.0.0.1", 0))
            port = free_port.getsockname()[1]
        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=void_base.ROOT, text=True).strip()
        dirty = bool(subprocess.check_output(
            ["git", "status", "--porcelain"], cwd=void_base.ROOT))
        vm = cls(work, name, port, revision=revision, dirty=dirty)
        (work / "run.json").write_text(json.dumps({
            "workspace": str(work),
            "ssh_port": port,
            "image": str(void_base.BASE_DISK),
            "revision": revision,
            "dirty": dirty,
        }, indent=2) + "\n")
        return vm

    def running(self):
        try:
            os.kill(int(self.pidfile.read_text().strip()), 0)
            return True
        except (FileNotFoundError, ProcessLookupError, ValueError):
            return False

    def start(self):
        """Boot this run's own grown copy of Base's image through UEFI."""
        self.sockets.mkdir(mode=0o700, exist_ok=True)
        for path in (self.qmp_sock, self.pidfile, self.known_hosts, *self.vnc):
            path.unlink(missing_ok=True)
        subprocess.run(
            ["cp", "--reflink=auto", "--sparse=always", void_base.BASE_DISK, self.disk],
            check=True,
        )
        # Larger than the image, as a person's disk is, so the root grows on boot.
        os.truncate(self.disk, max(self.disk.stat().st_size, void_base.CFG["disk_gib"] << 30))
        shutil.copyfile(void_base.CFG["firmware_vars"], self.nvram)
        try:
            self._boot()
        except BaseException:
            shutil.rmtree(self.sockets, ignore_errors=True)
            raise

    def _boot(self):
        void_base.run(
            *void_base.qemu(self.name, self.disk, self.nvram, self.ssh_port),
            # A VNC server per connector. A VNC client that sets a desktop
            # size on the second one plugs the second monitor in.
            "-vnc", f"unix:{self.vnc[0]},display=gpu0,head=0",
            "-vnc", f"unix:{self.vnc[1]},display=gpu0,head=1",
            "-qmp", f"unix:{self.qmp_sock},server=on,wait=off",
            "-chardev", f"file,id=ser0,path={self.serial_log}", "-serial", "chardev:ser0",
            "-display", "none", "-daemonize", "-pidfile", self.pidfile,
        )

    def ssh_args(self, user="root"):
        return [
            "ssh", "-i", str(self.key), "-p", str(self.ssh_port),
            "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={self.known_hosts}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR",
            f"{user}@127.0.0.1",
        ]

    def ssh(self, command, check=True, timeout=120, user="root", input=None):
        result = subprocess.run(
            self.ssh_args(user) + [command], input=input, text=True, capture_output=True, timeout=timeout,
        )
        if check and result.returncode != 0:
            raise RuntimeError(
                f"guest command failed ({result.returncode}): {command}\n"
                f"{result.stdout}{result.stderr}"
            )
        return result

    def wait_ssh(self, timeout=180):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.running():
                raise RuntimeError(f"QEMU exited while waiting for SSH; see {self.serial_log}")
            try:
                if self.ssh("true", check=False, timeout=15).returncode == 0:
                    return
            except subprocess.TimeoutExpired:
                pass
            time.sleep(2)
        raise RuntimeError(f"the guest did not come up on SSH; see {self.serial_log}")

    def qmp(self):
        return Qmp(self.qmp_sock)

    def plug_monitor(self, head, width, height):
        """Plug a monitor into a connector by asking QEMU's VNC server for a
        desktop size. QEMU passes the size to virtio-gpu, which raises a
        hotplug event in the guest."""
        sock = socket.socket(socket.AF_UNIX)
        try:
            sock.connect(str(self.vnc[head]))
            sock.settimeout(5)
            sock.recv(12)                       # server version
            sock.sendall(b"RFB 003.008\n")
            count = sock.recv(1)[0]             # security types
            sock.recv(count)
            sock.sendall(b"\x01")               # none
            sock.recv(4)                        # security result
            sock.sendall(b"\x01")               # shared
            header = sock.recv(24)              # server init
            name_length = struct.unpack(">I", header[20:24])[0]
            sock.recv(name_length)
            # SetDesktopSize: type 251, pad, width, height, one screen, pad,
            # then the screen: id, x, y, width, height, flags.
            sock.sendall(
                struct.pack(">BxHHBx", 251, width, height, 1)
                + struct.pack(">IHHHHI", 0, 0, 0, width, height, 0)
            )
            time.sleep(1)
        finally:
            sock.close()

    def stop(self):
        try:
            self._stop()
        finally:
            shutil.rmtree(self.sockets, ignore_errors=True)

    def discard_disk(self):
        """Delete the run's disk, 12 GB a run, and say whether it is gone. A
        run that passed has no use for it; one that failed keeps it, to be
        booted and looked at. A guest that did not stop keeps its disk too:
        it is still writing it."""
        if self.running():
            return False
        self.disk.unlink(missing_ok=True)
        return True

    def _stop(self):
        if not self.running():
            return
        pid = int(self.pidfile.read_text().strip())
        # Let the guest flush its filesystem before QEMU exits. The fallback
        # also handles a guest that did not boot far enough to bring up SSH.
        try:
            self.ssh("nohup sh -c 'sleep 1; poweroff' >/dev/null 2>&1 &", check=False, timeout=8)
            for _ in range(150):
                if not self.running():
                    self.pidfile.unlink(missing_ok=True)
                    return
                time.sleep(0.2)
        except (OSError, subprocess.TimeoutExpired):
            pass
        try:
            qmp = self.qmp()
            try:
                qmp.command("quit")
            finally:
                qmp.close()
        except (OSError, RuntimeError, ValueError):
            try:
                os.kill(pid, 15)
            except ProcessLookupError:
                pass
        for _ in range(50):
            if not self.running():
                # Only a guest that is gone loses its PID file: one that
                # outlives all of this is still known to be running.
                self.pidfile.unlink(missing_ok=True)
                break
            time.sleep(0.2)
