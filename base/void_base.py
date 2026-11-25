#!/usr/bin/env python3
"""The CrowbarOS base: plain Void Linux and the desktop, built rootless and booted under UEFI.

Stdlib only, no sudo. Root-owned work happens inside `unshare -r` user
namespaces. Downloads are shared with the earlier lab in ~/.cache/crowbaros/void;
the roots, the build, the disk, VM runtime files, and boot logs live in
<repo>/.lab/base.

The desktop (Fulcrum, the shell, Files, and Studio Settings) is built and
tested in a Void build root of its own, so it links Void's own libraries, and
goes into the image with its runit services from desktop/.

    void_base.py build [--force] [--lab-account]
                                   install the Void roots, build the desktop, and write the boot disk
    void_base.py boot [--window]   boot a fresh copy of the disk and wait for it
    void_base.py ssh [CMD]         run a command in the guest as root
    void_base.py console           attach to the guest's serial console
    void_base.py stop              power the guest off
    void_base.py check             build, boot, check, record, and power off
    void_base.py root save|restore ARCHIVE
                                   pack the finished Void root, or unpack one in its place
    void_base.py root key          name what installs the Void root, as CI's cache keys it
"""
import argparse
import ast
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
import tempfile
import threading
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
# target directory it builds in. The three apps share theirs.
PROJECTS = [("compositor", "compositor"), ("apps/shell", ""), ("apps/files", ""), ("apps/studio-settings", ""),
            ("base/helpers", "helpers"), ("lever", "lever")]
# The programs the image holds, and the project each comes from. The probe is
# Fulcrum's test client. The helpers are the desktop's four helpers in one
# program; see helpers/README.md.
PROGRAMS = {"crowbar-compositor": "compositor", "crowbar-probe": "compositor",
            "crowbaros-shell": "apps/shell", "crowbaros-files": "apps/files", "crowbaros-studio-settings": "apps/studio-settings",
            "crowbar-helpers": "base/helpers", "crowbar-lever": "lever", "crowbar-agent": "lever"}


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


def pinned(name, url, pin, holds):
    """Where the archive at `url` is unpacked, which holds the file `holds`.
    Downloads and unpacks it once for each pin, its SHA-256, and refuses an
    archive that does not match the pin."""
    CACHE.mkdir(parents=True, exist_ok=True)
    # The archive and what is unpacked from it are both kept under the pin's
    # name. A new pin never finds the last one's, and a build in another
    # checkout, with another pin, leaves this one's as it is.
    target = CACHE / f"{name}-{pin}"
    witness = target / holds
    if witness.exists():
        return target
    tarball = CACHE / f"{pin}-{url.rsplit('/', 1)[1]}"
    if not tarball.exists():
        # Downloaded to a file of this build's own, checked there, and moved
        # into place: two builds never write one file, and a download that is
        # not the pinned archive is never kept.
        handle, part = tempfile.mkstemp(prefix=tarball.name + ".", dir=CACHE)
        os.close(handle)
        part = Path(part)
        try:
            run("curl", "-fL", "--retry", "3", "-o", part, url)
            if sha256(part) != pin:
                raise SystemExit(f"{url} did not match the pinned SHA-256; the download was refused")
            part.replace(tarball)
        finally:
            part.unlink(missing_ok=True)
    if sha256(tarball) != pin:
        tarball.unlink(missing_ok=True)
        raise SystemExit(f"{tarball} did not match the pinned SHA-256; it was removed")
    # Unpacked beside its place and moved there whole, so an unpacking that
    # stops partway is never taken for the archive's contents.
    unpacking = Path(tempfile.mkdtemp(prefix=target.name + ".", dir=CACHE))
    try:
        with tarfile.open(tarball) as archive:
            archive.extractall(unpacking, filter="data")
        unpacking.chmod(0o755)
        try:
            unpacking.rename(target)
        except OSError:
            # Another build unpacked the same pin first, or the directory is
            # there without the file, as a cache restored in part leaves it.
            if not witness.exists():
                shutil.rmtree(target, ignore_errors=True)
                unpacking.rename(target)
    finally:
        shutil.rmtree(unpacking, ignore_errors=True)
    return target


def fetch():
    """Where the pinned static xbps is unpacked."""
    return pinned("xbps-static", CFG["xbps_static_url"], CFG["xbps_static_sha256"],
                  "usr/bin/xbps-install.static")


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


# How the root is packed into one archive and unpacked again: inside the user
# namespace the install ran in, so each file keeps its owner and group, with
# its extended attributes, ACLs, and holes. The root comes before its `.done`,
# so an unpack that stops partway leaves a root `install` does not trust.
ROOT_ARCHIVE = ["--zstd", "--numeric-owner", "--xattrs", "--xattrs-include=*", "--acls", "--sparse"]


