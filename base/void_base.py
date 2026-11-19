#!/usr/bin/env python3
"""The CrowbarOS base: plain Void Linux and the desktop, built rootless and booted under UEFI.

Stdlib only, no sudo. Root-owned work happens inside `unshare -r` user
namespaces. Downloads are shared with the earlier lab in ~/.cache/crowbaros/void;
the roots, the build, the disk, VM runtime files, and boot logs live in
<repo>/.lab/base.

The desktop (Fulcrum, the shell, and Studio Settings) is built and tested in a
Void build root of its own, so it links Void's own libraries, and goes into the
image with its runit services from desktop/.

    void_base.py build [--force]   install the Void roots, build the desktop, and write the boot disk
    void_base.py boot [--window]   boot a fresh copy of the disk and wait for it
    void_base.py ssh [CMD]         run a command in the guest as root
    void_base.py console           attach to the guest's serial console
    void_base.py stop              power the guest off
    void_base.py check             build, boot, check, record, and power off
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tarfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
CFG = json.loads((HERE / "config.json").read_text())
CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "crowbaros/void"
WORK = ROOT / ".lab/base"
ROOTFS = WORK / "rootfs"
BUILD_ROOT = WORK / "buildroot"
TARGET = WORK / "target"
BASE_DISK = WORK / "base.raw"
DISK = WORK / "disk.raw"
NVRAM = WORK / "nvram.fd"
KEY = WORK / "id_ed25519"
SERIAL_SOCK = WORK / "serial.sock"
SERIAL_LOG = WORK / "serial.log"
MONITOR_SOCK = WORK / "monitor.sock"
PIDFILE = WORK / "qemu.pid"
# What the desktop is built from: each Cargo project, and the part of the
# target directory it builds in. The two apps share theirs.
PROJECTS = [("compositor", "compositor"), ("apps/shell", ""), ("apps/studio-settings", "")]
# The programs the image holds, and the project each comes from. The probe is
# Fulcrum's test client.
PROGRAMS = {"crowbar-compositor": "compositor", "crowbar-probe": "compositor",
            "crowbaros-shell": "apps/shell", "crowbaros-studio-settings": "apps/studio-settings"}


def run(*args, **kwargs):
    print("+", shlex.join(str(a) for a in args), flush=True)
    return subprocess.run([str(a) for a in args], check=True, text=True, **kwargs)


def run_shown(*args, log, env):
    """Run a command, showing its output as it comes and keeping a copy in `log`."""
    print("+", shlex.join(str(a) for a in args), flush=True)
    with log.open("wb") as out:
        proc = subprocess.Popen([str(a) for a in args], env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT)
        while chunk := proc.stdout.read1(65536):
            out.write(chunk)
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
        return proc.wait()


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


# What the host needs, as (command or file, the Arch package that provides it).
NEEDS = [
    ("unshare", "util-linux"), ("newuidmap", "shadow"), ("curl", "curl"),
    ("mkfs.btrfs", "btrfs-progs"), ("mkfs.vfat", "dosfstools"), ("sfdisk", "util-linux"),
    ("qemu-system-x86_64", "qemu-base or qemu-desktop"), ("ssh", "openssh"),
    ("ssh-keygen", "openssh"),
]
# What building the desktop needs besides: Cargo fetches its crates on the
# host, and GPUI's preparation patches its source there.
BUILD_NEEDS = [("cargo", "rustup or rust"), ("patch", "patch")]


def preflight(vm=False, build=False):
    needs = NEEDS + (BUILD_NEEDS if build else [])
    missing = [f"{tool} (package {pkg})" for tool, pkg in needs if not shutil.which(tool)]
    if vm:
        missing += [f"{path} (package edk2-ovmf)"
                    for path in (CFG["firmware_code"], CFG["firmware_vars"])
                    if not Path(path).exists()]
        if not os.access("/dev/kvm", os.R_OK | os.W_OK):
            missing.append("read and write access to /dev/kvm (is KVM on, and are you in the kvm group?)")
        if shutil.which("qemu-system-x86_64") and '"virtio-gpu-pci"' not in subprocess.run(
                ["qemu-system-x86_64", "-device", "help"], text=True, capture_output=True).stdout:
            missing.append("QEMU's virtio-gpu-pci device (package qemu-hw-display-virtio-gpu-pci, "
                           "or set QEMU_MODULE_DIR to a directory that holds it)")
    if missing:
        raise SystemExit("This host is missing:\n  " + "\n  ".join(missing))


def subordinate(path):
    """The first range of IDs /etc/subuid or /etc/subgid gives the current user."""
    names = {str(os.getuid()), os.environ.get("USER", "")}
    try:
        for line in Path(path).read_text().splitlines():
            name, start, count = line.strip().split(":")
            if name in names:
                return int(start), int(count)
    except (FileNotFoundError, ValueError):
        pass
    return None


def userns():
    """`unshare` as fake root, with the user's subordinate IDs mapped from 1 up,
    so packages can give files to their own users and groups (tty, _dhcpcd)."""
    uids, gids = subordinate("/etc/subuid"), subordinate("/etc/subgid")
    if not (uids and gids):
        print("warning: no subordinate IDs in /etc/subuid and /etc/subgid; files that "
              "belong to a group other than root will belong to root", file=sys.stderr)
        return ["unshare", "-r"]
    return ["unshare", "--map-root-user", f"--map-users=1:{uids[0]}:{uids[1]}",
            f"--map-groups=1:{gids[0]}:{gids[1]}"]


def fetch():
    """Download the static xbps once, and refuse it unless it matches the pin."""
    CACHE.mkdir(parents=True, exist_ok=True)
    url = CFG["xbps_static_url"]
    tarball = CACHE / url.rsplit("/", 1)[1]
    if not tarball.exists():
        part = tarball.with_name(tarball.name + ".part")
        run("curl", "-fL", "--retry", "3", "-o", part, url)
        part.replace(tarball)
    if sha256(tarball) != CFG["xbps_static_sha256"]:
        raise SystemExit(f"{tarball} does not match the pinned SHA-256; refusing to use it")
    target = CACHE / "xbps-static"
    if not (target / "usr/bin/xbps-install.static").exists():
        shutil.rmtree(target, ignore_errors=True)
        target.mkdir()
        with tarfile.open(tarball) as archive:
            archive.extractall(target, filter="data")
    return target


def install(target, packages, what, force, missing=lambda: None):
    """Install a Void root of these packages at `target`.

    A root counts as installed only once an install of these packages from
    this repository finished, and `missing` names nothing the root lacks. A
    root left by a failed or interrupted install, or one built from other
    packages, is installed again."""
    wanted = json.dumps({"repository": CFG["void_repository"], "packages": packages})
    done = target.with_name(target.name + ".done")
    if not force and target.exists() and done.exists() and done.read_text() == wanted:
        print(f"Using the {what} at {target} (pass --force to install it again)")
        return
    if target.exists() and not force:
        print(f"The {what} at {target} is not a finished install of these packages; "
              "installing it again")
    xbps = fetch()
    WORK.mkdir(parents=True, exist_ok=True)
    done.unlink(missing_ok=True)
    env = dict(os.environ, XBPS=str(xbps / "usr/bin"), ROOTFS=str(target),
               REPO=CFG["void_repository"], KEYS=str(xbps / "var/db/xbps/keys"),
               PKGCACHE=str(CACHE / "packages"), PACKAGES=" ".join(packages))
    log = target.with_name(target.name + ".log")
    print(f"Installing the {what}: {len(packages)} packages and what they depend on. The first "
          f"time, this downloads them into {CACHE / 'packages'}.", flush=True)
    if run_shown(*userns(), "sh", HERE / "host/rootfs.sh", log=log, env=env) != 0:
        raise SystemExit(f"Installing the {what} failed; the output is in {log}")
    if lack := missing():
        raise SystemExit(f"The {what} has {lack} after the install; see {log}")
    done.write_text(wanted)
    print(f"Installed the {what} at {target} (log: {log})")


def rootfs(force):
    install(ROOTFS, CFG["packages"], "Void root", force,
            missing=lambda: None if any((ROOTFS / "boot").glob("vmlinuz-*")) else "no kernel in /boot")


def binaries():
    """The desktop's programs, and where the build leaves each."""
    parts = dict(PROJECTS)
    return {name: TARGET / parts[project] / "release" / name for name, project in PROGRAMS.items()}


