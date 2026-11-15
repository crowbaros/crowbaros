#!/usr/bin/env python3
"""Checks that the CrowbarOS compositor runs on a TTY in the Void guest.

Run through `compositor_guest.py check`. It boots the guest, starts the
compositor on a virtual terminal as an ordinary user, and drives it from
outside: keys and pointer through QEMU, a second monitor plugged in through
QEMU, and questions asked by `wayland-info` inside the guest. Everything it
runs and sees goes to an evidence log in .lab/void-graphical/.
"""
import re
import time

import compositor_guest as guest

RUNTIME = "/run/user/1000"
AS_USER = (f"chpst -u {guest.GUEST_USER}:{guest.GUEST_USER}:_seatd:video:input "
           f"env HOME=/home/{guest.GUEST_USER} XDG_RUNTIME_DIR={RUNTIME}")
FORBIDDEN = [
    "zwp_virtual_keyboard_manager_v1", "zwlr_virtual_pointer_manager_v1",
    "zwp_input_method_manager_v2", "zwp_input_method_v1",
    "zwlr_data_control_manager_v1", "ext_data_control_manager_v1",
    "ext_image_copy_capture_manager_v1", "ext_output_image_capture_source_manager_v1",
    "zwlr_screencopy_manager_v1", "zwlr_layer_shell_v1",
]


class Evidence:
    def __init__(self):
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        self.stamp = stamp
        self.path = guest.WORK / f"compositor-{stamp}.log"
        self.stream = self.path.open("w")
        self.results = []

    def log(self, text=""):
        line = f"[{time.strftime('%H:%M:%S', time.gmtime())}] {text}"
        print(line, flush=True)
        self.stream.write(line + "\n")
        self.stream.flush()

    def guest(self, command, check=True):
        self.log(f"guest$ {command}")
        result = guest.ssh(command, check=False)
        for line in (result.stdout + result.stderr).rstrip().splitlines():
            self.log(f"    {line}")
        if check and result.returncode != 0:
            raise RuntimeError(f"guest command failed ({result.returncode}): {command}")
        return result.stdout

    def check(self, name, ok, detail=""):
        self.results.append((name, bool(ok)))
        self.log(f"{'PASS' if ok else 'FAIL'}: {name}{' - ' + detail if detail else ''}")
        return ok