def save_root(archive):
    """Pack the finished Void root into `archive`. CI caches it, so a run
    whose root would be installed from the same files restores it instead."""
    done = ROOTFS.with_name(ROOTFS.name + ".done")
    if not done.exists() or (lack := missing_from_root(ROOTFS)):
        raise SystemExit(f"The Void root at {ROOTFS} is not a finished install"
                         + (f": it has {lack}" if done.exists() else "") + "; nothing saved")
    part = archive.with_name(archive.name + ".part")
    part.parent.mkdir(parents=True, exist_ok=True)
    run(*userns(), "tar", *ROOT_ARCHIVE, "-C", WORK, "-cf", part, ROOTFS.name, done.name)
    part.replace(archive)
    print(f"Saved the Void root to {archive} ({archive.stat().st_size >> 20} MiB)")


def restore_root(archive):
    """Unpack a root `save_root` packed, in place of the one here. The build
    still checks it as it checks any root: `install` uses it only if it was
    installed from the packages and repositories asked for now, and the image
    is written only from a root that has its kernel, microcode, and NVIDIA's
    modules. An archive that does not unpack leaves no root to trust, and the
    build installs one."""
    done = ROOTFS.with_name(ROOTFS.name + ".done")
    WORK.mkdir(parents=True, exist_ok=True)
    done.unlink(missing_ok=True)
    try:
        run(*userns(), "rm", "-rf", ROOTFS)
        run(*userns(), "tar", *ROOT_ARCHIVE, "-C", WORK, "-xpf", archive)
    except subprocess.CalledProcessError:
        done.unlink(missing_ok=True)
        print(f"{archive} did not unpack; the build installs the Void root", flush=True)
        return False
    print(f"Restored the Void root from {archive}")
    return True


def reached(source, names):
    """Every definition at a module's top level that the named ones use,
    themselves included, as the statements that define each name: a function,
    a class, or an assignment, and whatever those name in turn. Every import
    is reached, under the name `import`. A name a function uses only for a
    local of its own still counts, which errs toward reaching more.

    Only a module of imports, definitions, a docstring, and the `__main__`
    guard can be followed. Any other statement at the top level, such as a
    call or an `if` that changes a name after it is defined, could change
    what a reached name means with nothing here to see it, so it is refused.
    So is code anywhere else in the module that changes a reached name in a
    way that can be seen: `global`, `:=`, a store to or a `del` of one of its
    items or attributes, or a call of a method that changes a list, a dict,
    or a set. This follows names. It cannot see a reached value changed
    under another name."""
    defined = {"import": []}
    unreached = []
    for node in ast.parse(source).body:
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            targets = ["import"]
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            targets = [node.name]
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = [name.id for target in (node.targets if isinstance(node, ast.Assign)
                                              else [node.target])
                       for name in ast.walk(target) if isinstance(name, ast.Name)]
        elif (isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant)
              or isinstance(node, ast.If) and ast.unparse(node.test) == "__name__ == '__main__'"
              and not node.orelse and all(isinstance(line, ast.Expr) for line in node.body)):
            # The guard may only call: its body runs at the top level, where
            # an assignment, an import, or a `def` would give a name a new
            # meaning.
            unreached.append(node)
            continue
        else:
            raise ValueError(f"line {node.lineno}: a statement at the top level that is not an "
                             "import, a definition, or an assignment cannot be followed")
        for target in targets:
            defined.setdefault(target, []).append(node)
    found, waiting = {}, ["import", *names]
    while waiting:
        name = waiting.pop()
        if name in found or name not in defined:
            continue
        found[name] = defined[name]
        waiting += [used.id for node in defined[name] for used in ast.walk(node)
                    if isinstance(used, ast.Name)]
    followed = {id(node) for nodes in found.values() for node in nodes}
    unreached += [node for nodes in defined.values() for node in nodes if id(node) not in followed]
    for node in unreached:
        for part in ast.walk(node):
            if name := changes(part) & set(found):
                raise ValueError(f"line {part.lineno}: code the key does not follow changes "
                                 f"`{min(name)}`, which it does")
    return found


# The methods that change a list, a dict, or a set.
CHANGING = {"append", "extend", "insert", "remove", "pop", "clear", "sort", "reverse", "update",
            "setdefault", "popitem", "add", "discard", "difference_update",
            "intersection_update", "symmetric_difference_update", "__setitem__", "__delitem__"}


def changes(part):
    """The names this one piece of syntax may change other than by a plain
    assignment: those `global` names, the one `:=` binds, the one whose item
    or attribute is stored to or deleted, and the one a changing method is
    called on, however deep in it (`CFG["packages"].append`)."""
    def holder(value):
        while isinstance(value, (ast.Subscript, ast.Attribute)):
            value = value.value
        return {value.id} if isinstance(value, ast.Name) else set()
    if isinstance(part, (ast.Global, ast.Nonlocal)):
        return set(part.names)
    if isinstance(part, ast.NamedExpr):
        return {part.target.id}
    if isinstance(part, (ast.Subscript, ast.Attribute)) and isinstance(part.ctx, (ast.Store, ast.Del)):
        return holder(part.value)
    if (isinstance(part, ast.Call) and isinstance(part.func, ast.Attribute)
            and part.func.attr in CHANGING):
        return holder(part.func.value)
    return set()