def compile_desktop(force):
    """Build and test the desktop in the Void build root, against Void's libraries.

    The host's Cargo registry and git checkouts are bound in, so the build runs
    offline from the crates the lockfiles name. The build root and the target
    directory are kept, so the next build starts from them."""
    install(BUILD_ROOT, CFG["build_packages"], "Void build root", force)
    run(sys.executable, ROOT / "apps/shell/packaging/prepare-gpui.py")
    for project, _ in PROJECTS:
        run("cargo", "fetch", "--locked", "--target", "x86_64-unknown-linux-gnu",
            "--manifest-path", ROOT / project / "Cargo.toml")
    TARGET.mkdir(parents=True, exist_ok=True)
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    env = dict(os.environ, B=str(BUILD_ROOT), SRC=str(ROOT), TARGET=str(TARGET),
               PROJECTS=" ".join(f"{project}:{part}" for project, part in PROJECTS),
               CARGO_REGISTRY=str(cargo_home / "registry"), CARGO_GIT=str(cargo_home / "git"))
    run(*userns(), "--mount", "sh", HERE / "host/build.sh", env=env)
    for name, path in binaries().items():
        print(f"{name}: sha256 {sha256(path)}")


def build(force=False):
    preflight(build=True)
    rootfs(force)
    compile_desktop(force)
    if not KEY.exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "crowbaros-base", "-f", KEY)
    env = dict(os.environ, ROOTFS=str(ROOTFS), WORK=str(WORK), OUT=str(BASE_DISK),
               PUBKEY=str(KEY) + ".pub", HOSTNAME=CFG["hostname"],
               ROOT_LABEL=CFG["root_label"], ESP_LABEL=CFG["esp_label"],
               ESP_MIB=str(CFG["esp_mib"]), ROOT_GIB=str(CFG["root_gib"]),
               DESKTOP=str(HERE / "desktop"), REPO_ROOT=str(ROOT),
               BINARIES=" ".join(str(path) for path in binaries().values()))
    run(*userns(), "sh", HERE / "host/image.sh", env=env)


