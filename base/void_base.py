#!/usr/bin/env python3
"""The CrowbarOS base: plain Void Linux and the desktop, built rootless and booted under UEFI.

Stdlib only, no sudo. Root-owned work happens inside `unshare -r` user
namespaces. Downloads are shared with the earlier lab in ~/.cache/crowbaros/void;
the roots, the build, the disk, VM runtime files, and boot logs live in
<repo>/.lab/base.

The desktop (Fulcrum, the shell, and Studio Settings) is built and tested in a
Void build root of its own, so it links Void's own libraries, and goes into the
image with its runit services from desktop/.

    void_base.py build [--force] [--lab-account]
                                   install the Void roots, build the desktop, and write the boot disk
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
import re
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
# The person's account: its name and password hash, kept outside git. See
# README.md, "The person's account".
ACCOUNT_FILE = HERE / "account.json"
REPOSITORY = CFG["void_repository"]
# Void's nonfree repository, which holds NVIDIA's driver. Only the image's root
# installs from it.
NONFREE = REPOSITORY + "/nonfree"
# What the desktop is built from: each Cargo project, and the part of the
# target directory it builds in. The two apps share theirs.
PROJECTS = [("compositor", "compositor"), ("apps/shell", ""), ("apps/studio-settings", ""),
            ("base/helpers", "helpers")]
# The programs the image holds, and the project each comes from. The probe is
# Fulcrum's test client. The helpers are the desktop's four helpers in one
# program; see helpers/README.md.
PROGRAMS = {"crowbar-compositor": "compositor", "crowbar-probe": "compositor",
            "crowbaros-shell": "apps/shell", "crowbaros-studio-settings": "apps/studio-settings",
            "crowbar-helpers": "base/helpers"}


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


def install(target, packages, what, force, missing=lambda: None, repositories=(REPOSITORY,)):
    """Install a Void root of these packages, from these repositories, at `target`.

    A root counts as installed only once an install of these packages from
    these repositories finished, and `missing` names nothing the root lacks. A
    root left by a failed or interrupted install, or one built from other
    packages, is installed again."""
    wanted = json.dumps({"repositories": list(repositories), "packages": packages})
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
               REPOS=" ".join(repositories), KEYS=str(xbps / "var/db/xbps/keys"),
               PKGCACHE=str(CACHE / "packages"), PACKAGES=" ".join(packages))
    log = target.with_name(target.name + ".log")
    print(f"Installing the {what}: {len(packages)} packages and what they depend on. The first "
          f"time, this downloads them into {CACHE / 'packages'}.", flush=True)
    # In a mount namespace of its own, so the root can borrow the host's /dev.
    if run_shown(*userns(), "--mount", "sh", HERE / "host/rootfs.sh", log=log, env=env) != 0:
        raise SystemExit(f"Installing the {what} failed; the output is in {log}")
    if lack := missing():
        raise SystemExit(f"The {what} has {lack} after the install; see {log}")
    done.write_text(wanted)
    print(f"Installed the {what} at {target} (log: {log})")


# NVIDIA's kernel modules, which DKMS builds for the image's kernel while the
# root is installed. The compositor needs the first three.
NVIDIA_MODULES = ("nvidia", "nvidia-modeset", "nvidia-drm", "nvidia-uvm")


def image_kernel(root):
    """The version of the kernel the image boots: the newest in /boot. The
    build checks it and hands it to host/image.sh. None if there is none."""
    def version(path):
        return [(0, int(part), "") if part.isdigit() else (1, 0, part)
                for part in re.split(r"(\d+)", path.name.removeprefix("vmlinuz-")) if part]
    kernels = sorted((root / "boot").glob("vmlinuz-*"), key=version)
    return kernels[-1].name.removeprefix("vmlinuz-") if kernels else None


# The CPU microcode the initramfs loads before the kernel starts, one file for
# each vendor, from `intel-ucode` and `linux-firmware-amd`.
MICROCODE = ("kernel/x86/microcode/GenuineIntel.bin", "kernel/x86/microcode/AuthenticAMD.bin")


def early_cpio(image):
    """The names in the uncompressed cpio archive at the start of an
    initramfs, where dracut puts the microcode the kernel loads first. Empty
    when the initramfs has no such archive."""
    names, offset = [], 0
    while image[offset:offset + 6] == b"070701":
        header = image[offset:offset + 110]
        filesize, namesize = int(header[54:62], 16), int(header[94:102], 16)
        name = image[offset + 110:offset + 110 + namesize - 1].decode(errors="replace")
        if name == "TRAILER!!!":
            break
        names.append(name)
        data = (offset + 110 + namesize + 3) & ~3
        offset = (data + filesize + 3) & ~3
    return names


def missing_from_root(root):
    """What the root lacks to boot the image, or None: the kernel, NVIDIA's
    modules for it, and both vendors' microcode in its initramfs. xbps reports
    success when DKMS fails to build the modules, and when the initramfs was
    made before the microcode was installed, so the root itself is the only
    witness."""
    kernel = image_kernel(root)
    if kernel is None:
        return "no kernel in /boot"
    try:
        with (root / "boot" / f"initramfs-{kernel}.img").open("rb") as initramfs:
            early = early_cpio(initramfs.read(64 << 20))
    except FileNotFoundError:
        return f"no initramfs for kernel {kernel}"
    if lack := [path.rsplit("/", 1)[1] for path in MICROCODE if path not in early]:
        return (f"no {', '.join(lack)} microcode in the initramfs for kernel {kernel}: it was made "
                "before the microcode was installed")
    try:
        listed = (root / "usr/lib/modules" / kernel / "modules.dep").read_text()
    except FileNotFoundError:
        listed = ""
    names = {line.split(":", 1)[0].rsplit("/", 1)[-1].split(".ko", 1)[0]
             for line in listed.splitlines()}
    if lack := [name for name in NVIDIA_MODULES if name not in names]:
        return (f"no {', '.join(lack)} module for kernel {kernel}: NVIDIA's DKMS build did not "
                f"finish; see its make.log under {root}/var/lib/dkms/nvidia")
    return None


def rootfs(force):
    install(ROOTFS, CFG["packages"], "Void root", force, missing=lambda: missing_from_root(ROOTFS),
            repositories=(REPOSITORY, NONFREE))


def binaries():
    """The desktop's programs, and where the build leaves each."""
    parts = dict(PROJECTS)
    return {name: TARGET / parts[project] / "release" / name for name, project in PROGRAMS.items()}