def reachable_source(source, names):
    """The source of what `reached` finds, by name, decorators included."""
    lines = source.splitlines(keepends=True)

    def text(node):
        first = min([node.lineno] + [d.lineno for d in getattr(node, "decorator_list", [])])
        return "".join(lines[first - 1:node.end_lineno])
    return {name: "".join(text(node) for node in nodes)
            for name, nodes in reached(source, names).items()}


def config_read(source, names):
    """The keys of `CFG` that what `reached` finds reads. Each must be read
    as `CFG["key"]`: anything else done with `CFG` could read any key, so it
    is refused."""
    keys = set()
    for name, nodes in reached(source, names).items():
        if name == "CFG":
            continue
        for node in nodes:
            by_key = {id(part.value): part.slice.value for part in ast.walk(node)
                      if isinstance(part, ast.Subscript) and isinstance(part.slice, ast.Constant)
                      and isinstance(part.slice.value, str)}
            for part in ast.walk(node):
                if isinstance(part, ast.Name) and part.id == "CFG":
                    if id(part) not in by_key:
                        raise ValueError(f"line {part.lineno}: `CFG` is used other than as "
                                         '`CFG["key"]`, so what it reads cannot be followed')
                    keys.add(by_key[id(part)])
    return keys


# What installs, checks, packs, and unpacks the Void root. `root_key` follows
# everything these use.
ROOT_CODE = ["rootfs", "save_root", "restore_root"]


def root_key():
    """A digest of all that decides what a finished Void root holds and how
    its archive is packed: the code here that installs, checks, packs, and
    unpacks the root, every value in config.json that code reads (the
    packages, the repository, and the pinned xbps), and host/rootfs.sh. CI's
    cache keys the root on it, so a change to anything else here, such as a
    boot check, still restores the root main installed, and a change to any
    of this installs a new one. A file this code came to read besides
    rootfs.sh would have to be added here."""
    source = Path(__file__).read_text()
    try:
        decides = {
            "config": {key: CFG[key] for key in sorted(config_read(source, ROOT_CODE))},
            "script": (HERE / "host/rootfs.sh").read_text(),
            "code": reachable_source(source, ROOT_CODE),
        }
    except ValueError as error:
        raise SystemExit(f"{__file__}: {error}; see `root_key`")
    return hashlib.sha256(json.dumps(decides, sort_keys=True).encode()).hexdigest()


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
    # `netdev` and `bluetooth` are groups the image makes or ensures for hardware access.
    if account["name"] in names_in_root(ROOTFS) | {"netdev", "bluetooth"}:
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
    # What only the lab's image gets: python3, as the lab's checks drive the
    # guest with `python3 -c`. It comes from the root's own package index, so
    # it matches what the root was installed with.
    lab_packages = CFG["lab_packages"] if lab_account else []
    xbps = fetch() if lab_packages else None
    if not KEY.exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "crowbaros-base", "-f", KEY)
    # The image is written beside its place and moved there only when it is
    # whole, so a build that stops partway leaves the last whole image, or
    # none, where `boot` and a person's `dd` look for one.
    part = BASE_DISK.with_name(BASE_DISK.name + ".part")
    part.unlink(missing_ok=True)
    env = dict(os.environ, ROOTFS=str(ROOTFS), WORK=str(WORK), OUT=str(part),
               PUBKEY=str(KEY) + ".pub", HOSTNAME=CFG["hostname"],
               ROOT_LABEL=CFG["root_label"], ESP_LABEL=CFG["esp_label"],
               ESP_MIB=str(CFG["esp_mib"]), ROOT_GIB=str(CFG["root_gib"]),
               KVER=image_kernel(ROOTFS), KERNEL_ARGS=" ".join(CFG["kernel_args"]),
               NVIDIA_OPTIONS=" ".join(CFG["nvidia_options"]),
               DESKTOP=str(HERE / "desktop"), REPO_ROOT=str(ROOT), PERSON_UID=str(PERSON_UID),
               PERSON=account["name"], PERSON_FULL_NAME=account["full_name"],
               PERSON_PASSWORD_HASH=account["password_hash"], PERSON_PASSWORD=account["password"],
               PERSON_SSH_KEYS="".join(key + "\n" for key in account["ssh_keys"]),
               LAB_PACKAGES=" ".join(lab_packages), XBPS=str(xbps / "usr/bin") if xbps else "",
               REPOS=REPOSITORY, PKGCACHE=str(CACHE / "packages"))
    run(*userns(), "--mount", "sh", HERE / "host/image.sh", *binaries().values(), env=env)
    part.replace(BASE_DISK)
    print(f"Built {BASE_DISK} with kernel {env['KVER']}")
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