def wait_for(ev, what, probe, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        found = probe()
        if found:
            return found
        time.sleep(0.5)
    ev.log(f"timed out waiting for {what}")
    return None


def compositor_log():
    return guest.ssh("cat /tmp/compositor.log 2>/dev/null", check=False).stdout


def interfaces(text):
    return sorted(re.findall(r"^interface: '([^']+)'", text, re.M))


def main():
    ev = Evidence()
    ev.log("# The CrowbarOS compositor on a TTY in the Void guest")
    for name in guest.BINARIES:
        path = guest.TARGET / "release" / name
        ev.log(f"binary {name}: sha256 {guest.void_lab.sha256(path)} (built in the Void build root)")
    if guest.running():
        guest.stop()
    guest.start()
    try:
        guest.wait_ssh()
        run_checks(ev)
    except Exception as error:  # noqa: BLE001 - record whatever stopped the run
        ev.check("the run finished", False, repr(error))
    finally:
        ev.log()
        ev.log("== compositor log")
        for line in compositor_log().splitlines():
            ev.log(f"    {line}")
        guest.stop()
    ev.log()
    failed = [name for name, ok in ev.results if not ok]
    ev.log(f"RESULT: {len(ev.results) - len(failed)} passed, {len(failed)} failed")
    ev.log(f"log: {ev.path}")
    return 1 if failed else 0


def run_checks(ev):
    qmp = guest.Qmp()

    ev.log()
    ev.log("== the guest: Void Linux, runit, seatd, no systemd")
    ev.guest("uname -sr; cat /etc/os-release | head -2")
    ev.guest("ps -o pid,user,args -p 1")
    systemd = ev.guest("ls /run/systemd 2>&1; pgrep -l 'systemd|elogind' || echo 'no systemd or elogind process'")
    ev.check("no systemd and no elogind in the guest", "no systemd or elogind process" in systemd
             and "No such file" in systemd)
    ev.check("seatd runs under runit", "run: seatd" in ev.guest("sv status seatd"))
    ev.guest("xbps-query -l | wc -l")

    ev.log()
    ev.log("== what the binary links, and the Void package each library comes from")
    libraries = ev.guest("ldd /usr/local/bin/crowbar-compositor | awk '$3 ~ /^\\// {print $3}' | sort")
    ev.guest("for lib in $(ldd /usr/local/bin/crowbar-compositor | awk '$3 ~ /^\\// {print $3}'); do "
             "xbps-query -o \"$(readlink -f $lib)\" | cut -d: -f1; done | sort -u")
    ev.check("the binary's libraries all resolve", "not found" not in libraries and libraries.strip())

    ev.log()
    ev.log("== start the compositor on virtual terminal 2 as an ordinary user")
    # No logind makes the runtime directory, so the lab does. The image is
    # built without a mapping for the user's id, so its home is fixed up here.
    ev.guest(f"install -d -o {guest.GUEST_USER} -g {guest.GUEST_USER} -m 700 {RUNTIME}; "
             f"chown {guest.GUEST_USER}:{guest.GUEST_USER} /home/{guest.GUEST_USER}")
    ev.guest("printf '#!/bin/sh\\nexec crowbar-probe keylog first >/tmp/keylog-1.out 2>&1\\n' "
             ">/usr/local/bin/keylog-1 && chmod +x /usr/local/bin/keylog-1")
    ev.guest(f"openvt -c 2 -s -f -- {AS_USER} sh -c "
             "'exec crowbar-compositor --backend tty --scale Virtual-2=2 --spawn keylog-1 "
             ">/tmp/compositor.log 2>&1'")
    up = wait_for(ev, "the compositor", lambda: "compositor is up" in compositor_log())
    log = compositor_log()
    ev.check("the compositor starts on a TTY", up)
    ev.check("the seat comes from libseat", "session opened through libseat" in log)
    ev.check("the display is a DRM device", "DRM device opened" in log)
    ev.check("the first output is up", "output connected name=Virtual-1" in log)
    ev.guest("cat /sys/class/tty/tty0/active; ls -l /run/seatd.sock; "
             "ps -o user,tty,args -C crowbar-compositor")
    socket_name = re.search(r'compositor is up socket="([^"]+)"', log)
    display = socket_name.group(1) if socket_name else "wayland-1"
    client = f"{AS_USER} WAYLAND_DISPLAY={display}"
    wait_for(ev, "the first window", lambda: "window mapped" in guest.ssh(
        "cat /tmp/keylog-1.out 2>/dev/null", check=False).stdout)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-1-first-output.png", 0)

    ev.log()
    ev.log("== what an ordinary client is offered (wayland-info inside the guest)")
    ordinary = interfaces(ev.guest(f"{client} wayland-info | grep -E \"^interface|name:|scale:\""))
    sandboxed = interfaces(ev.guest(f"{client} crowbar-probe sandbox wayland-info | grep ^interface"))
    ev.check("no forbidden global is offered to an ordinary client",
             not set(FORBIDDEN) & set(ordinary), ", ".join(sorted(set(FORBIDDEN) & set(ordinary))))
    ev.check("a security-context client gets a smaller set",
             sandboxed and set(sandboxed) < set(ordinary),
             "missing: " + ", ".join(sorted(set(ordinary) - set(sandboxed))))
    layer = ev.guest(f"{client} crowbar-probe layer 2>&1 || true")
    ev.check("an ordinary client cannot create a layer surface", "not offered" in layer)

    ev.log()
    ev.log("== keys from the virtual keyboard, through the kernel and libinput")
    for key in ("a",):
        qmp.key(key, True)
        qmp.key(key, False)
    for key, down in (("ctrl", True), ("alt", True), ("ret", True), ("ret", False),
                      ("alt", False), ("ctrl", False)):
        qmp.key(key, down)
    time.sleep(1)
    keylog = ev.guest("cat /tmp/keylog-1.out")
    ev.check("a plain key reaches the focused client",
             "key 30 pressed" in keylog and "key 30 released" in keylog)
    ev.check("the client held the keyboard during the chord (it saw Ctrl and Alt)",
             "key 29 pressed" in keylog and "key 56 pressed" in keylog)
    ev.check("the client saw neither the press nor the release of the chord's key (Enter, 28)",
             "key 28 " not in keylog)
    ev.check("the compositor took the confirm chord", "confirm chord taken" in compositor_log())

    ev.log()
    ev.log("== plug a second monitor in")
    guest.plug_monitor(1, 1024, 768)
    plugged = wait_for(ev, "the second output",
                       lambda: "output connected name=Virtual-2" in compositor_log())
    ev.check("the hotplug reaches the compositor through udev",
             "udev says the DRM device changed" in compositor_log())
    ev.check("the second output comes up", plugged)
    outputs = ev.guest(f"{client} wayland-info | grep -A12 \"interface: 'wl_output'\" | "
                       "grep -E \"name:|scale:|width:\"")
    ev.check("two outputs with different scales",
             re.search(r"name: Virtual-1", outputs) and re.search(r"name: Virtual-2", outputs)
             and "scale: 1" in outputs and "scale: 2" in outputs)

    ev.log()
    ev.log("== a window on each output is told that output's scale")
    qmp.pointer(0.85, 0.2)
    ev.guest(f"({client} crowbar-probe keylog second >/tmp/keylog-2.out 2>&1 &) ; sleep 2; "
             "echo first:; grep scale /tmp/keylog-1.out; echo second:; grep scale /tmp/keylog-2.out; true")
    first = guest.ssh("grep scale /tmp/keylog-1.out", check=False).stdout
    second = guest.ssh("grep scale /tmp/keylog-2.out", check=False).stdout
    # Scale 1 is the protocol's default, so a surface hears nothing for it.
    ev.check("the window on Virtual-1 stays at scale 1", "scale 2" not in first)
    ev.check("the window on Virtual-2 is told scale 2", second.strip().endswith("scale 2"))
    time.sleep(1)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-2-first-output.png", 0)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-3-second-output.png", 1)
    ev.log(f"screens: {guest.WORK}/compositor-{ev.stamp}-*.png")


if __name__ == "__main__":
    raise SystemExit(main())