def compile_desktop(force):
    """Build and test the desktop in the Void build root, against Void's libraries.

    The host's Cargo registry and git checkouts are bound in, so the build runs
    offline from the crates the lockfiles name. The build root and the target
    directory are kept, so the next build starts from them."""
    install(BUILD_ROOT, CFG["build_packages"], "Void build root", force,
            missing=lambda: None if (BUILD_ROOT / "usr/bin/cargo").exists() else "no cargo")
    run(sys.executable, ROOT / "apps/shell/packaging/prepare-gpui.py")
    for project, _ in PROJECTS:
        run("cargo", "fetch", "--locked", "--target", "x86_64-unknown-linux-gnu",
            "--manifest-path", ROOT / project / "Cargo.toml")
    TARGET.mkdir(parents=True, exist_ok=True)
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    # The build binds both in. Cargo makes `git` only for a git dependency.
    for cache in ("registry", "git"):
        (cargo_home / cache).mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, B=str(BUILD_ROOT), SRC=str(ROOT), TARGET=str(TARGET),
               PROJECTS=" ".join(f"{project}:{part}" for project, part in PROJECTS),
               CARGO_REGISTRY=str(cargo_home / "registry"), CARGO_GIT=str(cargo_home / "git"))
    run(*userns(), "--mount", "sh", HERE / "host/build.sh", env=env)
    for name, path in binaries().items():
        print(f"{name}: sha256 {sha256(path)}")


# The account an image gets with no account file. Its password is in this
# repository, so the image is for the lab alone.
LAB_ACCOUNT = {"name": "crowbar", "full_name": "", "password_hash": "", "password": "crowbar",
               "ssh_keys": []}
# The person's account has this UID whatever its name: the unlock helper, and
# both checks, find it by UID.
PERSON_UID = 1003
ACCOUNT_NAME = re.compile(r"[a-z_][a-z0-9_-]{0,31}")
# A crypt(5) hash: `$id$`, its settings and salt, each ended by `$`, then the
# hash itself, with nothing /etc/shadow would split on.
PASSWORD_HASH = re.compile(r"\$[0-9a-z]+\$(?:[^$:\s]+\$)+[./0-9A-Za-z]{20,}")
# One OpenSSH public key, as a `.pub` file holds it: its type, the key in
# base64, and an optional comment, on one line. No options before the type, so
# a key file cannot give a key a forced command or anything else.
SSH_PUBLIC_KEY = re.compile(r"(?:ssh-(?:ed25519|rsa)|ecdsa-sha2-nistp(?:256|384|521)"
                            r"|sk-(?:ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com)"
                            r" [A-Za-z0-9+/]+={0,2}(?: [^\n\r]*)?")