def nvidia_saves_video_memory(out):
    """Whether the check's output shows NVIDIA's module set to save all video
    memory across suspend into /var/tmp, on the root's disk: `modprobe -c`'s
    lines for the module, then the type of the file system under /var/tmp.
    modprobe gives the module every line's options, and the last value of
    each wins."""
    lines = out.splitlines()
    if not lines or lines[-1].strip() in ("", "tmpfs"):
        return False
    options = {}
    for line in lines[:-1]:
        if line.split()[:2] == ["options", "nvidia"]:
            options.update(word.split("=", 1) for word in line.split()[2:] if "=" in word)
    return all(options.get(key) == value
               for key, value in (option.split("=", 1) for option in CFG["nvidia_options"]))


def compositor_restarted(out):
    """Whether the check's output shows the compositor's service restarted
    with one new compositor, which stayed: `sv restart`'s exit status, then
    the compositor's account's processes before, after, and a moment later."""
    lines = dict((line.split(" ", 1) + [""])[:2] for line in out.splitlines())
    if not {"restart", "before", "after", "later"} <= lines.keys():
        return False
    after = lines["after"].split()
    return lines["restart"] == "0" and len(after) == 1 \
        and after[0] not in lines["before"].split() and lines["later"].split() == after


# The desktop's services, and how many seconds each must have run. runit
# starts a service that exits again a second later, so one that cannot start
# shows as running for a moment, over and over.
DESKTOP_SERVICES = ("seatd", "crowbar-compositor", "crowbar-shell", "crowbar-launcher",
                    "crowbar-unlock", "crowbar-lever", "crowbar-pipewire")
# One digit: the guest's own pattern for it, in CHECKS, counts on that.
STAYED_UP = 3
# The shell's service starts `crowbar-shell-connect`, which waits for the
# compositor's sockets, for as long as 20 seconds each, and then becomes this
# program. Until it has, the service is up and the shell is not.
SHELL_PROGRAM = "/usr/local/bin/crowbaros-shell"
# Lever, as the image holds it and its service, desktop/lever-run, runs it.
LEVER_PROGRAM = "/usr/local/bin/crowbar-lever"
LEVER_SURFACE_SOCKET = "/run/crowbar-lever/surface"
LEVER_PERSON_SOCKET = f"/run/user/{PERSON_UID}/crowbar-lever/person"
LEVER_STATE = ".local/state/crowbaros/lever"
# Starting idle memory limit for Lever, in MiB (issue #371 / #373).
LEVER_MEMORY_MIB = 50
# `{"version":1}` and `{"op":"status"}` as frames, in hexadecimal: 4-byte
# big-endian length prefix followed by UTF-8 JSON (lever/README.md).
VERSION_FRAME = "0000000d7b2276657273696f6e223a317d"
STATUS_FRAME = "0000000f7b226f70223a22737461747573227d"
PROBE_TIMEOUT = 3
# In the guest, `probe SOCKET ACCOUNT` connects as that account (or
# `ACCOUNT:GROUP`), states version 1, and if accepted asks `status`. Prints
# `ok <version_hex> <status_json>`, `refused` when closed with nothing sent,
# or `error: <reason>`.
LEVER_PROBE = (
    "probe() { chpst -u \"$2\" python3 -c '\n"
    "import socket, struct, sys\n"
    "s = socket.socket(socket.AF_UNIX)\n"
    f"s.settimeout({PROBE_TIMEOUT})\n"
    "def recv_exact(n):\n"
    "    buf = b\"\"\n"
    "    while len(buf) < n and (more := s.recv(n - len(buf))):\n"
    "        buf += more\n"
    "    return buf\n"
    "try:\n"
    "    s.connect(sys.argv[1])\n"
    "    vframe = bytes.fromhex(sys.argv[2])\n"
    "    s.sendall(vframe)\n"
    "    vreply = recv_exact(len(vframe))\n"
    "    if not vreply:\n"
    "        print(\"refused\")\n"
    "        sys.exit(0)\n"
    "    s.sendall(bytes.fromhex(sys.argv[3]))\n"
    "    hdr = recv_exact(4)\n"
    "    if len(hdr) < 4:\n"
    "        print(\"short\")\n"
    "        sys.exit(0)\n"
    "    (length,) = struct.unpack(\">I\", hdr)\n"
    "    body = recv_exact(length)\n"
    "    print(\"ok\", vreply.hex(), body.decode())\n"
    "except (ConnectionResetError, BrokenPipeError):\n"
    "    print(\"refused\")\n"
    "except OSError as error:\n"
    "    print(\"error:\", error.strerror or error)\n"
    f"' \"$1\" {VERSION_FRAME} {STATUS_FRAME} 2>&1; }}; "
)
# In the guest, the process Lever's service runs, or nothing.
LEVER_PID = ("pid() { sv status /var/service/crowbar-lever | "
             "sed -n 's|^run: [^ ]* (pid \\([0-9]*\\)).*|\\1|p'; }; ")
# In the guest, the person's account name and home.
PERSON_IN_GUEST = (f"u=$(getent passwd {PERSON_UID} | cut -d: -f1); "
                   f"h=$(getent passwd {PERSON_UID} | cut -d: -f6); ")


