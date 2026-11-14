#!/usr/bin/env python3
"""Repository infrastructure automation for the local Fedora lab (stdlib only)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time
import uuid
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
INFRA = ROOT / "infra/vm"
STATE = ROOT / ".local/vms"
CFG = json.loads((INFRA / "config.json").read_text())
if (STATE / "config.local.json").exists():
    CFG.update(json.loads((STATE / "config.local.json").read_text()))
URI = CFG["libvirt_uri"]
QEMU = "http://libvirt.org/schemas/domain/qemu/1.0"
ET.register_namespace("qemu", QEMU)


def run(*args, capture=False, **kwargs):
    if capture:
        kwargs["stdout"] = subprocess.PIPE
    return subprocess.run([str(a) for a in args], check=True, text=True,
                          **kwargs).stdout


def virsh(*args, **kwargs):
    return run("virsh", "-c", URI, *args, **kwargs)


def guest(role):
    return CFG["guests"][role], STATE / role


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(url, path):
    part = path.with_name(path.name + ".part")
    run("curl", "-fL", "--retry", "3", "--connect-timeout", "20", "-o", part, url)
    part.replace(path)


def download():
    cache = STATE / "cache"
    cache.mkdir(parents=True, exist_ok=True)
    (cache / "gnupg").mkdir(mode=0o700, exist_ok=True)
    for name, url in ((CFG["checksum"], CFG["image_directory"] + "/" + CFG["checksum"]),
                      ("fedora.gpg", "https://fedoraproject.org/fedora.gpg")):
        if not (cache / name).exists():
            fetch(url, cache / name)
    status = run("gpgv", "--homedir", cache / "gnupg", "--keyring", cache / "fedora.gpg",
                 "--status-fd", "1", cache / CFG["checksum"], capture=True)
    if not re.search(r"\[GNUPG:\] VALIDSIG " + CFG["signing_fingerprint"] + r"\s", status):
        raise RuntimeError("Checksum signature does not match the pinned Fedora signing key")
    expected = f'SHA256 ({CFG["image"]}) = {CFG["image_sha256"]}'
    if expected not in (cache / CFG["checksum"]).read_text().splitlines():
        raise RuntimeError("Pinned digest does not match the signed checksum manifest")
    image = cache / CFG["image"]
    part = image.with_name(image.name + ".part")
    if not image.exists() and part.exists() and digest(part) == CFG["image_sha256"]:
        part.replace(image)
    if not image.exists():
        fetch(CFG["image_directory"] + "/" + CFG["image"], image)
    if digest(image) != CFG["image_sha256"]:
        raise RuntimeError("Image SHA-256 mismatch; no VM will be created")
    image.chmod(0o444)
    print(f"Verified Fedora {CFG['fedora_release']} image and signing fingerprint.")


def ssh_args(role):
    info, folder = guest(role)
    return ["ssh", "-i", str(STATE / "id_ed25519"), "-p", str(info["ssh_port"]),
            "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={STATE / 'known_hosts'}",
            "-o", "GlobalKnownHostsFile=/dev/null", "crowbar@127.0.0.1"]


def write_ssh_config():
    sections = []
    for info in CFG["guests"].values():
        sections.append(f'''Host {info["name"]}
    HostName 127.0.0.1
    Port {info["ssh_port"]}
    User crowbar
    IdentityFile {json.dumps(str(STATE / "id_ed25519"))}
    UserKnownHostsFile {json.dumps(str(STATE / "known_hosts"))}
    GlobalKnownHostsFile /dev/null
    StrictHostKeyChecking yes
    IdentitiesOnly yes
    ForwardAgent no
''')
    (STATE / "ssh_config").write_text("\n".join(sections))


def ssh(role, command, **kwargs):
    return run(*ssh_args(role), shlex.join(command), **kwargs)


def domain_xml(role):
    info, folder = guest(role)
    root = ET.Element("domain", type="kvm")
    def add(parent, tag, text=None, **attrs):
        e = ET.SubElement(parent, tag, attrs)
        e.text = text
        return e
    add(root, "name", info["name"])
    add(root, "uuid", (folder / "uuid").read_text().strip())
    add(root, "title", f"CrowbarOS Fedora {CFG['fedora_release']} {role} lab")
    add(root, "memory", str(info["memory_mib"]), unit="MiB")
    add(root, "vcpu", str(info["vcpus"]), placement="static")
    backing = add(root, "memoryBacking")
    add(backing, "source", type="memfd")
    add(backing, "access", mode="shared")
    boot = add(root, "os")
    add(boot, "type", "hvm", arch="x86_64", machine="q35")
    add(boot, "loader", CFG["firmware_code"], readonly="yes", type="pflash", secure="no")
    add(boot, "nvram", str(folder / "nvram.fd"))
    add(boot, "boot", dev="hd")
    features = add(root, "features")
    add(features, "acpi")
    add(features, "apic")
    add(root, "cpu", mode="host-passthrough", check="none", migratable="off")
    add(root, "clock", offset="utc")
    add(root, "on_poweroff", "destroy")
    add(root, "on_reboot", "restart")
    add(root, "on_crash", "destroy")
    devices = add(root, "devices")
    add(devices, "emulator", shutil.which("qemu-system-x86_64"))
    for filename, dev, fmt, bus, dtype in (("disk.qcow2", "vda", "qcow2", "virtio", "disk"),
                                         ("seed.iso", "sda", "raw", "sata", "cdrom")):
        disk = add(devices, "disk", type="file", device=dtype)
        add(disk, "driver", name="qemu", type=fmt)
        add(disk, "source", file=str(folder / filename))
        add(disk, "target", dev=dev, bus=bus)
        if dtype == "cdrom":
            add(disk, "readonly")
    net = add(devices, "interface", type="user")
    add(net, "mac", address="52:54:00:cb:45:" + ("10" if role == "build" else "11"))
    add(net, "model", type="virtio")
    add(net, "backend", type="passt")
    port = add(net, "portForward", proto="tcp", address="127.0.0.1")
    add(port, "range", start=str(info["ssh_port"]), to="22")
    serial = add(devices, "serial", type="pty")
    add(serial, "target", port="0")
    console = add(devices, "console", type="pty")
    add(console, "target", type="serial", port="0")
    channel = add(devices, "channel", type="unix")
    add(channel, "target", type="virtio", name="org.qemu.guest_agent.0")
    graphics = add(devices, "graphics", type="spice", autoport="no")
    add(graphics, "listen", type="socket", socket=str(folder / "spice.sock"))
    add(graphics, "clipboard", copypaste="no")
    add(graphics, "filetransfer", enable="no")
    video = add(devices, "video")
    model = add(video, "model", type="virtio", heads="1", primary="yes")
    if role == "test":
        render = str(Path(CFG["render_node"]).resolve(strict=True))
        add(graphics, "gl", enable="yes", rendernode=render)
        add(model, "acceleration", accel3d="yes")
    if role == "test" and (CFG.get("venus_enabled", False) or CFG.get("drm_native_context_enabled", False)):
        model.set("blob", "on")
        # libvirt exposes blob/3D; hostmem and Vulkan modes need QEMU properties.
        override = add(root, f"{{{QEMU}}}override")
        device = add(override, f"{{{QEMU}}}device", alias="video0")
        frontend = add(device, f"{{{QEMU}}}frontend")
        add(frontend, f"{{{QEMU}}}property", name="hostmem", type="unsigned", value="1073741824")
        if CFG.get("venus_enabled", False):
            add(frontend, f"{{{QEMU}}}property", name="venus", type="bool", value="true")
        if CFG.get("drm_native_context_enabled", False):
            add(frontend, f"{{{QEMU}}}property", name="drm_native_context", type="bool", value="true")
        commandline = add(root, f"{{{QEMU}}}commandline")
        add(commandline, f"{{{QEMU}}}env", name="VK_DRIVER_FILES", value=CFG["vulkan_icd"])
    add(devices, "input", type="tablet", bus="usb")
    add(devices, "input", type="keyboard", bus="usb")
    rng = add(devices, "rng", model="virtio")
    add(rng, "backend", "/dev/urandom", model="random")
    add(devices, "memballoon", model="virtio")
    ET.indent(root)
    return ET.tostring(root, encoding="unicode") + "\n"


def create(role):
    info, folder = guest(role)
    names = virsh("list", "--all", "--name", capture=True).splitlines()
    if info["name"] in names:
        print(f"{info['name']} already exists; leaving its disk and definition unchanged.")
        return
    if (folder / "disk.qcow2").exists():
        raise RuntimeError(f"Unregistered disk exists at {folder}; inspect it before recreating")
    folder.mkdir(parents=True, mode=0o700, exist_ok=True)
    (folder / "uuid").write_text(str(uuid.uuid4()) + "\n")
    if not (STATE / "id_ed25519").exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "crowbaros-local-lab",
            "-f", STATE / "id_ed25519")
    hostkey = folder / "ssh_host_ed25519_key"
    run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", info["name"], "-f", hostkey)
    seed = folder / "seed"
    seed.mkdir(mode=0o700, exist_ok=True)
    config = {
        # Fedora 45's early cloud-init hostnamectl can run before the system bus.
        # Write the persistent name, then set the live name in the final stage.
        "preserve_hostname": True,
        "write_files": [{"path": "/etc/hostname", "content": info["name"] + "\n"}],
        "users": [{"name": "crowbar", "shell": "/bin/bash", "groups": ["wheel"],
                   "sudo": ["ALL=(ALL) NOPASSWD:ALL"], "lock_passwd": True,
                   "ssh_authorized_keys": [(STATE / "id_ed25519.pub").read_text().strip()]}],
        "disable_root": True, "ssh_pwauth": False,
        "ssh_keys": {"ed25519_private": hostkey.read_text(),
                     "ed25519_public": hostkey.with_suffix(".pub").read_text()},
        "growpart": {"mode": "auto", "devices": ["/"]}, "resize_rootfs": True,
        "packages": ["qemu-guest-agent", "rsync", "jq"],
        "runcmd": [["hostnamectl", "set-hostname", info["name"]],
                   ["systemctl", "enable", "--now", "qemu-guest-agent"]],
    }
    (seed / "user-data").write_text("#cloud-config\n" + json.dumps(config, indent=2) + "\n")
    (seed / "meta-data").write_text(json.dumps({"instance-id": str(uuid.uuid4())}))
    run("xorriso", "-as", "mkisofs", "-quiet", "-output", folder / "seed.iso",
        "-volid", "cidata", "-joliet", "-rock", seed / "user-data", seed / "meta-data")
    run("qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b",
        STATE / "cache" / CFG["image"], folder / "disk.qcow2", f"{info['disk_gib']}G")
    shutil.copyfile(CFG["firmware_vars"], folder / "nvram.fd")
    (folder / "domain.xml").write_text(domain_xml(role))
    pub = hostkey.with_suffix(".pub").read_text().split()
    known_path = STATE / "known_hosts"
    label = f"[127.0.0.1]:{info['ssh_port']}"
    lines = known_path.read_text().splitlines() if known_path.exists() else []
    lines = [line for line in lines if not line.startswith(label + " ")]
    known_path.write_text("\n".join(lines + [f"{label} {pub[0]} {pub[1]}"]) + "\n")
    virsh("define", folder / "domain.xml", "--validate")


def wait(role):
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        try:
            ssh(role, ["true"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except subprocess.CalledProcessError:
            time.sleep(3)
            continue
        ssh(role, ["sudo", "cloud-init", "status", "--wait"])
        print(f"{role}: SSH and cloud-init ready")
        return
    raise RuntimeError("Guest did not become ready; inspect virsh console and cloud-init logs")


def stop(role):
    info, _ = guest(role)
    if virsh("domstate", info["name"], capture=True).strip() == "shut off":
        return
    virsh("shutdown", info["name"])
    for _ in range(60):
        if virsh("domstate", info["name"], capture=True).strip() == "shut off":
            return
        time.sleep(1)
    raise RuntimeError("Guest has not shut down; refusing an automatic hard power-off")


def require_stopped(role):
    info, _ = guest(role)
    if virsh("domstate", info["name"], capture=True).strip() != "shut off":
        raise RuntimeError("Stop the guest before modifying its disk or taking a baseline")


def checkpoint(role, name, restore=False):
    if not re.fullmatch(r"[a-zA-Z0-9_-]+", name):
        raise RuntimeError("Checkpoint names may contain letters, numbers, underscores, and dashes")
    require_stopped(role)
    _, folder = guest(role)
    saved = folder / "checkpoints" / name
    if restore:
        if not (saved / "disk.qcow2").exists():
            raise RuntimeError("Checkpoint not found")
        # Preserve the outgoing disk/NVRAM together instead of deleting work.
        backup = folder / "checkpoints" / ("before-restore-" + str(time.time_ns()))
        backup.mkdir(parents=True)
        for filename in ("disk.qcow2", "nvram.fd"):
            (folder / filename).replace(backup / filename)
            run("cp", "--reflink=auto", "--sparse=always", saved / filename, folder / filename)
        print(f"Restored {name}; outgoing state retained at {backup}")
    else:
        saved.mkdir(parents=True, exist_ok=False)
        for filename in ("disk.qcow2", "nvram.fd", "domain.xml"):
            run("cp", "--reflink=auto", "--sparse=always", folder / filename, saved / filename)
        print(f"Saved offline disk/NVRAM checkpoint {name}")


def record(role):
    _, folder = guest(role)
    report = folder / "reports"
    report.mkdir(exist_ok=True)
    for name, cmd in {
        "packages.tsv": ["rpm", "-qa", "--qf", "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\n"],
        "system.txt": ["bash", "-lc", "cat /etc/os-release; uname -a; getenforce; systemctl --failed --no-pager; df -h /"],
        "repositories.txt": ["dnf", "repolist", "--enabled"],
    }.items():
        output = ssh(role, cmd, capture=True)
        if name == "packages.tsv":
            output = "\n".join(sorted(output.splitlines())) + "\n"
        (report / name).write_text(output)
    (report / "domain.xml").write_text(virsh("dumpxml", CFG["guests"][role]["name"], "--inactive", capture=True))
    print(f"Saved installed package versions and VM definition under {report}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["doctor", "download", "create", "start", "stop", "status",
        "wait", "ssh", "console", "view", "provision", "snapshot", "restore", "record", "smoke",
        "configure", "deploy-smoke", "gpui-build", "gpui-deploy", "gpui-run", "gpu-build", "gpu-deploy", "studio-build", "studio-deploy", "studio-run", "shell-build", "shell-deploy", "shell-run"])
    parser.add_argument("role", nargs="?", choices=["build", "test", "all"])
    parser.add_argument("args", nargs=argparse.REMAINDER)
    opts = parser.parse_args()
    STATE.mkdir(mode=0o700, parents=True, exist_ok=True)
    STATE.chmod(0o700)
    os.umask(0o077)
    if opts.action == "doctor":
        for tool in ("virsh", "qemu-img", "qemu-system-x86_64", "xorriso", "curl", "gpgv", "ssh", "ssh-keygen", "virt-viewer"):
            if not shutil.which(tool):
                raise RuntimeError(f"Missing host tool: {tool}")
        if not os.access("/dev/kvm", os.R_OK | os.W_OK):
            raise RuntimeError("The current user cannot access /dev/kvm (or the execution sandbox hides it)")
        for key in ("firmware_code", "firmware_vars", "render_node", "vulkan_icd"):
            if not Path(CFG[key]).exists():
                raise RuntimeError(f"Set {key} in .local/vms/config.local.json for this host")
        virsh("list", "--all")
        print("Host prerequisites available; Fedora image: " + CFG["image"])
        return
    if opts.action == "download":
        download()
        return
    if opts.action == "status":
        virsh("list", "--all")
        return
    if not opts.role:
        parser.error("This action needs build, test, or all")
    roles = ["build", "test"] if opts.role == "all" else [opts.role]
    if opts.role == "all" and opts.action in ("ssh", "console", "view", "smoke", "deploy-smoke", "gpui-build", "gpui-deploy", "gpui-run", "gpu-build", "gpu-deploy", "studio-build", "studio-deploy", "studio-run", "shell-build", "shell-deploy", "shell-run"):
        parser.error("This action needs one guest")
    if opts.action == "create":
        download()
        write_ssh_config()
    for role in roles:
        info, folder = guest(role)
        if opts.action == "create":
            create(role)
        elif opts.action == "start":
            if virsh("domstate", info["name"], capture=True).strip() != "running":
                virsh("start", info["name"])
        elif opts.action == "stop":
            stop(role)
        elif opts.action == "configure":
            require_stopped(role)
            (folder / "domain.xml").write_text(domain_xml(role))
            virsh("define", folder / "domain.xml", "--validate")
        elif opts.action == "wait":
            wait(role)
        elif opts.action == "ssh":
            if opts.args:
                ssh(role, opts.args)
            else:
                run(*ssh_args(role)[:-1], "-t", ssh_args(role)[-1])
        elif opts.action == "console":
            virsh("console", info["name"])
        elif opts.action == "view":
            run("virt-viewer", "--connect", URI, "--attach", info["name"])
        elif opts.action == "provision":
            with (INFRA / "guest" / (role + ".sh")).open() as source:
                ssh(role, ["sudo", "bash", "-se"], stdin=source)
            record(role)
        elif opts.action in ("snapshot", "restore"):
            if len(opts.args) != 1:
                parser.error("Supply exactly one checkpoint name")
            checkpoint(role, opts.args[0], restore=opts.action == "restore")
        elif opts.action == "record":
            record(role)
        elif opts.action == "smoke":
            if role != "build":
                parser.error("Use smoke build")
            ssh(role, ["mkdir", "-p", "/home/crowbar/src/lab"])
            archive = subprocess.Popen(["tar", "-C", str(INFRA), "-cf", "-", "fixtures", "guest/smoke-build.sh"], stdout=subprocess.PIPE)
            try:
                ssh(role, ["tar", "-C", "/home/crowbar/src/lab", "-xf", "-"], stdin=archive.stdout)
            finally:
                archive.stdout.close()
                if archive.wait() != 0:
                    raise RuntimeError("Source archive transfer failed")
            ssh(role, ["bash", "/home/crowbar/src/lab/guest/smoke-build.sh"])
            with (STATE / "smoke-repo.tar.gz").open("wb") as artifact:
                ssh(role, ["cat", "/home/crowbar/artifacts/smoke-repo.tar.gz"], stdout=artifact)
            print("Signed development repository copied to .local/vms/smoke-repo.tar.gz")
        elif opts.action == "deploy-smoke":
            if role != "test":
                parser.error("Use deploy-smoke test")
            ssh(role, ["mkdir", "-p", "/home/crowbar/incoming"])
            with (STATE / "smoke-repo.tar.gz").open("rb") as artifact:
                ssh(role, ["tar", "-C", "/home/crowbar/incoming", "-xzf", "-"], stdin=artifact)
            with (INFRA / "guest/deploy-smoke.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se"], stdin=script)
        elif opts.action == "gpui-build":
            if role != "build":
                parser.error("Use gpui-build build")
            ssh(role, ["mkdir", "-p", "/home/crowbar/src/gpui-probe"])
            archive = subprocess.Popen([
                "tar", "--exclude=./target", "--exclude=./vendor", "--exclude=./.cargo",
                "--exclude=./dependency-licenses", "--exclude=./THIRD_PARTY.json",
                "--exclude=./BUILD-MANIFEST.txt", "-C", str(ROOT / "apps/gpui-probe"),
                "-cf", "-", "."], stdout=subprocess.PIPE)
            try:
                ssh(role, ["tar", "-C", "/home/crowbar/src/gpui-probe", "-xf", "-"], stdin=archive.stdout)
            finally:
                archive.stdout.close()
                if archive.wait() != 0:
                    raise RuntimeError("GPUI source transfer failed")
            with (INFRA / "guest/gpui-build.sh").open() as script:
                ssh(role, ["bash", "-se"], stdin=script)
            with (STATE / "gpui-repo.tar.gz").open("wb") as artifact:
                ssh(role, ["cat", "/home/crowbar/artifacts/gpui-repo.tar.gz"], stdout=artifact)
            print("Signed GPUI repository copied to .local/vms/gpui-repo.tar.gz")
        elif opts.action == "gpui-deploy":
            if role != "test":
                parser.error("Use gpui-deploy test")
            ssh(role, ["mkdir", "-p", "/home/crowbar/incoming-gpui"])
            with (STATE / "gpui-repo.tar.gz").open("rb") as artifact:
                ssh(role, ["tar", "-C", "/home/crowbar/incoming-gpui", "-xzf", "-"], stdin=artifact)
            with (INFRA / "guest/gpui-deploy.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se"], stdin=script)
        elif opts.action == "gpui-run":
            if role != "test":
                parser.error("Use gpui-run test")
            if len(opts.args) > 1:
                parser.error("Supply at most one Vulkan mode: system, intel-virtio, or software")
            mode = opts.args[0] if opts.args else CFG.get("gpui_vulkan_driver", "system")
            if mode not in ("system", "intel-virtio", "software"):
                parser.error("Expected Vulkan mode: system, intel-virtio, or software")
            with (INFRA / "guest/gpui-run.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se", "--", mode], stdin=script)
        elif opts.action == "studio-build":
            if role != "build":
                parser.error("Use studio-build build")
            ssh(role, ["mkdir", "-p", "/home/crowbar/src/studio-settings"])
            archive = subprocess.Popen([
                "tar", "--exclude=./target", "--exclude=./vendor", "--exclude=./.cargo",
                "--exclude=./dependency-licenses", "--exclude=./THIRD_PARTY.json",
                "--exclude=./BUILD-MANIFEST.txt", "-C", str(ROOT / "apps/studio-settings"),
                "-cf", "-", "."], stdout=subprocess.PIPE)
            try:
                ssh(role, ["tar", "-C", "/home/crowbar/src/studio-settings", "-xf", "-"], stdin=archive.stdout)
            finally:
                archive.stdout.close()
                if archive.wait() != 0:
                    raise RuntimeError("Studio Settings source transfer failed")
            with (INFRA / "guest/studio-build.sh").open() as script:
                ssh(role, ["bash", "-se"], stdin=script)
            with (STATE / "studio-repo.tar.gz").open("wb") as artifact:
                ssh(role, ["cat", "/home/crowbar/artifacts/studio-repo.tar.gz"], stdout=artifact)
            print("Signed Studio Settings repository copied to .local/vms/studio-repo.tar.gz")
        elif opts.action == "studio-deploy":
            if role != "test":
                parser.error("Use studio-deploy test")
            ssh(role, ["mkdir", "-p", "/home/crowbar/incoming-studio"])
            with (STATE / "studio-repo.tar.gz").open("rb") as artifact:
                ssh(role, ["tar", "-C", "/home/crowbar/incoming-studio", "-xzf", "-"], stdin=artifact)
            with (INFRA / "guest/studio-deploy.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se"], stdin=script)
        elif opts.action == "studio-run":
            if role != "test":
                parser.error("Use studio-run test")
            if len(opts.args) > 1:
                parser.error("Supply at most one Vulkan mode: system, intel-virtio, or software")
            mode = opts.args[0] if opts.args else CFG.get("gpui_vulkan_driver", "system")
            if mode not in ("system", "intel-virtio", "software"):
                parser.error("Expected Vulkan mode: system, intel-virtio, or software")
            with (INFRA / "guest/studio-run.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se", "--", mode], stdin=script)
        elif opts.action == "shell-build":
            if role != "build":
                parser.error("Use shell-build build")
            ssh(role, ["mkdir", "-p", "/home/crowbar/src/shell"])
            archive = subprocess.Popen([
                "tar", "--exclude=./target", "--exclude=./vendor", "--exclude=./.cargo",
                "--exclude=./dependency-licenses", "--exclude=./THIRD_PARTY.json",
                "--exclude=./BUILD-MANIFEST.txt", "--exclude=./.upstream-cache", "--exclude=./gpui-source", "-C", str(ROOT / "apps/shell"),
                "-cf", "-", "."], stdout=subprocess.PIPE)
            try:
                ssh(role, ["tar", "-C", "/home/crowbar/src/shell", "-xf", "-"], stdin=archive.stdout)
            finally:
                archive.stdout.close()
                if archive.wait() != 0:
                    raise RuntimeError("Desktop shell source transfer failed")
            with (INFRA / "guest/shell-build.sh").open() as script:
                ssh(role, ["bash", "-se"], stdin=script)
            with (STATE / "shell-repo.tar.gz").open("wb") as artifact:
                ssh(role, ["cat", "/home/crowbar/artifacts/shell-repo.tar.gz"], stdout=artifact)
            print("Signed Desktop shell repository copied to .local/vms/shell-repo.tar.gz")
        elif opts.action == "shell-deploy":
            if role != "test":
                parser.error("Use shell-deploy test")
            ssh(role, ["mkdir", "-p", "/home/crowbar/incoming-shell"])
            with (STATE / "shell-repo.tar.gz").open("rb") as artifact:
                ssh(role, ["tar", "-C", "/home/crowbar/incoming-shell", "-xzf", "-"], stdin=artifact)
            with (INFRA / "guest/shell-deploy.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se"], stdin=script)
        elif opts.action == "shell-run":
            if role != "test":
                parser.error("Use shell-run test")
            if len(opts.args) > 1:
                parser.error("Supply at most one Vulkan mode: system, intel-virtio, or software")
            mode = opts.args[0] if opts.args else CFG.get("gpui_vulkan_driver", "system")
            if mode not in ("system", "intel-virtio", "software"):
                parser.error("Expected Vulkan mode: system, intel-virtio, or software")
            with (INFRA / "guest/shell-run.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se", "--", mode], stdin=script)
        elif opts.action == "gpu-build":
            if role != "build":
                parser.error("Use gpu-build build")
            ssh(role, ["mkdir", "-p", "/home/crowbar/src/gpu"])
            archive = subprocess.Popen([
                "tar", "-C", str(INFRA), "-cf", "-", "gpu", "guest/gpu-build.sh"
            ], stdout=subprocess.PIPE)
            try:
                ssh(role, ["tar", "-C", "/home/crowbar/src/gpu", "-xf", "-"], stdin=archive.stdout)
            finally:
                archive.stdout.close()
                if archive.wait() != 0:
                    raise RuntimeError("GPU recipe transfer failed")
            ssh(role, ["bash", "/home/crowbar/src/gpu/guest/gpu-build.sh"])
            with (STATE / "gpu-repo.tar.gz").open("wb") as artifact:
                ssh(role, ["cat", "/home/crowbar/artifacts/gpu-repo.tar.gz"], stdout=artifact)
            print("Signed GPU repository copied to .local/vms/gpu-repo.tar.gz")
        elif opts.action == "gpu-deploy":
            if role != "test":
                parser.error("Use gpu-deploy test")
            ssh(role, ["mkdir", "-p", "/home/crowbar/incoming-gpu"])
            with (STATE / "gpu-repo.tar.gz").open("rb") as artifact:
                ssh(role, ["tar", "-C", "/home/crowbar/incoming-gpu", "-xzf", "-"], stdin=artifact)
            with (INFRA / "guest/gpu-deploy.sh").open() as script:
                ssh(role, ["sudo", "bash", "-se"], stdin=script)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError, OSError) as error:
        print(f"lab: {error}", file=sys.stderr)
        sys.exit(1)