def warn_lab_account(why):
    bar = "!" * 78
    print(f"\n{bar}\nWARNING: {why}, so this image has the lab account `{LAB_ACCOUNT['name']}`.\n"
          f"Its password, `{LAB_ACCOUNT['password']}`, is published in this repository, so anyone "
          "at the\nmachine can unlock its desktop. Never put it on a machine anyone else can "
          "reach.\nTo set your own account, see base/README.md, \"The person's account\".\n"
          + bar + "\n", file=sys.stderr, flush=True)


def person_account(lab=False):
    """The person's account, as `image.sh` takes it: its name, full name, and
    either the password hash from the account file or the lab's password, and
    the SSH public keys the file lists. The build's own key is added in
    `image.sh`, for this account as for root.

    With `lab`, or with no account file, it is the lab account, which always
    comes with a loud warning: anyone who reads this repository knows its
    password, and the checks write the same image file a person would."""
    if lab:
        warn_lab_account("--lab-account ignores any account file, as the checks do")
        return dict(LAB_ACCOUNT)
    if not ACCOUNT_FILE.exists():
        warn_lab_account(f"there is no account file at {ACCOUNT_FILE}")
        return dict(LAB_ACCOUNT)
    try:
        account = json.loads(ACCOUNT_FILE.read_text())
    except ValueError as error:
        raise SystemExit(f"{ACCOUNT_FILE} is not JSON: {error}")
    if not isinstance(account, dict) or not {"name", "password_hash"} <= set(account) \
            or not set(account) <= {"name", "password_hash", "full_name", "ssh_keys"}:
        raise SystemExit(f"{ACCOUNT_FILE} must hold `name` and `password_hash`, and may hold "
                         "`full_name` and `ssh_keys`, and nothing else")
    name, hashed, full = account["name"], account["password_hash"], account.get("full_name", "")
    if not (isinstance(name, str) and ACCOUNT_NAME.fullmatch(name)) or name.startswith("crowbar-"):
        raise SystemExit(f"{ACCOUNT_FILE}: `name` must be a lowercase account name of up to 32 "
                         "characters, and not a crowbar- service account")
    if not (isinstance(hashed, str) and PASSWORD_HASH.fullmatch(hashed)):
        raise SystemExit(f"{ACCOUNT_FILE}: `password_hash` must be a whole crypt hash, as "
                         "`openssl passwd -6` prints it")
    if not isinstance(full, str) or any(c in full for c in ":,\n"):
        raise SystemExit(f"{ACCOUNT_FILE}: `full_name` must be text with no colon, comma, or "
                         "line break")
    keys = account.get("ssh_keys", [])
    if not (isinstance(keys, list) and all(isinstance(key, str) and SSH_PUBLIC_KEY.fullmatch(key)
                                           for key in keys)):
        raise SystemExit(f"{ACCOUNT_FILE}: `ssh_keys` must be a list of SSH public keys, each "
                         "one line as its .pub file holds it")
    return {"name": name, "full_name": full, "password_hash": hashed, "password": "",
            "ssh_keys": keys}


def names_in_root(root):
    """Every user and group name the root already has, which the person's
    account must not take."""
    names = set()
    for table in ("etc/passwd", "etc/group"):
        try:
            names |= {line.split(":", 1)[0] for line in (root / table).read_text().splitlines()}
        except FileNotFoundError:
            pass
    return names