def wait_in_guest(condition, seconds=30):
    """In the guest: tries `condition`, a shell command list, five times a
    second until it holds or `seconds` have passed."""
    return (f"t=$(($(date +%s) + {seconds})); until {condition}; do "
            "[ \"$(date +%s)\" -ge \"$t\" ] && break; sleep 0.2; done; ")


def labelled(out):
    """A check's output by the word each line starts with: every `label rest`
    line, as `{label: [rest, ...]}`."""
    lines = {}
    for line in out.splitlines():
        label, _, rest = line.partition(" ")
        lines.setdefault(label, []).append(rest)
    return lines


def only(lines, label):
    """What the one line with this label says, or None when there is no such
    line or more than one."""
    return lines[label][0] if len(lines.get(label, [])) == 1 else None


def lever_status_ok(answer):
    """Whether `answer` from `LEVER_PROBE` shows version 1 echoed and a
    `status` reply with Lever's version and no harness started."""
    if not answer or not answer.startswith(f"ok {VERSION_FRAME} "):
        return False
    try:
        status = json.loads(answer.split(" ", 2)[2])
    except ValueError:
        return False
    return (isinstance(status, dict)
            and status.get("reply") == "status"
            and isinstance(status.get("version"), str) and bool(status["version"])
            and status.get("harness_started") is False
            and status.get("harnesses") == [])


def lever_answers_surface(out):
    """Whether Lever answers version 1 and `status` to the shell's account on
    its surface socket, refuses root, the person, and another account with the
    shell's group, keeps others out of the directory, and owns the directory
    and socket with the shell's group alone."""
    lines = labelled(out)
    shell = (only(lines, "shell-account") or "").split()
    if len(shell) != 2:
        return False
    return lever_status_ok(only(lines, "shell")) and all(only(lines, label) == value for label, value in (
        ("root", "refused"), ("person", "refused"), ("group", "refused"),
        ("other", "error: Permission denied"),
        ("directory", f"{PERSON_UID} {shell[1]} 2750"), ("socket", f"{PERSON_UID} {shell[1]} 660")))


def lever_answers_person(out):
    """Whether Lever answers version 1 and `status` to the person's account on
    its person socket, refuses root by SO_PEERCRED, keeps the shell out of the
    directory, and keeps the directory and socket private to the person."""
    lines = labelled(out)
    return lever_status_ok(only(lines, "person")) and all(only(lines, label) == value for label, value in (
        ("root", "refused"), ("shell", "error: Permission denied"),
        ("directory", f"{PERSON_UID} 700"), ("socket", f"{PERSON_UID} 600")))


def lever_runs_as_the_person(out):
    """Whether Lever's service stays up running `crowbar-lever` as the person
    with their groups, in the launcher's app environment, starting in their
    home, with its state directory (`700`) and files (`600`) theirs alone."""
    lines = labelled(out)
    up = re.match(r"run: /var/service/crowbar-lever: \(pid \d+\) (\d+)s", only(lines, "service") or "")
    process, person = ((only(lines, label) or "").split() for label in ("process", "person"))
    home = only(lines, "home")
    env = dict(line.split("=", 1) for line in lines.get("env", []) if "=" in line)
    state = [line.split(" ", 2) for line in lines.get("state", [])]
    if not up or len(process) < 3 or not home or not state or any(len(entry) != 3 for entry in state):
        return False
    return int(up[1]) >= STAYED_UP \
        and process[:2] == person[:2] and process[0] == str(PERSON_UID) \
        and set(process[2:]) == set(person[2:]) \
        and only(lines, "program") == LEVER_PROGRAM and only(lines, "cwd") == home \
        and env.get("HOME") == home and env.get("XDG_RUNTIME_DIR") == f"/run/user/{PERSON_UID}" \
        and env.get("WAYLAND_DISPLAY") == "wayland-0" and env.get("CROWBAR_SESSION") == "crowbaros" \
        and bool(env.get("PATH")) and bool(env.get("LANG")) \
        and state[0] == ["700", str(PERSON_UID), f"{home}/{LEVER_STATE}"] \
        and all(entry[:2] == ["600", str(PERSON_UID)] for entry in state[1:]) \
        and {"lever.log", "lever.sqlite"} <= {entry[2].rsplit("/", 1)[1] for entry in state[1:]}


def lever_restarted(out):
    """Whether runit starts Lever again after a kill, the new Lever answers the
    shell's account, and `lever.log` records the restart."""
    lines = labelled(out)
    before, after = only(lines, "before"), only(lines, "after")
    return bool(before) and bool(after) and before != after \
        and lever_status_ok(only(lines, "shell")) and only(lines, "logged") == "1"


def lever_is_small(out):
    """Whether the idle Lever holds less than `LEVER_MEMORY_MIB` MiB of RSS."""
    return out.strip().isdigit() and 0 < int(out) < LEVER_MEMORY_MIB * 1024


