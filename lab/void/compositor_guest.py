#!/usr/bin/env python3
"""The Void lab's graphical guest: a compositor, and for the desktop the shell and Studio Settings.

Stdlib only, no sudo. It shares the static xbps and the package cache with
void_lab.py. Its own files live in <repo>/.lab/void-graphical (outside git).

The guest is a plain Void root on Btrfs, booted straight from its kernel. It
is not built through the apply engine and has no generations. Its desktop
runs on a TTY under runit, without systemd or a host compositor.

There are two images. The plain one holds the compositor and its probe, for
`check`. They are built from `compositor/` (ADR 0031). The desktop one holds the Alpha desktop on that compositor:
the shell and Studio Settings, with a stand-in for the Warden that registers
one compartment for the apps (ADR 0016).

    compositor_guest.py rootfs     install the guest root and the build root (rootless)
    compositor_guest.py build      build and test in Void what an image holds (--desktop for the desktop image)
    compositor_guest.py image      make the guest disk (--desktop for the desktop image)
    compositor_guest.py start      boot the guest
    compositor_guest.py view       open its graphical console
    compositor_guest.py desktop    build a desktop image, boot it, and open its console
    compositor_guest.py ssh CMD    run a command in the guest as root
    compositor_guest.py stop       power the guest off
    compositor_guest.py check      build, make the disk, boot, run every compositor check, and write an evidence log
    compositor_guest.py desktop-check   the Alpha desktop smoke check, in a fresh guest

QEMU needs the virtio-gpu device. Where the distribution splits it into its
own package (Arch: qemu-hw-display-virtio-gpu-pci), install that, or point
QEMU_MODULE_DIR at a directory that holds the modules.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import time

import void_lab

LAB = void_lab.LAB
ROOT = void_lab.ROOT
CFG = void_lab.CFG["graphical"]
WORK = ROOT / ".lab/void-graphical"
GUEST_ROOT = WORK / "rootfs"
BUILD_ROOT = WORK / "buildroot"
TARGET = WORK / "target"
DISK = WORK / "disk.raw"
KEY = WORK / "id_ed25519"
PIDFILE = WORK / "qemu.pid"
QMP_SOCK = WORK / "qmp.sock"
SERIAL_LOG = WORK / "serial.log"
VNC = [WORK / "vnc0.sock", WORK / "vnc1.sock"]
# The compositor, Fulcrum, is built from this repository (ADR 0031).
COMPOSITOR = "compositor"
# The compositor builds in a part of the target directory of its own. The
# apps stay where the desktop image has always built them.
COMPOSITOR_PART = "compositor"
APPS = [("apps/shell", ""), ("apps/studio-settings", "")]
COMPOSITOR_BINARIES = ["crowbar-compositor", "crowbar-probe"]
APP_BINARIES = ["crowbaros-shell", "crowbaros-studio-settings"]

run = void_lab.run


def install_root(target, packages, log_name):
    void_lab.fetch()
    WORK.mkdir(parents=True, exist_ok=True)
    xbps = void_lab.xbps_dir()
    env = dict(os.environ, XBPS=str(xbps / "usr/bin"), ROOTFS=str(target),
               REPO=void_lab.CFG["void_repository"], KEYS=str(xbps / "var/db/xbps/keys"),
               PKGCACHE=str(void_lab.CACHE / "packages"), PACKAGES=" ".join(packages))
    with (WORK / log_name).open("w") as log:
        run("unshare", "-r", "sh", LAB / "host/build-rootfs.sh", env=env, stdout=log,
            stderr=subprocess.STDOUT)
    print(f"Installed {target} (log: {WORK / log_name})")


def rootfs(force=False):
    for target, packages, log_name in (
            (GUEST_ROOT, CFG["guest_packages"], "build-rootfs.log"),
            (BUILD_ROOT, CFG["build_packages"], "build-buildroot.log")):
        if target.exists() and not force:
            print(f"{target} exists; pass --force to rebuild it")
            continue
        install_root(target, packages, log_name)


def projects(desktop):
    """What an image is built from: each project and its part of the target directory.

    Both images hold the compositor. The desktop one adds the shell and Studio
    Settings."""
    compositor = (COMPOSITOR, COMPOSITOR_PART)
    return [compositor, *APPS] if desktop else [compositor]


def binaries(desktop):
    """The programs an image holds, and where the build leaves each."""
    built = {name: TARGET / COMPOSITOR_PART / "release" / name for name in COMPOSITOR_BINARIES}
    if desktop:
        built.update({name: TARGET / "release" / name for name in APP_BINARIES})
    return built


def build(desktop=False):
    """Builds and tests what an image holds against Void's libraries, inside the build root.

    The host's Cargo registry and git checkouts are bound in read-write so the
    build runs offline from the crates the lockfile already names.
    """
    if not BUILD_ROOT.exists():
        rootfs()
    if desktop:
        run(sys.executable, ROOT / "apps/shell/packaging/prepare-gpui.py")
    built_from = projects(desktop)
    for project, _ in built_from:
        run("cargo", "fetch", "--locked", "--target", "x86_64-unknown-linux-gnu",
            "--manifest-path", ROOT / project / "Cargo.toml")
    TARGET.mkdir(parents=True, exist_ok=True)
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    env = dict(os.environ, B=str(BUILD_ROOT), SRC=str(ROOT), TARGET=str(TARGET),
               PROJECTS=" ".join(f"{project}:{part}" for project, part in built_from),
               CARGO_REGISTRY=str(cargo_home / "registry"), CARGO_GIT=str(cargo_home / "git"))
    run("unshare", "-r", "-m", "sh", LAB / "host/build-compositor.sh", env=env)
    for name, path in binaries(desktop).items():
        print(f"{path}: sha256 {void_lab.sha256(path)}")


def image(desktop=False):
    if running():
        raise SystemExit("Stop the graphical guest before rebuilding its disk")
    if not GUEST_ROOT.exists():
        rootfs()
    if not all(path.exists() for path in binaries(desktop).values()):
        build(desktop)
    if not KEY.exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "crowbar-void-graphical", "-f", KEY)
    if DISK.exists():
        backup = WORK / f"disk-before-rebuild-{time.time_ns()}.raw"
        run("cp", "--reflink=auto", "--sparse=always", DISK, backup)
        print(f"Preserved the previous guest disk at {backup}")
    env = dict(os.environ, ROOTFS=str(GUEST_ROOT), WORK=str(WORK), OUT=str(DISK),
               BINARIES=" ".join(str(path) for path in binaries(desktop).values()),
               PUBKEY=str(KEY) + ".pub", ROOT_LABEL=CFG["root_label"], ROOT_GIB=str(CFG["root_gib"]),
               REPO_ROOT=str(ROOT), DESKTOP="1" if desktop else "0")
    run("unshare", "-r", "sh", LAB / "host/build-graphical-image.sh", env=env)


def view():
    if not running():
        raise SystemExit("Start the graphical guest first")
    subprocess.Popen(["remote-viewer", "--title=CrowbarOS on Void Linux", "--auto-resize=never",
                      f"vnc://127.0.0.1:{CFG['vnc_port']}"],
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)


def running():
    if not PIDFILE.exists():
        return False
    try:
        os.kill(int(PIDFILE.read_text().strip()), 0)
        return True
    except (ProcessLookupError, ValueError):
        return False


def start():
    if running():
        raise SystemExit("The graphical guest is already running; `compositor_guest.py stop` first")
    if not DISK.exists():
        raise SystemExit("No guest disk; run `compositor_guest.py image` first")
    for path in (QMP_SOCK, PIDFILE, *VNC):
        path.unlink(missing_ok=True)
    (WORK / "known_hosts").unlink(missing_ok=True)
    kernel = sorted((GUEST_ROOT / "boot").glob("vmlinuz-*"))[-1]
    initrd = GUEST_ROOT / "boot" / kernel.name.replace("vmlinuz-", "initramfs-")
    initrd = initrd.with_name(initrd.name + ".img")
    run("qemu-system-x86_64", "-name", "crowbar-void-graphical",
        "-machine", "q35,accel=kvm", "-cpu", "host",
        "-m", str(CFG["memory_mib"]), "-smp", str(CFG["vcpus"]),
        "-kernel", kernel, "-initrd", initrd,
        "-append", "root=/dev/vda rw console=ttyS0 console=tty0 loglevel=4",
        "-drive", f"file={DISK},format=raw,if=virtio,cache=unsafe",
        "-netdev", f"user,id=n0,hostfwd=tcp:127.0.0.1:{CFG['ssh_port']}-:22",
        "-device", "virtio-net-pci,netdev=n0",
        # One GPU with two connectors. The second starts unplugged. No default
        # VGA card: it would be a second DRM device, and the primary one.
        "-vga", "none", "-device", "virtio-gpu-pci,id=gpu0,max_outputs=2,xres=1280,yres=960",
        "-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci",
        # A VNC server per connector. A VNC client that sets a desktop size on
        # the second one is how the lab plugs the second monitor in.
        "-vnc", (f"127.0.0.1:{CFG['vnc_port'] - 5900},display=gpu0,head=0"
                  if CFG.get("vnc_port") else f"unix:{VNC[0]},display=gpu0,head=0"),
        "-vnc", f"unix:{VNC[1]},display=gpu0,head=1",
        "-qmp", f"unix:{QMP_SOCK},server=on,wait=off",
        "-chardev", f"file,id=ser0,path={SERIAL_LOG}", "-serial", "chardev:ser0",
        "-display", "none", "-daemonize", "-pidfile", PIDFILE)


class Qmp:
    """A QMP connection: QEMU's JSON control protocol."""

    def __init__(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(10)
        self.sock.connect(str(QMP_SOCK))
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
        path.unlink(missing_ok=True)
        self.command("screendump", filename=str(path), device="gpu0", head=head, format="png")


def plug_monitor(head, width, height):
    """Plugs a monitor into a connector by acting as a VNC client that asks
    for a desktop size. QEMU passes the size to virtio-gpu, which raises a
    hotplug event in the guest."""
    if head == 0 and CFG.get("vnc_port"):
        sock = socket.create_connection(("127.0.0.1", CFG["vnc_port"]), timeout=5)
    else:
        sock = socket.socket(socket.AF_UNIX)
        sock.connect(str(VNC[head]))
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
    # SetDesktopSize: type 251, pad, width, height, one screen, pad, then the
    # screen: id, x, y, width, height, flags.
    sock.sendall(struct.pack(">BxHHBx", 251, width, height, 1)
                 + struct.pack(">IHHHHI", 0, 0, 0, width, height, 0))
    time.sleep(1)
    sock.close()


def stop():
    if not running():
        print("The graphical guest is not running")
        return
    pid = int(PIDFILE.read_text().strip())
    # Let the guest flush its filesystem before QEMU exits. The fallback also
    # handles a guest that did not boot far enough to bring up SSH.
    try:
        ssh("nohup sh -c 'sleep 1; poweroff' >/dev/null 2>&1 &", check=False, timeout=8)
        for _ in range(150):
            if not running():
                PIDFILE.unlink(missing_ok=True)
                print("Powered off the graphical guest")
                return
            time.sleep(0.2)
    except (OSError, subprocess.TimeoutExpired):
        pass
    try:
        Qmp().command("quit")
    except (OSError, RuntimeError, ValueError):
        os.kill(pid, 15)
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.2)
    PIDFILE.unlink(missing_ok=True)
    print("Stopped the graphical guest")


