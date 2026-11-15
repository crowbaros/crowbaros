#!/usr/bin/env python3
"""Milestone 1 lab: a Void Linux VM on Btrfs driven by the Crowbar apply engine.

Stdlib only, no sudo. Root-owned work happens inside `unshare -r` user
namespaces. Downloads live in ~/.cache/crowbaros/void; images, VM runtime
files, and evidence logs live in <repo>/.lab/void (both outside git).

    void_lab.py fetch       download and verify the static xbps
    void_lab.py rootfs      install the Void base root (rootless)
    void_lab.py image       build the crowbar binary and the bootable base disk
    void_lab.py start       boot a fresh copy of the base disk (or --keep the current one)
    void_lab.py ssh [CMD]   run a command in the guest as root
    void_lab.py stop        power the VM off
    void_lab.py milestone   run the whole Milestone 1 loop and record evidence
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tarfile
import threading
import time

LAB = Path(__file__).resolve().parent
WORKSPACE = LAB.parent
ROOT = WORKSPACE.parent
CFG = json.loads((LAB / "config.json").read_text())
CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "crowbaros/void"
WORK = ROOT / ".lab/void"
ROOTFS = WORK / "rootfs"
BASE_DISK = WORK / "base.raw"
DISK = WORK / "disk.raw"
NVRAM = WORK / "nvram.fd"
KEY = WORK / "id_ed25519"
SERIAL_SOCK = WORK / "serial.sock"
MONITOR_SOCK = WORK / "monitor.sock"
SERIAL_LOG = WORK / "serial.log"
PIDFILE = WORK / "qemu.pid"
MUSL = "x86_64-unknown-linux-musl"
CROWBAR_BIN = WORKSPACE / "target" / MUSL / "release/crowbar"
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][0-9A-Za-z]|\x1b[=>]|\r")


def run(*args, **kwargs):
    print("+", shlex.join(str(a) for a in args), flush=True)
    return subprocess.run([str(a) for a in args], check=True, text=True, **kwargs)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def xbps_dir():
    return CACHE / "xbps-static"


def fetch():
    CACHE.mkdir(parents=True, exist_ok=True)
    url = CFG["xbps_static_url"]
    tarball = CACHE / url.rsplit("/", 1)[1]
    if not tarball.exists():
        part = tarball.with_name(tarball.name + ".part")
        run("curl", "-fL", "--retry", "3", "-o", part, url)
        part.replace(tarball)
    if sha256(tarball) != CFG["xbps_static_sha256"]:
        raise SystemExit(f"{tarball} does not match the pinned SHA-256; refusing to use it")
    target = xbps_dir()
    if not (target / "usr/bin/xbps-install.static").exists():
        shutil.rmtree(target, ignore_errors=True)
        target.mkdir()
        with tarfile.open(tarball) as archive:
            archive.extractall(target, filter="data")
    print(f"Verified static xbps at {target}")


def rootfs(force=False):
    if ROOTFS.exists() and not force:
        print(f"{ROOTFS} exists; pass --force to rebuild it")
        return
    fetch()
    WORK.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, XBPS=str(xbps_dir() / "usr/bin"), ROOTFS=str(ROOTFS),
               REPO=CFG["void_repository"], KEYS=str(xbps_dir() / "var/db/xbps/keys"),
               PKGCACHE=str(CACHE / "packages"), PACKAGES=" ".join(CFG["base_packages"]))
    with (WORK / "build-rootfs.log").open("w") as log:
        run("unshare", "-r", "sh", LAB / "host/build-rootfs.sh", env=env, stdout=log,
            stderr=subprocess.STDOUT)
    print(f"Void root installed at {ROOTFS} (log: {WORK / 'build-rootfs.log'})")


def build_crowbar():
    run("cargo", "build", "--release", "--locked", "--target", MUSL, "--bin", "crowbar",
        cwd=WORKSPACE)
    return CROWBAR_BIN


def image():
    if not ROOTFS.exists():
        rootfs()
    crowbar = build_crowbar()
    if not KEY.exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "crowbar-void-lab", "-f", KEY)
    env = dict(os.environ, ROOTFS=str(ROOTFS), WORK=str(WORK), OUT=str(BASE_DISK),
               CROWBAR=str(crowbar), STATE=str(LAB / "states/01-base.toml"),
               PUBKEY=str(KEY) + ".pub", ROOT_LABEL=CFG["root_label"],
               ESP_MIB=str(CFG["esp_mib"]), ROOT_GIB=str(CFG["root_gib"]))
    run("unshare", "-r", "sh", LAB / "host/build-image.sh", env=env)


def running():
    if not PIDFILE.exists():
        return False
    try:
        os.kill(int(PIDFILE.read_text().strip()), 0)
        return True
    except (ProcessLookupError, ValueError):
        return False


def start(fresh=True):
    if running():
        raise SystemExit("The lab VM is already running; `void_lab.py stop` first")
    if not BASE_DISK.exists():
        raise SystemExit("No base disk; run `void_lab.py image` first")
    if fresh or not DISK.exists():
        DISK.unlink(missing_ok=True)
        run("cp", "--reflink=auto", "--sparse=always", BASE_DISK, DISK)
        shutil.copyfile(CFG["firmware_vars"], NVRAM)
        (WORK / "known_hosts").unlink(missing_ok=True)
    for path in (SERIAL_SOCK, MONITOR_SOCK, PIDFILE):
        path.unlink(missing_ok=True)
    run("qemu-system-x86_64", "-name", "crowbar-void-lab",
        "-machine", "q35,accel=kvm", "-cpu", "host",
        "-m", str(CFG["memory_mib"]), "-smp", str(CFG["vcpus"]),
        "-drive", f"if=pflash,format=raw,readonly=on,file={CFG['firmware_code']}",
        "-drive", f"if=pflash,format=raw,file={NVRAM}",
        "-drive", f"file={DISK},format=raw,if=virtio,cache=unsafe",
        "-netdev", f"user,id=n0,hostfwd=tcp:127.0.0.1:{CFG['ssh_port']}-:22",
        "-device", "virtio-net-pci,netdev=n0",
        "-chardev", f"socket,id=ser0,path={SERIAL_SOCK},server=on,wait=off,"
                    f"logfile={SERIAL_LOG},logappend=on",
        "-serial", "chardev:ser0",
        "-monitor", f"unix:{MONITOR_SOCK},server=on,wait=off",
        "-display", "none", "-daemonize", "-pidfile", PIDFILE)


def monitor(command):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.connect(str(MONITOR_SOCK))
        sock.sendall(command.encode() + b"\n")
        time.sleep(0.5)


def stop():
    if not running():
        print("The lab VM is not running")
        return
    pid = int(PIDFILE.read_text().strip())
    monitor("quit")
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.2)
    PIDFILE.unlink(missing_ok=True)
    print("Stopped the lab VM")


def ssh_args():
    return ["ssh", "-i", str(KEY), "-p", str(CFG["ssh_port"]), "-o", "IdentitiesOnly=yes",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
            "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=3",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={WORK / 'known_hosts'}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "root@127.0.0.1"]


def ssh(command, check=True, timeout=600):
    result = subprocess.run(ssh_args() + [command], text=True, capture_output=True,
                            timeout=timeout)
    if check and result.returncode != 0:
        raise RuntimeError(f"guest command failed ({result.returncode}): {command}\n"
                           f"{result.stdout}{result.stderr}")
    return result


def ssh_up():
    try:
        return ssh("true", check=False, timeout=15).returncode == 0
    except subprocess.TimeoutExpired:
        return False


class Serial:
    """Connected to the guest's serial console for the whole run."""

    def __init__(self, transcript):
        self.text = ""
        self.mark = 0
        self.lock = threading.Lock()
        self.transcript = transcript
        self.sock = None
        self.connect()

    def connect(self):
        for _ in range(50):
            try:
                self.sock = socket.socket(socket.AF_UNIX)
                self.sock.connect(str(SERIAL_SOCK))
                break
            except OSError:
                time.sleep(0.2)
        else:
            raise RuntimeError("cannot connect to the serial console socket")
        threading.Thread(target=self.reader, daemon=True).start()

    def reader(self):
        sock = self.sock
        while True:
            try:
                data = sock.recv(4096)
            except OSError:
                return
            if not data:
                return
            with self.lock:
                self.text += ANSI.sub("", data.decode("utf-8", "replace"))

    def set_mark(self):
        with self.lock:
            self.mark = len(self.text)

    def wait_for(self, pattern, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                found = re.search(pattern, self.text[self.mark:])
            if found:
                return found
            time.sleep(0.2)
        return None

    def send(self, data):
        self.sock.sendall(data)

    def since_mark(self):
        with self.lock:
            return self.text[self.mark:]


class Evidence:
    def __init__(self):
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        self.path = WORK / f"milestone-{stamp}.log"
        self.stream = self.path.open("w")
        self.results = []

    def log(self, text):
        line = f"[{time.strftime('%H:%M:%S')}] {text}"
        print(line, flush=True)
        self.stream.write(line + "\n")
        self.stream.flush()

    def guest(self, command, check=True):
        self.log(f"guest$ {command}")
        result = ssh(command, check=False)
        for line in (result.stdout + result.stderr).rstrip().splitlines():
            self.log(f"    {line}")
        if check and result.returncode != 0:
            raise RuntimeError(f"guest command failed ({result.returncode}): {command}")
        return result

    def check(self, name, ok, detail=""):
        self.results.append((name, bool(ok)))
        self.log(f"{'PASS' if ok else 'FAIL'}: {name}{' - ' + detail if detail else ''}")
        return ok


def wait_ssh(ev, timeout=240):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if ssh_up():
            ev.log("ssh is up")
            return True
        time.sleep(3)
    ev.log(f"ssh did not come up within {timeout}s")
    return False


def booted_generation(ev):
    out = ev.guest("cat /proc/cmdline").stdout
    found = re.search(r"crowbar\.generation=(\d+)", out)
    return int(found.group(1)) if found else None


def reboot(ev, serial):
    serial.set_mark()
    # Detach so the ssh session ends before the guest goes down.
    ev.guest("nohup sh -c 'sleep 1; reboot' >/dev/null 2>&1 &", check=False)
    # Wait until the old system is gone before polling ssh again.
    serial.wait_for(r"BdsDxe|UEFI|GRUB|CrowbarOS generation", 120)


def milestone():
    ev = Evidence()
    ev.log(f"Evidence log: {ev.path}")
    image()
    start(fresh=True)
    serial = Serial(ev)
    try:
        ev.log("== Boot generation 1 (base image)")
        menu = serial.wait_for(r"CrowbarOS generation 1", 120)
        ev.check("GRUB menu lists generation 1", menu)
        ev.check("generation 1 boots and ssh comes up", wait_ssh(ev))
        ev.check("booted generation is 1", booted_generation(ev) == 1)
        ev.guest("findmnt -no SOURCE,FSTYPE,OPTIONS /")
        ev.guest("crowbar generations")
        drift = ev.guest("crowbar drift", check=False)
        ev.check("no drift on the base image", drift.returncode == 0)

        ev.log("== Edit the state: 02-tools.toml adds jq, htop, and /etc/motd")
        ev.guest("diff -u /etc/crowbar/state.toml /root/states/02-tools.toml", check=False)
        ev.guest("crowbar plan /root/states/02-tools.toml")
        ev.guest("CROWBAR_ACTOR=milestone-1 crowbar apply /root/states/02-tools.toml")
        ev.guest("crowbar generations")
        ev.check("running system untouched by apply", ev.guest(
            "command -v jq", check=False).returncode != 0)
        reboot(ev, serial)
        ev.check("generation 2 boots and ssh comes up", wait_ssh(ev))
        ev.check("booted generation is 2", booted_generation(ev) == 2)
        ev.check("jq is installed in generation 2",
                 ev.guest("jq --version", check=False).returncode == 0)
        ev.check("/etc/motd is managed",
                 "crowbar apply" in ev.guest("cat /etc/motd").stdout)
        ev.check("no drift on generation 2",
                 ev.guest("crowbar drift", check=False).returncode == 0)

        ev.log("== Apply a deliberately broken change: 03-broken-mount.toml")
        ev.guest("diff -u /etc/crowbar/state.toml /root/states/03-broken-mount.toml", check=False)
        plan = ev.guest("crowbar plan /root/states/03-broken-mount.toml")
        ev.check("plan flags the broken mount as boot-affecting",
                 re.search(r"^\s*! managed mounts: 0 -> 1", plan.stdout, re.M))
        ev.guest("CROWBAR_ACTOR=milestone-1 crowbar apply /root/states/03-broken-mount.toml")
        ev.guest("crowbar generations")
        reboot(ev, serial)
        broken = serial.wait_for(r"Cannot continue due to errors above", 240)
        ev.check("generation 3 fails to boot (emergency shell)", broken)
        tail = serial.since_mark()[-1500:]
        for line in tail.splitlines()[-25:]:
            ev.log(f"    serial| {line}")
        ev.check("ssh is down while generation 3 is stuck", not ssh_up())

        ev.log("== Recover from the boot menu: reset, pick generation 2")
        serial.set_mark()
        monitor("system_reset")
        menu = serial.wait_for(r"CrowbarOS generation 2", 120)
        ev.check("GRUB menu shows generation 2 after reset", menu)
        time.sleep(1)
        serial.send(b"\x1b[B")  # Down: entries are newest first, so this is gen 2.
        time.sleep(0.5)
        serial.send(b"\r")
        ev.log("sent Down, Enter to the GRUB menu over the serial console")
        ev.check("generation 2 boots from the menu and ssh comes up", wait_ssh(ev))
        ev.check("booted generation is 2 (picked in the menu)", booted_generation(ev) == 2)
        ev.check("broken mount absent in generation 2",
                 "crowbar-data" not in ev.guest("cat /etc/fstab").stdout)
        ev.guest("crowbar generations")

        ev.log("== Make the rollback stick")
        ev.guest("CROWBAR_ACTOR=milestone-1 crowbar rollback")
        ev.guest("crowbar generations")
        reboot(ev, serial)
        ev.check("after rollback, the default boot comes up", wait_ssh(ev))
        ev.check("default boot is generation 2", booted_generation(ev) == 2)

        ev.log("== Drift: a hand edit outside the engine is detected")
        ev.guest("echo 'edited by hand' >> /etc/motd")
        drift = ev.guest("crowbar drift", check=False)
        ev.check("drift reports the hand-edited file", drift.returncode == 2
                 and "/etc/motd" in drift.stdout)

        ev.log("== A failing apply leaves nothing behind")
        ev.guest("sed 's/\"jq\", \"htop\"/\"jq\", \"htop\", \"crowbar-no-such-package\"/' "
                 "/root/states/02-tools.toml > /root/states/04-bad-package.toml")
        bad = ev.guest("crowbar apply /root/states/04-bad-package.toml", check=False)
        ev.check("apply with a missing package fails", bad.returncode != 0)
        after = ev.guest("crowbar generations; ls /run/crowbar/top/gens").stdout
        ev.check("failed apply created no generation and kept default 2",
                 "default" in next(l for l in after.splitlines() if l.strip().startswith("2 "))
                 and not re.search(r"^\s*4\s", after, re.M))
        ev.guest("crowbar ledger")
        ev.guest("ls -la /run/crowbar/top/gens; btrfs subvolume list /run/crowbar/top")
    except Exception as exc:  # Record the failure as evidence, then stop the VM.
        ev.check("milestone script completed", False, repr(exc))
    finally:
        stop()
    failed = [name for name, ok in ev.results if not ok]
    ev.log(f"== {len(ev.results) - len(failed)}/{len(ev.results)} checks passed")
    for name in failed:
        ev.log(f"   failed: {name}")
    ev.log(f"Evidence: {ev.path}; raw serial console: {SERIAL_LOG}")
    return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("fetch")
    sub.add_parser("rootfs").add_argument("--force", action="store_true")
    sub.add_parser("image")
    sub.add_parser("start").add_argument("--keep", action="store_true",
                                         help="boot the existing disk instead of a fresh copy")
    sub.add_parser("ssh").add_argument("cmd", nargs=argparse.REMAINDER)
    sub.add_parser("stop")
    sub.add_parser("milestone")
    args = parser.parse_args()
    if args.command == "fetch":
        fetch()
    elif args.command == "rootfs":
        rootfs(args.force)
    elif args.command == "image":
        image()
    elif args.command == "start":
        start(fresh=not args.keep)
    elif args.command == "ssh":
        os.execvp("ssh", ssh_args() + args.cmd)
    elif args.command == "stop":
        stop()
    elif args.command == "milestone":
        sys.exit(milestone())


if __name__ == "__main__":
    main()
