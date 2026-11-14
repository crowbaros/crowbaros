#!/usr/bin/env python3
"""Exercise the installed GPUI probe through the test VM's input devices.

Run with its viewer open. This restarts only the probe, moves/resizes its window,
and drives its controls. Evidence is written under .local/gpui/validation/.
"""
import argparse
import json
from pathlib import Path
import subprocess
import time

import lab


OUT = lab.ROOT / ".local/gpui/validation"
DESKTOP = ["sudo", "-u", "desktop", "env", "XDG_RUNTIME_DIR=/run/user/1001",
           "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus"]


def guest(*args, capture=True):
    return lab.ssh("test", list(args), capture=capture)


def hypr(*args):
    return guest(*DESKTOP, "hyprctl", "-i", "0", *args)


def key(*codes):
    lab.virsh("send-key", "crowbaros-test", "--codeset", "linux", "--holdtime", "100",
              *map(str, codes), capture=True)
    time.sleep(0.4)


def events(items):
    lab.virsh("qemu-monitor-command", "crowbaros-test", json.dumps({
        "execute": "input-send-event", "arguments": {"events": items}}), capture=True)


def pointer(x, y, width, height):
    events([{"type": "abs", "data": {"axis": axis, "value": round(value / extent * 32767)}}
            for axis, value, extent in [("x", x, width - 1), ("y", y, height - 1)]])
    time.sleep(0.3)


def button(name):
    for down in (True, False):
        events([{"type": "btn", "data": {"button": name, "down": down}}])
        time.sleep(0.15)
    time.sleep(0.3)


def capture(name):
    guest(*DESKTOP, "env", "WAYLAND_DISPLAY=wayland-1", "grim", "/home/desktop/gpui-check.png")
    with (OUT / f"{name}.png").open("wb") as target:
        lab.ssh("test", ["sudo", "cat", "/home/desktop/gpui-check.png"], stdout=target)


def window():
    return next(c for c in json.loads(hypr("clients", "-j"))
                if c["class"] == "com.crowbaros.GpuiProbe")


def expect(last_event):
    logs = guest("sudo", "journalctl", "-b", f"_SYSTEMD_INVOCATION_ID={invocation}",
                 "--no-pager", "--grep=GPUI_PROBE")
    (OUT / "input-journal.txt").write_text(logs)
    lines = [line for line in logs.splitlines() if "GPUI_PROBE" in line]
    assert lines and last_event in lines[-1], logs
    print("PASS", last_event, flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vulkan-driver", choices=["system", "intel-virtio", "software"])
    parser.add_argument("--output", type=Path, default=OUT)
    args = parser.parse_args()
    OUT = args.output
    OUT.mkdir(parents=True, exist_ok=True)
    state = guest(*DESKTOP, "systemctl", "--user", "show", "crowbaros-gpui-probe.service",
                  "-p", "LoadState", "--value").strip()
    if state == "loaded":
        guest(*DESKTOP, "systemctl", "--user", "stop", "crowbaros-gpui-probe.service")
    time.sleep(0.5)
    command = ["python3", str(lab.INFRA / "lab.py"), "gpui-run", "test"]
    if args.vulkan_driver:
        command.append(args.vulkan_driver)
    subprocess.run(command, check=True)
    time.sleep(2)
    invocation = guest(*DESKTOP, "systemctl", "--user", "show", "crowbaros-gpui-probe.service",
                       "-p", "InvocationID", "--value").strip()
    assert invocation
    if args.vulkan_driver == "intel-virtio":
        pid = guest(*DESKTOP, "systemctl", "--user", "show", "crowbaros-gpui-probe.service",
                    "-p", "MainPID", "--value").strip()
        maps = guest("sudo", "cat", f"/proc/{pid}/maps")
        assert "/opt/crowbaros-lab/mesa-intel-virtio/lib64/libvulkan_intel.so" in maps
        assert "libvulkan_lvp" not in maps
        libraries = sorted({line.split()[-1] for line in maps.splitlines() if "vulkan" in line})
        (OUT / "vulkan-libraries.txt").write_text("\n".join(libraries) + "\n")
        print("PASS isolated Intel VirtIO Vulkan driver loaded; no lavapipe", flush=True)
    w = window()
    assert not w["xwayland"]
    assert w["title"] == "CrowbarOS · GPUI Probe"
    hypr("eval", 'hl.dispatch(hl.dsp.focus({window="class:com.crowbaros.GpuiProbe"}))')
    if not w["floating"]:
        key(56, 47)  # Lab Alt+V binding.
    hypr("eval", 'hl.dispatch(hl.dsp.window.resize({x=820,y=700,relative=false,window="class:com.crowbaros.GpuiProbe"})); '
                 'hl.dispatch(hl.dsp.window.move({x=230,y=50,relative=false,window="class:com.crowbaros.GpuiProbe"}))')
    time.sleep(0.5)
    monitor = json.loads(hypr("monitors", "-j"))[0]
    assert monitor["scale"] == 1 and monitor["width"] >= 1100 and monitor["height"] >= 760
    width, height = monitor["width"], monitor["height"]
    capture("initial")
    pointer(310, 395, width, height)
    button("left")
    expect("event=increment count=1 studio=false")
    key(29, 23)  # Ctrl+I
    expect("event=increment count=2 studio=false")
    key(15)  # Tab to palette button.
    expect("event=focus-next count=2 studio=false")
    capture("keyboard-focus")
    key(28)  # Enter activates exactly once.
    expect("event=palette count=2 studio=true")
    pointer(800, 600, width, height)
    for _ in range(8):
        button("wheel-down")
    expect("event=scroll")
    capture("studio-scrolled")
    key(42, 15)  # Shift+Tab back to Increment.
    expect("event=focus-previous count=2 studio=true")
    key(57)  # Space activates exactly once.
    expect("event=increment count=3 studio=true")
    key(29, 19)  # Ctrl+R
    expect("event=reset count=0 studio=true")
    key(29, 20)  # Ctrl+T
    expect("event=palette count=0 studio=false")
    hypr("eval", 'hl.dispatch(hl.dsp.window.resize({x=640,y=600,relative=false,window="class:com.crowbaros.GpuiProbe"}))')
    time.sleep(0.5)
    assert window()["size"] == [640, 600]
    capture("minimum-size")
    (OUT / "window.json").write_text(json.dumps(window(), indent=2) + "\n")
    key(29, 16)  # Ctrl+Q
    time.sleep(0.5)
    assert not any(c["class"] == "com.crowbaros.GpuiProbe" for c in json.loads(hypr("clients", "-j")))
    (OUT / "journal.txt").write_text(guest("sudo", "journalctl", "-b",
        f"_SYSTEMD_INVOCATION_ID={invocation}", "--no-pager"))
    print("PASS native Wayland, title, resize to 640×600, and keyboard quit; inspect captured images.")