def build(force=False, lab_account=False):
    preflight(build=True)
    account = person_account(lab_account)
    rootfs(force)
    # `netdev` is the group the image makes for Wi-Fi.
    if account["name"] in names_in_root(ROOTFS) | {"netdev"}:
        raise SystemExit(f"{ACCOUNT_FILE}: the image already has a user or group named "
                         f"`{account['name']}`; choose another name")
    if not account["password"]:
        print(f"Building with the account `{account['name']}` from {ACCOUNT_FILE}")
    compile_desktop(force)
    # Checked on every build, not only after an install: the image is never
    # written from a root that cannot drive NVIDIA's GPUs.
    if lack := missing_from_root(ROOTFS):
        raise SystemExit(f"The Void root at {ROOTFS} has {lack}. Install it again with "
                         "`base/void_base.py build --force`")
    # What only the lab's image gets, installed from the root's own package
    # index, so the same versions the root was installed with.
    lab_packages = CFG["lab_packages"] if lab_account else []
    xbps = fetch() if lab_packages else None
    if not KEY.exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "crowbaros-base", "-f", KEY)
    env = dict(os.environ, ROOTFS=str(ROOTFS), WORK=str(WORK), OUT=str(BASE_DISK),
               PUBKEY=str(KEY) + ".pub", HOSTNAME=CFG["hostname"],
               ROOT_LABEL=CFG["root_label"], ESP_LABEL=CFG["esp_label"],
               ESP_MIB=str(CFG["esp_mib"]), ROOT_GIB=str(CFG["root_gib"]),
               KVER=image_kernel(ROOTFS), KERNEL_ARGS=" ".join(CFG["kernel_args"]),
               DESKTOP=str(HERE / "desktop"), REPO_ROOT=str(ROOT), PERSON_UID=str(PERSON_UID),
               PERSON=account["name"], PERSON_FULL_NAME=account["full_name"],
               PERSON_PASSWORD_HASH=account["password_hash"], PERSON_PASSWORD=account["password"],
               PERSON_SSH_KEYS="".join(key + "\n" for key in account["ssh_keys"]),
               LAB_PACKAGES=" ".join(lab_packages), XBPS=str(xbps / "usr/bin") if xbps else "",
               REPOS=REPOSITORY, PKGCACHE=str(CACHE / "packages"))
    run(*userns(), "sh", HERE / "host/image.sh", *binaries().values(), env=env)
    lab = " (the lab account; see the warning above)" if account["password"] else ""
    print(f"The image's person's account is `{account['name']}`{lab}.")


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
    # Larger than the image, as a person's disk is, so the root grows on boot.
    os.truncate(DISK, max(DISK.stat().st_size, CFG["disk_gib"] << 30))
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


def ssh_args(user="root"):
    return ["ssh", "-i", str(KEY), "-p", str(CFG["ssh_port"]), "-o", "IdentitiesOnly=yes",
            "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={WORK / 'known_hosts'}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", f"{user}@127.0.0.1"]


def ssh(command, timeout=60, user="root"):
    return subprocess.run(ssh_args(user) + [command], text=True, capture_output=True,
                          timeout=timeout)


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


def root_fills_disk(out):
    """Whether the check's output shows the root partition reaching its
    disk's end, past the image's size, and its Btrfs filling the partition, to
    within the last part of a 4 KiB block that Btrfs leaves unused."""
    try:
        disk, start, size, filesystem = (int(value) for value in out.split())
    except ValueError:
        return False
    image = (1 + CFG["esp_mib"] + CFG["root_gib"] * 1024 + 1) * 2048
    return disk > image and disk - start - size <= 4096 and 0 <= size * 512 - filesystem < 4096


def person_may_reboot(out):
    """Whether the check's output shows the person in `wheel`, with `reboot`
    and `poweroff` that ask sudo for Void's own, and sudo letting them run
    both with no password: their groups, where each command is found, then
    what sudo allows."""
    lines = out.splitlines()
    return len(lines) == 5 and "wheel" in lines[0].split() and lines[1:] == [
        "/usr/local/bin/reboot", "/usr/local/bin/poweroff", "/usr/bin/reboot", "/usr/bin/poweroff"]