def ssh_args():
    return ["ssh", "-i", str(KEY), "-p", str(CFG["ssh_port"]), "-o", "IdentitiesOnly=yes",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={WORK / 'known_hosts'}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "root@127.0.0.1"]


def ssh(command, check=True, timeout=120):
    result = subprocess.run(ssh_args() + [command], text=True, capture_output=True,
                            timeout=timeout)
    if check and result.returncode != 0:
        raise RuntimeError(f"guest command failed ({result.returncode}): {command}\n"
                           f"{result.stdout}{result.stderr}")
    return result


def wait_ssh(timeout=180):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if ssh("true", check=False, timeout=15).returncode == 0:
                return
        except subprocess.TimeoutExpired:
            pass
        time.sleep(2)
    raise RuntimeError("the guest did not come up on SSH")


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("rootfs").add_argument("--force", action="store_true")
    sub.add_parser("build").add_argument("--desktop", action="store_true")
    sub.add_parser("image").add_argument("--desktop", action="store_true")
    sub.add_parser("start")
    sub.add_parser("view")
    sub.add_parser("desktop")
    sub.add_parser("stop")
    sub.add_parser("ssh").add_argument("guest_command", nargs=argparse.REMAINDER)
    sub.add_parser("check")
    sub.add_parser("desktop-check").add_argument("--build-cache", type=Path)
    args = parser.parse_args()
    if args.command == "rootfs":
        rootfs(args.force)
    elif args.command == "build":
        build(args.desktop)
    elif args.command == "image":
        image(args.desktop)
    elif args.command == "start":
        start()
        wait_ssh()
    elif args.command == "stop":
        stop()
    elif args.command == "view":
        view()
    elif args.command == "desktop":
        if running():
            raise SystemExit("The graphical guest is running; stop it before rebuilding the desktop")
        rootfs()
        build(desktop=True)
        image(desktop=True)
        start()
        wait_ssh()
        view()
    elif args.command == "ssh":
        result = ssh(shlex.join(args.guest_command) or "true", check=False)
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        sys.exit(result.returncode)
    elif args.command == "desktop-check":
        import desktop_check
        sys.argv = [sys.argv[0]] + (["--build-cache", str(args.build_cache)] if args.build_cache else [])
        sys.exit(desktop_check.main())
    elif args.command == "check":
        import compositor_check
        sys.exit(compositor_check.main())


if __name__ == "__main__":
    main()
