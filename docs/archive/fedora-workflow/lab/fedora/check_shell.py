#!/usr/bin/env python3
"""Read-only shell evidence helpers plus explicit input methods for the test VM.

Call from the repository with the VM viewer open. Screenshots are real guest
frames, not mockups. Interactive validation is documented separately.
"""
import json
from pathlib import Path
import lab
import check_gpui as inputs
import check_studio
OUT = lab.ROOT / ".local/shell/validation"
DESKTOP = inputs.DESKTOP

def guest(*args):
    return lab.ssh("test", list(args), capture=True)

def desktop(*args):
    return guest(*DESKTOP, *args)

def hypr(*args):
    return desktop("hyprctl", "-i", "0", *args)

def capture(name):
    OUT.mkdir(parents=True, exist_ok=True)
    desktop("env", "WAYLAND_DISPLAY=wayland-1", "grim", "/home/desktop/shell-check.png")
    with (OUT / f"{name}.png").open("wb") as stream:
        lab.ssh("test", ["sudo", "cat", "/home/desktop/shell-check.png"], stdout=stream)
    return OUT / f"{name}.png"

def click(x,y,width=1280,height=800):
    inputs.pointer(x,y,width,height)
    inputs.button("left")

def key(*codes):
    inputs.key(*codes)

def evidence():
    OUT.mkdir(parents=True,exist_ok=True)
    for command in ("layers","monitors","clients"):
        (OUT / f"{command}.json").write_text(hypr(command,"-j"))
    (OUT / "journal.txt").write_text(guest("sudo","journalctl","-b","_SYSTEMD_USER_UNIT=crowbaros-shell.service","--no-pager"))
    (OUT / "packages.txt").write_text(guest("rpm","-q","crowbaros-shell","crowbaros-studio-settings","hyprland"))

if __name__ == "__main__":
    evidence()
    print(capture("desktop"))
