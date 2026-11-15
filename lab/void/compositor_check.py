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
# LIBSEAT_BACKEND names the seat manager outright. Void builds libseat with an
# elogind backend as well; this guest runs no elogind.
AS_USER = (f"chpst -u {guest.GUEST_USER}:{guest.GUEST_USER}:_seatd "
           f"env HOME=/home/{guest.GUEST_USER} XDG_RUNTIME_DIR={RUNTIME} LIBSEAT_BACKEND=seatd")
# The same list as compositor/checks/nested.sh.
FORBIDDEN = [
    "zwp_virtual_keyboard_manager_v1", "zwlr_virtual_pointer_manager_v1",
    "zwp_input_method_manager_v2", "zwp_input_method_v1",
    "zwlr_data_control_manager_v1", "ext_data_control_manager_v1",
    "ext_image_copy_capture_manager_v1", "ext_output_image_capture_source_manager_v1",
    "ext_foreign_toplevel_image_capture_source_manager_v1",
    "zwlr_screencopy_manager_v1", "zwlr_export_dmabuf_manager_v1", "zwlr_layer_shell_v1",
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

    def run(self, command, check=True):
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
    if guest.running():
        guest.stop()
    guest.build()
    guest.image()
    built = {}
    for name in guest.BINARIES:
        built[name] = guest.void_lab.sha256(guest.TARGET / "release" / name)
        ev.log(f"binary {name}: sha256 {built[name]} (built in the Void build root)")
    guest.start()
    try:
        guest.wait_ssh()
        run_checks(ev, built)
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


def run_checks(ev, built):
    qmp = guest.Qmp()

    ev.log()
    ev.log("== the binaries in the guest are the ones that were built")
    in_guest = ev.run("sha256sum " + " ".join(f"/usr/local/bin/{name}" for name in guest.BINARIES))
    ev.check("the guest runs the binaries built above",
             all(f"{digest}  /usr/local/bin/{name}" in in_guest for name, digest in built.items()))

    ev.log()
    ev.log("== the guest: Void Linux, runit, seatd, no systemd")
    ev.run("uname -sr; cat /etc/os-release | head -2")
    ev.run("ps -o pid,user,args -p 1")
    systemd = ev.run("ls /run/systemd 2>&1; pgrep -l 'systemd|elogind' || echo 'no systemd or elogind process'")
    ev.check("no systemd and no elogind in the guest", "no systemd or elogind process" in systemd
             and "No such file" in systemd)
    ev.check("seatd runs under runit", "run: seatd" in ev.run("sv status seatd"))
    ev.run("xbps-query -l | wc -l")

    ev.log()
    ev.log("== what the binary links, and the Void package each library comes from")
    linked = ev.run("ldd /usr/local/bin/crowbar-compositor")
    ev.run("for lib in $(ldd /usr/local/bin/crowbar-compositor | awk '$3 ~ /^\\// {print $3}'); do "
             "xbps-query -o \"$(readlink -f $lib)\" | cut -d: -f1; done | sort -u")
    ev.check("the binary's libraries all resolve", "=> /" in linked and "not found" not in linked)

    ev.log()
    ev.log("== start the compositor on virtual terminal 2 as an ordinary user")
    # No logind makes the runtime directory, so the lab does. The image is
    # built without a mapping for the user's id, so its home is fixed up here.
    ev.run(f"install -d -o {guest.GUEST_USER} -g {guest.GUEST_USER} -m 700 {RUNTIME}; "
             f"chown {guest.GUEST_USER}:{guest.GUEST_USER} /home/{guest.GUEST_USER}")
    ev.run("printf '#!/bin/sh\\nexec crowbar-probe keylog first >/tmp/keylog-1.out 2>&1\\n' "
             ">/usr/local/bin/keylog-1 && chmod +x /usr/local/bin/keylog-1")
    ev.run(f"openvt -c 2 -s -f -- {AS_USER} sh -c "
             "'exec crowbar-compositor --backend tty --scale Virtual-2=2 --spawn keylog-1 "
             ">/tmp/compositor.log 2>&1'")
    up = wait_for(ev, "the compositor", lambda: "compositor is up" in compositor_log())
    log = compositor_log()
    ev.check("the compositor starts on a TTY", up)
    ev.check("the seat comes from libseat", "session opened through libseat" in log)
    ev.check("the display is a DRM device", "DRM device opened" in log)
    ev.check("the first output is up", "output connected name=Virtual-1" in log)
    ev.run("cat /sys/class/tty/tty0/active; ls -l /run/seatd.sock; "
           f"ps -o user,tty,args -C crowbar-compositor; id {guest.GUEST_USER}")

    ev.log()
    ev.log("== routes around the compositor that ADR 0014 names: input devices and tracing")
    # libinput takes any input device that appears, so an account that can
    # open /dev/uinput can type the confirm chord as the person.
    ev.run("modprobe uinput 2>&1; ls -l /dev/uinput /dev/input/ 2>&1", check=False)
    uinput = ev.run(f"{AS_USER} sh -c 'exec 3<>/dev/uinput' 2>&1 && echo OPENED || echo REFUSED", check=False)
    ev.check("the account that runs the compositor's clients cannot open /dev/uinput", "REFUSED" in uinput)
    events = ev.run(f"{AS_USER} sh -c 'for d in /dev/input/event*; do (exec 3<$d) 2>/dev/null && echo OPENED $d; done'; "
                    "echo done", check=False)
    ev.check("that account cannot read the input devices directly", "OPENED" not in events)
    # Recorded, not checked: every client here runs as the same user as the
    # compositor, and what stops one tracing another is this setting alone.
    ev.run("cat /proc/sys/kernel/yama/ptrace_scope 2>&1 || echo 'no Yama ptrace_scope'", check=False)

    socket_name = re.search(r'compositor is up socket="([^"]+)"', log)
    display = socket_name.group(1) if socket_name else "wayland-1"
    client = f"{AS_USER} WAYLAND_DISPLAY={display}"
    wait_for(ev, "the first window", lambda: "window mapped" in guest.ssh(
        "cat /tmp/keylog-1.out 2>/dev/null", check=False).stdout)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-1-first-output.png", 0)

    ev.log()
    ev.log("== what an ordinary client is offered (wayland-info inside the guest)")
    ordinary = interfaces(ev.run(f"{client} wayland-info | grep -E \"^interface|name:|scale:\""))
    confined = interfaces(ev.run(f"{client} crowbar-probe security-context wayland-info | grep ^interface"))
    ev.check("no forbidden global is offered to an ordinary client",
             not set(FORBIDDEN) & set(ordinary), ", ".join(sorted(set(FORBIDDEN) & set(ordinary))))
    ev.check("a security-context client gets a smaller set",
             confined and set(confined) < set(ordinary),
             "missing: " + ", ".join(sorted(set(ordinary) - set(confined))))
    layer = ev.run(f"{client} crowbar-probe layer 2>&1 || true")
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
    keylog = ev.run("cat /tmp/keylog-1.out")
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
    outputs = ev.run(f"{client} wayland-info | grep -A12 \"interface: 'wl_output'\" | "
                       "grep -E \"name:|scale:|width:\"")
    ev.check("two outputs with different scales",
             re.search(r"name: Virtual-1", outputs) and re.search(r"name: Virtual-2", outputs)
             and "scale: 1" in outputs and "scale: 2" in outputs)

    ev.log()
    ev.log("== a window on each output is told that output's scale")
    qmp.pointer(0.85, 0.2)
    ev.run(f"({client} crowbar-probe keylog second >/tmp/keylog-2.out 2>&1 &) ; sleep 2; "
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
