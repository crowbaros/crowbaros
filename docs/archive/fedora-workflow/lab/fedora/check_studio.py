#!/usr/bin/env python3
"""Exercise Studio Settings in the disposable test VM with its viewer open.

Changes this lab user's appearance preferences. Start from a saved checkpoint;
do not run during manual exploration. Finishes with Dark, Studio off, motion reduced.
"""
import json
from pathlib import Path
import subprocess
import time

import lab
import check_gpui as inputs

OUT = lab.ROOT / ".local/studio/validation"
DESKTOP = inputs.DESKTOP
APP = "com.crowbaros.StudioSettings"
UNIT = "crowbaros-studio-settings.service"


def guest(*args):
    return lab.ssh("test", list(args), capture=True)


def hypr(*args):
    return guest(*DESKTOP, "hyprctl", "-i", "0", *args)


def windows():
    return [w for w in json.loads(hypr("clients", "-j")) if w["class"] == APP]


def key(*codes):
    inputs.key(*codes)


def prefs():
    return json.loads(guest(*DESKTOP, "/usr/bin/crowbaros-studio-settings", "--print-settings"))


def expect(theme, studio, reduced):
    expected = {"schema_version": 1, "theme": theme, "studio": studio, "reduced_motion": reduced}
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        if prefs() == expected:
            break
        time.sleep(.2)
    assert prefs() == expected, (prefs(), expected)
    assert json.loads(hypr("getoption", "animations:enabled", "-j"))["bool"] == (not reduced)
    print("PASS saved preferences and live motion:", expected, flush=True)


def capture(name):
    guest(*DESKTOP, "env", "WAYLAND_DISPLAY=wayland-1", "grim", "/home/desktop/studio-check.png")
    with (OUT / f"{name}.png").open("wb") as f:
        lab.ssh("test", ["sudo", "cat", "/home/desktop/studio-check.png"], stdout=f)


def launch():
    subprocess.run(["python3", str(lab.INFRA / "lab.py"), "studio-run", "test", "intel-virtio"], check=True)
    time.sleep(2)
    window = windows()[0]
    assert not window["xwayland"]
    assert window["title"] == "Studio Settings"
    hypr("eval", f'hl.dispatch(hl.dsp.focus({{window="class:{APP}"}}))')
    if not window["floating"]:
        key(56, 47)
    resize(980, 720)
    pid = guest(*DESKTOP, "systemctl", "--user", "show", UNIT, "-p", "MainPID", "--value").strip()
    maps = guest("sudo", "cat", f"/proc/{pid}/maps")
    assert "/opt/crowbaros-lab/mesa-intel-virtio/lib64/libvulkan_intel.so" in maps
    assert "libvulkan_lvp" not in maps
    print("PASS native Wayland and hardware Vulkan", flush=True)


def resize(width, height):
    hypr("eval", f'hl.dispatch(hl.dsp.window.resize({{x={width},y={height},relative=false,window="class:{APP}"}})); '
         f'hl.dispatch(hl.dsp.window.move({{x=150,y=40,relative=false,window="class:{APP}"}}))')
    time.sleep(.8)
    assert windows()[0]["floating"], "Resize check requires a floating window"
    assert windows()[0]["size"] == [width, height], windows()[0]


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    if windows():
        guest(*DESKTOP, "systemctl", "--user", "stop", UNIT)
        time.sleep(.5)
    launch()
    # Establish a known baseline through the UI, including after manual exploration.
    original = prefs()
    key(57)  # Dark is initially focused.
    key(15); key(15); key(15)  # Studio
    if original["studio"]:
        key(57)
    key(15)  # Reduced motion
    if not original["reduced_motion"]:
        key(57)
    key(29, 31)
    expect("dark", False, True)
    key(29, 16)
    launch()
    capture("dark")
    key(15)  # Dark -> Light
    key(57)  # Space, one activation
    capture("light-preview")
    key(29, 31)  # Ctrl+S
    expect("light", False, True)
    key(15)  # Warm Paper
    key(28)  # Enter
    capture("paper-preview")
    assert prefs()["theme"] == "light", "Preview must not write preferences"
    key(29, 44)  # Ctrl+Z
    assert prefs()["theme"] == "light"
    key(15)  # Studio toggle
    key(57)
    key(29, 31)
    expect("light", True, True)
    capture("studio")
    key(15)  # Reduced motion
    key(28)
    assert not json.loads(hypr("getoption", "animations:enabled", "-j"))["bool"], "Motion changed before Save"
    key(29, 31)
    expect("light", True, False)
    key(29, 16)  # Quit saved app
    assert not windows()
    launch()  # Same saved preferences; default focus Dark.
    expect("light", True, False)
    key(57)  # Dark
    key(15); key(15); key(15)  # Studio
    key(57)  # Studio off
    key(15); key(57)  # Reduced motion on
    key(29, 31)
    expect("dark", False, True)
    key(42, 15); key(57)  # Studio preview, unsaved
    key(56, 16)  # Alt+Q compositor close request
    assert windows(), "Unsaved changes must block compositor close"
    capture("unsaved-close")
    key(1)  # Escape cancels close
    key(29, 44)  # Undo remains reachable after cancellation
    key(29, 16)
    assert not windows(), "Undo/quit must work after cancelling a close"
    launch()
    resize(800, 640)
    capture("compact")
    assert windows()[0]["size"] == [800, 640]
    resize(980, 720)
    capture("final")
    (OUT / "window.json").write_text(json.dumps(windows()[0], indent=2) + "\n")
    (OUT / "journal.txt").write_text(guest("sudo", "journalctl", "-b", f"_SYSTEMD_USER_UNIT={UNIT}", "--no-pager"))
    print("PASS preview, save, persistence, motion, keyboard, close protection, and minimum size", flush=True)