def desktop_services_stay_up(out):
    """Whether the check's output shows each of the desktop's services
    running, and for at least STAYED_UP seconds, and the shell's service
    running the shell: `sv status` of the services, then `shell` and the
    program the shell's service runs."""
    up = {}
    for line in out.splitlines():
        if found := re.match(r"run: /var/service/([^:]+): \(pid \d+\) (\d+)s", line):
            up[found[1]] = int(found[2])
    return all(up.get(service, 0) >= STAYED_UP for service in DESKTOP_SERVICES) \
        and f"shell {SHELL_PROGRAM}" in out.splitlines()


# Retired Pi Durable Harness, Node.js, and `crowbar-agent` service artifacts
# that Base's image must no longer carry (ADR 0036). `/usr/local/bin/crowbar-agent`
# is back as Lever's Rust client.
RETIRED_AGENT_PATHS = (
    "/usr/local/lib/crowbar-agent",
    "/usr/share/licenses/crowbar-agent",
    "/usr/bin/node",
    "/usr/local/bin/node",
    "/etc/sv/crowbar-agent",
    "/var/service/crowbar-agent",
    "/run/crowbar-agent",
)
RETIRED_AGENT_COMMANDS = ("node", "npm", "npx")


def no_harness_or_node(out):
    """Whether the check's output shows the image holds no retired Harness,
    no Node.js, and no `crowbar-agent` service."""
    return out.strip() == "clean"


def pipewire_audio_ready(out):
    """Whether PipeWire, WirePlumber, and pipewire-pulse are up as the person
    and `wpctl status` reports a working audio graph."""
    lines = out.splitlines()
    return ("pulse-socket yes" in lines
            and "PipeWire" in out
            and "Audio" in out
            and "WirePlumber" in out)


def bluetooth_reachable(out):
    """Whether `bluetoothd` is supervised by runit and answers `org.bluez`
    over the system D-Bus (even with no physical Bluetooth adapter)."""
    lines = out.splitlines()
    return (any(line.startswith("run: /var/service/bluetoothd:") for line in lines)
            and "dbus-exit 0" in lines
            and "array [" in out)