def running():
    try:
        os.kill(int(PIDFILE.read_text().strip()), 0)
        return True
    except (FileNotFoundError, ProcessLookupError, ValueError):
        return False


def qemu(name, disk, nvram, ssh_port):
    """The VM an image boots in: UEFI firmware with its own variables, KVM, a
    virtio disk and network with SSH forwarded, and for the desktop a virtio
    GPU with two connectors (the second starts unplugged), a keyboard, and a
    tablet. There is no VGA card: it would be a second DRM device, and the
    primary one. Add the serial port, a control socket, and a display."""
    return ["qemu-system-x86_64", "-name", name,
            "-machine", "q35,accel=kvm", "-cpu", "host",
            "-m", str(CFG["memory_mib"]), "-smp", str(CFG["vcpus"]),
            "-drive", f"if=pflash,format=raw,readonly=on,file={CFG['firmware_code']}",
            "-drive", f"if=pflash,format=raw,file={nvram}",
            "-drive", f"file={disk},format=raw,if=virtio,cache=unsafe",
            "-netdev", f"user,id=n0,hostfwd=tcp:127.0.0.1:{ssh_port}-:22",
            "-device", "virtio-net-pci,netdev=n0",
            "-vga", "none", "-device", "virtio-gpu-pci,id=gpu0,max_outputs=2,xres=1280,yres=960",
            "-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci"]


def launch(window):
    """Start QEMU on a fresh copy of the base disk, with UEFI firmware."""
    preflight(vm=True)
    if running():
        raise SystemExit("The guest is already running; `void_base.py stop` first")
    if not BASE_DISK.exists():
        raise SystemExit("No base disk; run `void_base.py build` first")
    DISK.unlink(missing_ok=True)
    run("cp", "--reflink=auto", "--sparse=always", BASE_DISK, DISK)
    shutil.copyfile(CFG["firmware_vars"], NVRAM)
    for path in (SERIAL_SOCK, SERIAL_LOG, MONITOR_SOCK, PIDFILE, WORK / "known_hosts"):
        path.unlink(missing_ok=True)
    display = ["-display", "gtk"] if window else ["-display", "none"]
    run(*qemu("crowbaros-base", DISK, NVRAM, CFG["ssh_port"]),
        "-chardev", f"socket,id=ser0,path={SERIAL_SOCK},server=on,wait=off,logfile={SERIAL_LOG}",
        "-serial", "chardev:ser0",
        "-monitor", f"unix:{MONITOR_SOCK},server=on,wait=off",
        *display, "-daemonize", "-pidfile", PIDFILE)
    return time.monotonic()


def serial_text():
    try:
        return SERIAL_LOG.read_text(errors="replace")
    except FileNotFoundError:
        return ""


def wait_for(predicate, timeout, what):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        if not running():
            raise SystemExit(f"QEMU exited while waiting for {what}; see {SERIAL_LOG}")
        time.sleep(0.5)
    raise SystemExit(f"Timed out after {timeout}s waiting for {what}; see {SERIAL_LOG}")


def ssh_args():
    return ["ssh", "-i", str(KEY), "-p", str(CFG["ssh_port"]), "-o", "IdentitiesOnly=yes",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={WORK / 'known_hosts'}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "root@127.0.0.1"]


def ssh(command, timeout=60):
    return subprocess.run(ssh_args() + [command], text=True, capture_output=True, timeout=timeout)


def ssh_up():
    try:
        return ssh("true", timeout=15).returncode == 0
    except subprocess.TimeoutExpired:
        return False