def person_has_home(out):
    """Whether the check's output shows the person's account with a home of
    its own under /home and a login shell: `name:home:shell`, then the home's
    owner and mode."""
    lines = out.splitlines()
    if len(lines) < 2 or len(account := lines[0].split(":")) != 3:
        return False
    name, home, shell = account
    return home == f"/home/{name}" and shell in ("/bin/bash", "/bin/sh") \
        and lines[1].split()[:1] == [name]


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
     lambda out: all(f"run: /var/service/{s}:" in out
                     for s in ("sshd", "dhcpcd", "agetty-ttyS0", "dbus", "iwd",
                               "crowbar-ctrl-alt-del"))),
    # The disk's size, the root partition's start and size, in 512-byte
    # sectors, then the size of the root's Btrfs, in bytes.
    ("the root fills a disk larger than the image",
     "p=$(basename \"$(readlink -f \"$(findmnt -no SOURCE / | sed 's/\\[.*//')\")\"); "
     "d=$(basename \"$(readlink -f /sys/class/block/$p/..)\"); "
     "cat /sys/class/block/$d/size /sys/class/block/$p/start /sys/class/block/$p/size; "
     "btrfs filesystem show --raw / | awk '/devid/ {print $4}'",
     lambda out: root_fills_disk(out)),
    ("the image loads both vendors' microcode early",
     "lsinitrd /boot/initramfs-$(uname -r).img 2>/dev/null | grep -o 'kernel/x86/microcode/[A-Za-z]*.bin' | sort -u",
     lambda out: set(MICROCODE) <= set(out.split())),
    ("the person may join Wi-Fi with iwctl",
     f"su -s /bin/sh -c 'id -nG; iwctl device list' \"$(getent passwd {PERSON_UID} | cut -d: -f1)\"",
     lambda out: "netdev" in out.splitlines()[0].split() if out else False),
    # sshd -T names each setting in lowercase or, in newer releases, as written.
    ("SSH takes keys and no passwords", "sshd -T | grep -iE '^(passwordauthentication|kbdinteractiveauthentication|pubkeyauthentication) '",
     lambda out: {"passwordauthentication no", "kbdinteractiveauthentication no",
                  "pubkeyauthentication yes"} <= set(out.lower().splitlines())),
    # Its name, home, and shell, then who owns the home and its mode.
    ("the person's account has a home and a login shell",
     f"getent passwd {PERSON_UID} | cut -d: -f1,6,7; "
     f"stat -c '%U %a' \"$(getent passwd {PERSON_UID} | cut -d: -f6)\"",
     lambda out: person_has_home(out)),
    # Logged in as the person, not as root: the build's key reaches their account too.
    ("the person's account takes the build's key over SSH", "id -u",
     lambda out: out.strip() == str(PERSON_UID), "person"),
    # A service's message and a kernel message, each with a mark of its own,
    # reach the system's log on the root.
    ("the system's log keeps services' and the kernel's messages",
     "m=crowbar-check-$$; logger -t crowbar-check \"$m-service\"; echo \"$m-kernel\" > /dev/kmsg; "
     "for i in $(seq 50); do "
     "grep -rqs \"$m-service\" /var/log/socklog && grep -qs \"$m-kernel\" /var/log/socklog/kernel/current && break; "
     "sleep 0.2; done; "
     "grep -rhs \"$m-\" /var/log/socklog | sed 's/.*crowbar-check-[0-9]*-//' | sort -u",
     lambda out: {"service", "kernel"} <= set(out.split())),
    # As the person: no password is asked for either, and sudo -n would fail if one were.
    ("the person may reboot and power off from the terminal",
     "id -nG; command -v reboot poweroff; sudo -n -l /usr/bin/reboot; sudo -n -l /usr/bin/poweroff",
     lambda out: person_may_reboot(out), "person"),
    ("network has an address", "ip -4 -o addr show scope global",
     lambda out: "inet " in out),
    ("packages keep their groups", "stat -c '%G %n' /usr/bin/wall /var/db/dhcpcd",
     lambda out: "tty /usr/bin/wall" in out and "_dhcpcd /var/db/dhcpcd" in out),
    ("kernel command line carries NVIDIA's settings", "cat /proc/cmdline",
     lambda out: set(CFG["kernel_args"]) <= set(out.split())),
    # The first line is where the module is, or nothing; the rest are the
    # modules loaded. The VM has no NVIDIA card, so none of NVIDIA's loads.
    ("NVIDIA's module is built, and not loaded without an NVIDIA card",
     "modinfo -n nvidia; cut -d' ' -f1 /proc/modules",
     lambda out: bool(out.split()) and re.search(r"/nvidia\.ko(\.\w+)?$", out.split()[0]) is not None
     and not {name.replace("-", "_") for name in NVIDIA_MODULES} & set(out.split()[1:])),
    ("no failed kernel messages", "dmesg --level=emerg,alert,crit",
     lambda out: out.strip() == ""),
]


def boot_checks():
    """Each check a booted Base must pass, as `(name, user, command, ok)`."""
    return [(name, LAB_ACCOUNT["name"] if as_person else "root", command, ok)
            for name, command, ok, *as_person in CHECKS]


def check():
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True,
                          capture_output=True).stdout.strip()
    preflight(vm=True, build=True)
    build(force=False, lab_account=True)
    to_login = boot()
    lines = [f"CrowbarOS base boot check, {stamp}, commit {head}",
             f"disk sha256 {sha256(BASE_DISK)}", f"login prompt after {to_login:.1f}s", ""]
    failed = 0
    for name, user, command, ok in boot_checks():
        result = ssh(command, user=user)
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
    build_parser = sub.add_parser("build")
    build_parser.add_argument("--force", action="store_true", help="install the Void roots again")
    build_parser.add_argument("--lab-account", action="store_true",
                              help="make the lab account and ignore the account file, and add the lab's "
                                   "packages, as the checks do")
    sub.add_parser("boot").add_argument("--window", action="store_true",
                                        help="also show the guest's screen in a window")
    sub.add_parser("ssh").add_argument("command", nargs=argparse.REMAINDER)
    sub.add_parser("console")
    sub.add_parser("stop")
    sub.add_parser("check")
    args = parser.parse_args()
    if args.cmd == "build":
        build(args.force, args.lab_account)
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