def mpv_defaults_ready(out):
    """Whether `mpv --version` runs cleanly, its linked libraries resolve, and
    `video/mp4` + `audio/mpeg` default to `mpv.desktop`."""
    lines = labelled(out)
    return (only(lines, "video") == "mpv.desktop"
            and only(lines, "audio") == "mpv.desktop"
            and bool(only(lines, "mpv")) and only(lines, "mpv").startswith("mpv ")
            and only(lines, "conf") == "yes"
            and "=> /" in out and "not found" not in out)


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
                               "bluetoothd", "crowbar-pipewire", "crowbar-ctrl-alt-del"))),
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
    # The compositor's account runs nothing but the compositor. Restarting
    # its service leaves one compositor, a new one, which stays up; the
    # desktop check finds it locked.
    ("the compositor's service restarts",
     "c=crowbar-compositor; b=$(pgrep -u $c | xargs); sv restart $c >/dev/null; echo \"restart $?\"; "
     "for i in $(seq 50); do a=$(pgrep -u $c | xargs); [ -n \"$a\" ] && [ \"$a\" != \"$b\" ] && break; "
     "sleep 0.2; done; sleep 2; echo \"before $b\"; echo \"after $a\"; echo \"later $(pgrep -u $c | xargs)\"",
     lambda out: compositor_restarted(out)),
    # After the restart above, which the shell must come back from. Waits
    # until every service has run for STAYED_UP seconds and the shell's
    # service runs the shell, for at most 30.
    ("the desktop's services stay up",
     "for i in $(seq 150); do "
     f"o=$(sv status {' '.join('/var/service/' + s for s in DESKTOP_SERVICES)}); "
     # The program of the process that line of `sv status` names, so the
     # time it has run and what it runs are one process's.
     "p=$(echo \"$o\" | sed -n 's|^run: /var/service/crowbar-shell: (pid \\([0-9]*\\)).*|\\1|p'); "
     "e=$(readlink \"/proc/$p/exe\" 2>/dev/null); "
     "echo \"$o\" | grep -Evq '^run: [^ ]+ \\(pid [0-9]+\\) "
     f"([{STAYED_UP}-9]|[0-9]{{2,}})s' || [ \"$e\" != {SHELL_PROGRAM} ] || break; "
     "sleep 0.2; done; echo \"$o\"; echo \"shell $e\"",
     lambda out: desktop_services_stay_up(out)),
    # The unlock helper and the compositor are two programs that speak one
    # framing (ADR 0035), and the tests of each play the other: here they
    # meet. This is the suspend hook's command, and the desktop is locked
    # from boot, so it changes nothing.
    ("the compositor answers the unlock helper's lock",
     "/usr/local/bin/crowbar-unlock lock; echo \"exit $?\"",
     lambda out: out.strip() == "exit 0"),
    # Lever (ADR 0036) starts at boot, before anyone unlocks. Waits for its
    # answer to the shell's account on the surface socket, for at most 30s.
    # Each peer connects as itself so root cannot mask mode errors, and
    # crowbar-compositor:crowbar-shell enters the directory by group to test
    # SO_PEERCRED refusal.
    ("Lever answers the shell's account on its surface socket, and no other",
     LEVER_PROBE + PERSON_IN_GUEST
     + wait_in_guest(f"s=$(probe {LEVER_SURFACE_SOCKET} crowbar-shell); "
                     f"case \"$s\" in 'ok {VERSION_FRAME} '*) true;; *) false;; esac")
     + f"echo \"shell $s\"; echo \"root $(probe {LEVER_SURFACE_SOCKET} root)\"; "
     f"echo \"person $(probe {LEVER_SURFACE_SOCKET} \"$u\")\"; "
     f"echo \"group $(probe {LEVER_SURFACE_SOCKET} crowbar-compositor:crowbar-shell)\"; "
     f"echo \"other $(probe {LEVER_SURFACE_SOCKET} crowbar-compositor)\"; "
     f"stat -c 'directory %u %g %a' {LEVER_SURFACE_SOCKET.rsplit('/', 1)[0]}; "
     f"stat -c 'socket %u %g %a' {LEVER_SURFACE_SOCKET}; "
     "echo \"shell-account $(id -u crowbar-shell) $(id -g crowbar-shell)\"",
     lambda out: lever_answers_surface(out)),
    ("Lever answers the person's account on its person socket, and no other",
     LEVER_PROBE + PERSON_IN_GUEST
     + f"echo \"person $(probe {LEVER_PERSON_SOCKET} \"$u\")\"; "
     f"echo \"root $(probe {LEVER_PERSON_SOCKET} root)\"; "
     f"echo \"shell $(probe {LEVER_PERSON_SOCKET} crowbar-shell)\"; "
     f"stat -c 'directory %u %a' {LEVER_PERSON_SOCKET.rsplit('/', 1)[0]}; "
     f"stat -c 'socket %u %a' {LEVER_PERSON_SOCKET}",
     lambda out: lever_answers_person(out)),
    # Waits until the service has run for STAYED_UP seconds, for at most 30s.
    ("Lever runs as the person, in an app's environment, with its state theirs alone",
     LEVER_PID + PERSON_IN_GUEST
     + wait_in_guest("o=$(sv status /var/service/crowbar-lever); echo \"$o\" | "
                     f"grep -Eq '^run: [^ ]+ \\(pid [0-9]+\\) ([{STAYED_UP}-9]|[0-9]{{2,}})s'")
     + "p=$(pid); echo \"service $o\"; "
     "awk '/^Uid:/{u=$2} /^Gid:/{g=$2} /^Groups:/{$1=\"\"; gr=$0} END{print \"process\", u, g, gr}' \"/proc/$p/status\"; "
     "echo \"person $(id -u \"$u\") $(id -g \"$u\") $(id -G \"$u\")\"; "
     "echo \"program $(readlink \"/proc/$p/exe\")\"; "
     "echo \"cwd $(readlink \"/proc/$p/cwd\")\"; "
     "echo \"home $h\"; "
     "tr '\\0' '\\n' < \"/proc/$p/environ\" | sed 's/^/env /'; "
     f"stat -c 'state %a %u %n' \"$h/{LEVER_STATE}\" \"$h/{LEVER_STATE}\"/*",
     lambda out: lever_runs_as_the_person(out)),
    ("Lever's service starts it again after a kill, and its log records the restart",
     LEVER_PROBE + LEVER_PID + PERSON_IN_GUEST
     + "b=$(pid); kill -KILL \"$b\"; s=; "
     + wait_in_guest(f"a=$(pid); [ -n \"$a\" ] && [ \"$a\" != \"$b\" ] "
                     f"&& s=$(probe {LEVER_SURFACE_SOCKET} crowbar-shell) "
                     f"&& case \"$s\" in 'ok {VERSION_FRAME} '*) true;; *) false;; esac")
     + "echo \"before $b\"; echo \"after $a\"; echo \"shell $s\"; "
     "echo \"logged $(grep -c \"restart: the Lever before this one, process $b, ended without stopping\" "
     f"\"$h/{LEVER_STATE}/lever.log\")\"",
     lambda out: lever_restarted(out)),
    ("the idle Lever holds less than 50 MiB of memory",
     LEVER_PID + "awk '/^VmRSS:/ {print $2}' \"/proc/$(pid)/status\"",
     lambda out: lever_is_small(out)),
    ("crowbar-agent reaches Lever as the person",
     PERSON_IN_GUEST
     + "out=$(su -s /bin/sh \"$u\" -c '/usr/local/bin/crowbar-agent --list 2>&1'); rc=$?; "
     "echo \"exit $rc\"; printf '%s\\n' \"$out\"",
     lambda out: out.startswith("exit 0\n") and "No threads yet." in out),
    ("PipeWire and WirePlumber serve a working audio graph for the person",
     PERSON_IN_GUEST
     + wait_in_guest(f"[ -S /run/user/{PERSON_UID}/pipewire-0 ] && [ -S /run/user/{PERSON_UID}/pulse/native ] "
                     f"&& su -s /bin/sh \"$u\" -c 'XDG_RUNTIME_DIR=/run/user/{PERSON_UID} wpctl status' 2>/dev/null "
                     "| grep -q WirePlumber")
     + f"[ -S /run/user/{PERSON_UID}/pulse/native ] && echo 'pulse-socket yes' || echo 'pulse-socket no'; "
     f"su -s /bin/sh \"$u\" -c 'XDG_RUNTIME_DIR=/run/user/{PERSON_UID} wpctl status'",
     lambda out: pipewire_audio_ready(out)),
    ("bluetoothd is supervised and reachable over system D-Bus",
     wait_in_guest("dbus-send --system --print-reply --dest=org.bluez / "
                   "org.freedesktop.DBus.ObjectManager.GetManagedObjects >/dev/null 2>&1")
     + "sv status /var/service/bluetoothd; "
     "out=$(dbus-send --system --print-reply --dest=org.bluez / "
     "org.freedesktop.DBus.ObjectManager.GetManagedObjects 2>&1); rc=$?; "
     "echo \"dbus-exit $rc\"; printf '%s\\n' \"$out\"",
     lambda out: bluetooth_reachable(out)),
    ("mpv runs cleanly as the person and handles video and audio MIME defaults",
     PERSON_IN_GUEST
     + "echo \"video $(su -s /bin/sh \"$u\" -c 'xdg-mime query default video/mp4')\"; "
     "echo \"audio $(su -s /bin/sh \"$u\" -c 'xdg-mime query default audio/mpeg')\"; "
     "echo \"mpv $(su -s /bin/sh \"$u\" -c 'mpv --version | head -n 1')\"; "
     "test -f /etc/mpv/mpv.conf && echo 'conf yes' || echo 'conf no'; "
     "ldd /usr/bin/mpv",
     lambda out: mpv_defaults_ready(out)),
    # ADR 0036 retired the Pi Durable Harness and Node.js: fail if the image
    # still carries any Harness files, Node.js, or the `crowbar-agent` service.
    ("the image holds no Harness, no Node.js, and no crowbar-agent service",
     f"for p in {' '.join(RETIRED_AGENT_PATHS)}; do "
     "[ -e \"$p\" ] || [ -L \"$p\" ] && echo \"path $p\"; done; "
     f"for c in {' '.join(RETIRED_AGENT_COMMANDS)}; do "
     "found=$(command -v \"$c\" 2>/dev/null || true); [ -n \"$found\" ] && echo \"cmd $c $found\"; done; "
     "pkgs=$(xbps-query -l) || exit 1; "
     "printf '%s\\n' \"$pkgs\" | awk '$2 ~ /^nodejs([0-9]|-)/ {print \"pkg \" $2}'; "
     "echo clean",
     lambda out: no_harness_or_node(out)),
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
    # Read from modprobe's configuration, as the VM's module never loads.
    ("NVIDIA's module saves video memory to the disk across suspend",
     "modprobe -c | grep '^options nvidia '; findmnt -no FSTYPE -T /var/tmp",
     lambda out: nvidia_saves_video_memory(out)),
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
    # The image is hashed while its copy boots: the guest writes only the
    # copy, and hashing 8 GB takes as long as the boot does.
    hashed = {}

    def hash_image():
        try:
            hashed["sha256"] = sha256(BASE_DISK)
        except OSError as error:
            hashed["error"] = error
    # A daemon, so a boot that fails, or a person's Ctrl-C, does not wait
    # for the rest of the hash.
    hashing = threading.Thread(target=hash_image, daemon=True)
    hashing.start()
    to_login = boot()
    hashing.join()
    if "error" in hashed:
        stop()
        raise SystemExit(f"Could not hash {BASE_DISK}: {hashed['error']}")
    lines = [f"CrowbarOS base boot check, {stamp}, commit {head}",
             f"disk sha256 {hashed['sha256']}", f"login prompt after {to_login:.1f}s", ""]
    failed = 0
    for name, user, command, ok in boot_checks():
        result = ssh(command, user=user)
        passed = result.returncode == 0 and ok(result.stdout)
        failed += not passed
        lines.append(f"{'PASS' if passed else 'FAIL'}: {name}")
        lines += [f"    $ {l}" for l in command.splitlines()] + [f"    {l}" for l in result.stdout.strip().splitlines()]
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
    root_parser = sub.add_parser("root", help="pack the finished Void root into an archive, "
                                              "or unpack one in its place, as CI's cache does")
    root_parser.add_argument("action", choices=["save", "restore", "key"])
    root_parser.add_argument("archive", type=Path, nargs="?")
    args = parser.parse_args()
    if args.cmd == "root" and (args.archive is None) != (args.action == "key"):
        parser.error("`root save` and `root restore` take an archive, and `root key` takes none")
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
    elif args.cmd == "root" and args.action == "key":
        print(root_key())
    elif args.cmd == "root":
        save_root(args.archive) if args.action == "save" else restore_root(args.archive)


if __name__ == "__main__":
    main()