def boot(window=False):
    started = launch(window)
    wait_for(lambda: "login:" in serial_text(), 180, "the login prompt on the serial console")
    to_login = time.monotonic() - started
    wait_for(ssh_up, 60, "SSH")
    print(f"Booted to a login prompt in {to_login:.1f}s, and SSH answers.")
    print(f"  {sys.argv[0]} ssh        a root shell")
    print(f"  {sys.argv[0]} console    the serial console (Ctrl-] to leave)")
    print(f"  {sys.argv[0]} stop       power off")
    return to_login


def monitor(command):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.connect(str(MONITOR_SOCK))
        sock.sendall(command.encode() + b"\n")
        time.sleep(0.5)


def stop():
    if not running():
        print("The guest is not running")
        return
    pid = int(PIDFILE.read_text().strip())
    if ssh_up():
        ssh("poweroff", timeout=15)
    for _ in range(100):
        if not running():
            break
        time.sleep(0.2)
    else:
        monitor("quit")
    PIDFILE.unlink(missing_ok=True)
    print(f"Powered off the guest (pid {pid})")


# What a booted base must show, as (name, guest command, test on its output).
CHECKS = [
    ("Void Linux", "cat /etc/os-release",
     lambda out: 'ID="void"' in out or "ID=void" in out),
    ("kernel matches the boot menu", "uname -r; grep -o 'kernel [^)]*' /boot/grub/grub.cfg",
     lambda out: out.split()[0] in out.split("kernel", 1)[1]),
    ("root is the labelled Btrfs", "findmnt -no SOURCE,FSTYPE,OPTIONS /",
     lambda out: "btrfs" in out and "rw" in out),
    ("booted with UEFI", "test -d /sys/firmware/efi && echo efi", lambda out: "efi" in out),
    ("hostname", "hostname", lambda out: out.strip() == CFG["hostname"]),
    ("runit is PID 1", "cat /proc/1/comm", lambda out: out.strip() == "runit"),
    ("services are up", "sv status /var/service/*",
     lambda out: all(f"run: /var/service/{s}:" in out for s in ("sshd", "dhcpcd", "agetty-ttyS0"))),
    ("network has an address", "ip -4 -o addr show scope global",
     lambda out: "inet " in out),
    ("packages keep their groups", "stat -c '%G %n' /usr/bin/wall /var/db/dhcpcd",
     lambda out: "tty /usr/bin/wall" in out and "_dhcpcd /var/db/dhcpcd" in out),
    ("no failed kernel messages", "dmesg --level=emerg,alert,crit",
     lambda out: out.strip() == ""),
]


def check():
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True,
                          capture_output=True).stdout.strip()
    build(force=False)
    to_login = boot()
    lines = [f"CrowbarOS base boot check, {stamp}, commit {head}",
             f"disk sha256 {sha256(BASE_DISK)}", f"login prompt after {to_login:.1f}s", ""]
    failed = 0
    for name, command, ok in CHECKS:
        result = ssh(command)
        passed = result.returncode == 0 and ok(result.stdout)
        failed += not passed
        lines.append(f"{'PASS' if passed else 'FAIL'}: {name}")
        lines += [f"    $ {command}"] + [f"    {l}" for l in result.stdout.strip().splitlines()]
        if result.stderr.strip():
            lines += [f"    ! {l}" for l in result.stderr.strip().splitlines()]
    stop()
    lines += ["", f"{len(CHECKS) - failed} of {len(CHECKS)} checks passed", "",
              "--- serial console ---", serial_text()]
    log = WORK / f"boot-{stamp}.log"
    log.write_text("\n".join(lines))
    print("\n".join(l for l in lines[:lines.index('--- serial console ---')]))
    print(f"Log: {log}")
    if failed:
        raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("build").add_argument("--force", action="store_true",
                                         help="install the Void roots again")
    sub.add_parser("boot").add_argument("--window", action="store_true",
                                        help="also show the guest's screen in a window")
    sub.add_parser("ssh").add_argument("command", nargs=argparse.REMAINDER)
    sub.add_parser("console")
    sub.add_parser("stop")
    sub.add_parser("check")
    args = parser.parse_args()
    if args.cmd == "build":
        build(args.force)
    elif args.cmd == "boot":
        boot(args.window)
    elif args.cmd == "ssh":
        os.execvp("ssh", ssh_args() + ([" ".join(args.command)] if args.command else []))
    elif args.cmd == "console":
        if not shutil.which("socat"):
            raise SystemExit("The console needs socat (package socat)")
        os.execvp("socat", ["socat", "-,raw,echo=0,escape=0x1d", f"UNIX-CONNECT:{SERIAL_SOCK}"])
    elif args.cmd == "stop":
        stop()
    elif args.cmd == "check":
        check()


if __name__ == "__main__":
    main()
