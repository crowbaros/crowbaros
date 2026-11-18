#!/usr/bin/env python3
"""Checks the CrowbarOS compositor on a TTY in the Void guest.

The compositor is built from the Fulcrum commit that compositor/fulcrum.json
pins, or the one FULCRUM_REVISION names (ADR 0027). The log names both
repositories' commits.

Run through `compositor_guest.py check`. It boots the guest and starts the
compositor on a virtual terminal as its own user. Clients reach it the two
ways it allows (ADR 0016): the shell's user connects to the shell's socket, and
everything else comes through a listener that a stand-in for the Warden
registers as root. The check drives the guest from outside: keys and the
pointer through QEMU, a second monitor plugged in through QEMU, and questions
asked by `wayland-info` and the probe inside the guest. Everything it runs and
sees goes to an evidence log in .lab/void-graphical/.
"""
import re
import subprocess
import time

import compositor_guest as guest

COMPOSITOR, SHELL, APP = "crowbar-compositor", "crowbar-shell", "crowbar-app"
SHELL_UID = 1001
# A directory for each socket, which only its two ends can enter (ADR 0016).
# Base will make these. Here the check does.
SHELL_SOCKET = "/run/crowbar/compositor-shell/wayland"
WARDEN_SOCKET = "/run/crowbar/compositor-warden/wayland"
# Where the stand-in for the Warden puts one compartment's listener. The
# directory is the client account's alone, in place of a sandbox.
LISTENER = "/run/crowbar/compartment-1/wayland"
# LIBSEAT_BACKEND names the seat manager outright. Void builds libseat with an
# elogind backend as well; this guest runs no elogind. The compositor logs
# each frame it draws at the debug level.
AS_COMPOSITOR = (f"chpst -u {COMPOSITOR}:{COMPOSITOR}:_seatd env HOME=/home/{COMPOSITOR} "
                 "XDG_RUNTIME_DIR=/run/user/1000 LIBSEAT_BACKEND=seatd "
                 "FULCRUM_LOG=debug")
AS_SHELL = f"chpst -u {SHELL}:{SHELL} env HOME=/home/{SHELL} WAYLAND_DISPLAY={SHELL_SOCKET}"
AS_WARDEN = f"env WAYLAND_DISPLAY={WARDEN_SOCKET}"
# A client's account: no group, and nothing in its environment but a home.
AS_APP = f"chpst -u {APP}:{APP} env HOME=/home/{APP}"
AS_CLIENT = f"{AS_APP} WAYLAND_DISPLAY={LISTENER}"

# What each client is offered: the rows of ADR 0020's table, less the globals
# the compositor does not create yet. Fulcrum's tests/table.rs holds the same
# lists.
COMPARTMENT = {"wl_compositor", "wl_data_device_manager", "wl_output", "wl_seat", "wl_shm",
               "wl_subcompositor", "wp_fractional_scale_manager_v1", "wp_presentation",
               "wp_viewporter", "xdg_wm_base"}
# The shell is offered a compartment-with-the-GPU's row too (ADR 0023), so
# explicit sync where the display's device has timeline syncobjs, as the
# guest's does.
SHELL_ALSO = {"wp_linux_drm_syncobj_manager_v1", "zwp_linux_dmabuf_v1", "zwlr_layer_shell_v1"}
WARDEN = {"wp_security_context_manager_v1"}


def nobody_row():
    """The "Nobody" row of the table, read from the source of the compositor that was built."""
    source = (guest.fulcrum_source() / "src/policy.rs").read_text()
    row = re.search(r"pub const NOBODY: &\[&str\] = &\[(.*?)\];", source, re.S)
    names = re.findall(r'"([a-z0-9_]+)"', row.group(1)) if row else []
    if len(names) < 20:
        raise RuntimeError("could not read the Nobody row from Fulcrum's src/policy.rs")
    return names


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
    return set(re.findall(r"^interface: '([^']+)'", text, re.M))


def exactly(ev, who, seen, wanted):
    ev.check(f"{who} is offered exactly the table's list", seen == wanted,
             "differs in: " + ", ".join(sorted(seen ^ wanted)) if seen != wanted else "")


def main():
    ev = Evidence()
    ev.log("# The CrowbarOS compositor on a TTY in the Void guest")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=guest.ROOT, text=True).strip()
    changed = subprocess.check_output(["git", "status", "--porcelain"], cwd=guest.ROOT, text=True).strip()
    ev.log(f"CrowbarOS commit {commit}" + (", with changes that are not committed" if changed else ""))
    fulcrum = guest.fulcrum_pin()
    ev.log(f"Fulcrum commit {fulcrum['revision']} from {fulcrum['repository']}"
           + (", as pinned" if fulcrum["pinned"] else
              f", NOT the pinned {fulcrum['pinned_revision']}: FULCRUM_REVISION names it"))
    if guest.running():
        guest.stop()
    guest.build()
    guest.image()
    built = {}
    for name, path in guest.binaries(desktop=False).items():
        built[name] = guest.void_lab.sha256(path)
        ev.log(f"binary {name}: sha256 {built[name]} (built and tested in the Void build root)")
    guest.start()
    try:
        guest.wait_ssh()
        run_checks(ev, built)
    except Exception as error:  # noqa: BLE001 - record whatever stopped the run
        ev.check("the run finished", False, repr(error))
    finally:
        ev.log()
        ev.log("== compositor log, without the line for each frame")
        for line in compositor_log().splitlines():
            if "frame drawn" not in line:
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
    in_guest = ev.run("sha256sum " + " ".join(f"/usr/local/bin/{name}" for name in built))
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

    ev.log()
    ev.log("== what the binary links, and the Void package each library comes from")
    linked = ev.run("ldd /usr/local/bin/crowbar-compositor")
    ev.run("for lib in $(ldd /usr/local/bin/crowbar-compositor | awk '$3 ~ /^\\// {print $3}'); do "
           "xbps-query -o \"$(readlink -f $lib)\" | cut -d: -f1; done | sort -u")
    ev.check("the binary's libraries all resolve", "=> /" in linked and "not found" not in linked)

    ev.log()
    ev.log("== start the compositor on virtual terminal 2 as its own user")
    # No logind makes a runtime directory, so the lab does. The image is built
    # without a mapping for the users' ids, so their homes are fixed up here.
    ev.run(f"for user in {COMPOSITOR} {SHELL} {APP}; do chown $user:$user /home/$user; done; "
           f"install -d -o {COMPOSITOR} -g {COMPOSITOR} -m 700 /run/user/1000; "
           "install -d -m 755 /run/crowbar; "
           f"install -d -o {COMPOSITOR} -g {SHELL} -m 2750 {SHELL_SOCKET.rsplit('/', 1)[0]}; "
           f"install -d -o {COMPOSITOR} -g root -m 750 {WARDEN_SOCKET.rsplit('/', 1)[0]}; "
           f"install -d -o {APP} -g {APP} -m 700 {LISTENER.rsplit('/', 1)[0]}")
    # The shell that becomes the compositor first lists the descriptors it was
    # started with, so that a later check can tell them from the ones the
    # compositor opened.
    ev.run(f"openvt -c 2 -s -f -- {AS_COMPOSITOR} sh -c "
           f"'echo /proc/$$/fd/* >/tmp/started-with; exec crowbar-compositor --backend tty --shell-socket {SHELL_SOCKET} --shell-uid {SHELL_UID} "
           f"--warden-socket {WARDEN_SOCKET} --warden-uid 0 >/tmp/compositor.log 2>&1'")
    up = wait_for(ev, "the compositor", lambda: "compositor is up" in compositor_log())
    log = compositor_log()
    ev.check("the compositor starts on a TTY", up)
    ev.check("the seat comes from seatd", "session opened through seatd" in log)
    ev.check("the display is a DRM device", "DRM device opened" in log)
    ev.check("the first output is up", "output connected name=Virtual-1" in log)
    who = ev.run("cat /sys/class/tty/tty0/active; ls -l /run/seatd.sock; "
                 f"ps -o uid=,tty=,args= -C crowbar-compositor; id -u {COMPOSITOR}; getent group _seatd")
    ev.check("the compositor runs as its own user, the only member of _seatd",
             re.search(r"^\s*1000\s+tty2\s+crowbar-compositor ", who, re.M) and re.search(r"^1000$", who, re.M)
             and re.search(rf"^_seatd:[^:]*:\d+:{COMPOSITOR}$", who, re.M))
    # By its whole name: `pgrep -x` matches the kernel's name for a process,
    # which holds 15 characters, and so never matches this one.
    pid = ev.run("pidof crowbar-compositor").strip()
    if not pid.isdigit():
        raise RuntimeError(f"expected one compositor process, found: {pid!r}")
    children = ev.run(f"pgrep -P {pid} || echo 'no child process'")
    ev.check("the compositor started no process", "no child process" in children)
    # The seat, the DRM device, and the input devices are open by now. A
    # descriptor is close-on-exec when the 02000000 bit of its flags is set.
    inherited = ev.run(f"cd /proc/{pid} && for fd in fd/*; do "
                       "flags=$(awk '/^flags:/ {print $2}' fdinfo/${fd#fd/}); "
                       "[ $(( 0$flags & 02000000 )) -ne 0 ] || echo INHERITABLE ${fd#fd/} $(readlink $fd); "
                       "done; echo looked at $(ls fd | wc -l) descriptors")
    # What it was started with is not its own: the terminal, the log, and a
    # second descriptor for the terminal that openvt leaves open.
    started_with = {path.rsplit("/", 1)[1] for path in ev.run("cat /tmp/started-with").split()}
    kept = [line for line in inherited.splitlines()
            if line.startswith("INHERITABLE") and line.split()[1] not in started_with]
    ev.check("every descriptor the compositor opened on the TTY is close-on-exec",
             not kept and len(started_with) < 8 and re.search(r"looked at [1-9]\d+ descriptors", inherited),
             "; ".join(kept))

    ev.log()
    ev.log("== routes around the compositor that ADR 0014 names: input devices and tracing")
    # libinput takes any input device that appears, so an account that can
    # open /dev/uinput can type as the person. Each check needs the device to
    # exist, the command to run as the client's account, root to succeed where
    # the account fails, and the failure to be a refusal.
    ev.run("modprobe uinput 2>&1; ls -l /dev/uinput /dev/input/ 2>&1", check=False)
    who = ev.run(f"{AS_APP} id", check=False)
    ev.check("the client's account has no group but its own",
             f"({APP})" in who and "_seatd" not in who and "input" not in who and "video" not in who)
    open_uinput = "sh -c 'test -c /dev/uinput && exec 3<>/dev/uinput && echo OPENED' 2>&1"
    as_root = ev.run(open_uinput, check=False)
    as_client = ev.run(f"{AS_APP} {open_uinput}", check=False)
    ev.check("root can open /dev/uinput and the client's account is refused",
             "OPENED" in as_root and "OPENED" not in as_client and "Permission denied" in as_client)
    open_events = ("sh -c 'for d in /dev/input/event*; do out=$( (exec 3<$d) 2>&1 ) "
                   "&& echo OPENED $d || echo FAILED $d: $out; done'")
    as_root = ev.run(open_events, check=False).splitlines()
    as_client = ev.run(f"{AS_APP} {open_events}", check=False).splitlines()
    ev.check("root can read every input device and the client's account is refused each one",
             as_root and all(line.startswith("OPENED /dev/input/event") for line in as_root)
             and len(as_client) == len(as_root)
             and all(line.startswith("FAILED") and "Permission denied" in line for line in as_client))
    # Recorded, not checked: Base does not set this yet.
    ev.run("cat /proc/sys/kernel/yama/ptrace_scope 2>&1 || echo 'no Yama ptrace_scope'", check=False)

    ev.log()
    ev.log("== ADR 0016, attack 11: a compartment with no listener looks for any Wayland socket")
    sockets = ev.run("find / -xdev -type s 2>/dev/null; find /run /tmp /dev -type s 2>/dev/null").split()
    sockets = sorted(set(sockets))
    ev.check("the compositor made its two sockets and no other",
             SHELL_SOCKET in sockets and WARDEN_SOCKET in sockets
             and not ev.run("ls -A /run/user/1000").strip()
             and not [path for path in sockets if "wayland" in path and path not in (SHELL_SOCKET, WARDEN_SOCKET)])
    # Each socket on the machine, asked for a Wayland registry by the client's
    # account. One that answers is a way in.
    answered = ev.run("for s in " + " ".join(sockets) + f"; do {AS_APP} WAYLAND_DISPLAY=$s timeout 3 "
                      "crowbar-probe globals >/dev/null 2>&1 && echo ANSWERED $s; done; echo asked", check=False)
    ev.check(f"none of the {len(sockets)} sockets on the machine answers the client's account as a compositor",
             "ANSWERED" not in answered and "asked" in answered)
    # The directories keep the client's account out. Open them, to test the
    # check behind them: the peer's user ID, as the kernel reports it.
    doors = [SHELL_SOCKET.rsplit("/", 1)[0], WARDEN_SOCKET.rsplit("/", 1)[0]]
    try:
        ev.run("chmod o+rx " + " ".join(doors) + f"; chmod o+rw {SHELL_SOCKET} {WARDEN_SOCKET}")
        given = ev.run(f"for s in {SHELL_SOCKET} {WARDEN_SOCKET}; do {AS_APP} WAYLAND_DISPLAY=$s timeout 3 "
                       "crowbar-probe globals 2>&1 && echo ANSWERED $s; done; echo asked", check=False)
    finally:
        ev.run(f"chmod 2750 {doors[0]}; chmod 750 {doors[1]}; chmod 660 {SHELL_SOCKET} {WARDEN_SOCKET}")
    ev.check("given both trusted sockets on purpose, the client's account is refused on its user ID",
             "ANSWERED" not in given and "asked" in given
             and compositor_log().count("refused a peer with the wrong user ID") >= 2)

    ev.log()
    ev.log("== what the shell and the Warden are offered (wayland-info inside the guest)")
    shell = interfaces(ev.run(f"{AS_SHELL} wayland-info | grep -E \"^interface|name:\""))
    warden = interfaces(ev.run(f"{AS_WARDEN} wayland-info | grep ^interface"))
    exactly(ev, "the shell", shell, COMPARTMENT | SHELL_ALSO)
    exactly(ev, "the Warden", warden, WARDEN)
    ev.check("the shell can create a layer surface",
             "layer surface created" in ev.run(f"{AS_SHELL} crowbar-probe layer 2>&1 || true"))

    ev.log()
    ev.log("== a stand-in for the Warden registers one compartment's listener, as root")
    ev.run(f"({AS_WARDEN} crowbar-probe warden org.crowbaros.Probe compartment-1 {LISTENER} "
           ">/tmp/warden.out 2>&1 &); sleep 1; cat /tmp/warden.out")
    ev.check("the compositor took the listener with its principal ID and compartment ID",
             "compartment listener registered principal=org.crowbaros.Probe compartment=compartment-1"
             in compositor_log())
    client = interfaces(ev.run(f"{AS_CLIENT} wayland-info | grep ^interface"))
    exactly(ev, "a client of the compartment's listener", client, COMPARTMENT)

    ev.log()
    ev.log("== ADR 0020, attack 1, and ADR 0016, attack 12: a client on a listener asks for what it is not offered")
    forbidden = nobody_row() + sorted(SHELL_ALSO | WARDEN)
    ev.check("none of them is offered to it", not set(forbidden) & client,
             ", ".join(sorted(set(forbidden) & client)))
    guessed = ev.run(f"{AS_CLIENT} crowbar-probe guess " + " ".join(forbidden) + " 2>&1; echo exit $?", check=False)
    ev.check(f"each of the {len(forbidden)} is refused at every global number it could guess",
             "exit 0" in guessed and "BOUND" not in guessed
             and guessed.count(": refused at every number it could guess") == len(forbidden))
    layer = ev.run(f"{AS_CLIENT} crowbar-probe layer 2>&1 || true")
    ev.check("it cannot create a layer surface", "not offered" in layer)
    nested = ev.run(f"{AS_CLIENT} crowbar-probe warden nested nested-1 /tmp/nested-listener true 2>&1 || true")
    ev.check("it cannot register a listener of its own", "not offered" in nested)

    ev.log()
    ev.log("== keys from the virtual keyboard, through the kernel and libinput")
    ev.run(f"({AS_CLIENT} crowbar-probe keylog first >/tmp/keylog-1.out 2>&1 &); true")
    wait_for(ev, "the first window", lambda: "window mapped" in guest.ssh(
        "cat /tmp/keylog-1.out 2>/dev/null", check=False).stdout)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-1-first-output.png", 0)
    qmp.key("a", True)
    qmp.key("a", False)
    time.sleep(1)
    ev.check("a new window does not take the keyboard", "key 30 " not in ev.run("cat /tmp/keylog-1.out"))
    # The window opens at 48,48 and is 480 by 320. The output is 1280 by 960.
    qmp.pointer(200 / 1280, 200 / 960)
    qmp.click()
    qmp.key("b", True)
    qmp.key("b", False)
    time.sleep(1)
    keylog = ev.run("cat /tmp/keylog-1.out")
    ev.check("a click gives the window the keyboard, and a plain key then reaches it",
             "enter held" in keylog and "key 48 pressed" in keylog and "key 48 released" in keylog)

    ev.log()
    ev.log("== a still desktop draws no frames (ADR 0020)")

    def drawing():
        text = guest.ssh(f"grep -c 'frame drawn' /tmp/compositor.log; grep ctxt_switches /proc/{pid}/status",
                         check=False).stdout
        frames = int(text.split()[0])
        switches = re.findall(r"ctxt_switches:\s+(\d+)", text)
        if len(switches) != 2:
            raise RuntimeError(f"could not read how often the compositor ran: {text!r}")
        return frames, sum(int(number) for number in switches)

    time.sleep(3)
    before = drawing()
    time.sleep(10)
    after = drawing()
    ev.log(f"    frames drawn: {before[0]} then {after[0]}; "
           f"times the compositor's process ran: {before[1]} then {after[1]}")
    ev.check("ten seconds with a window on screen and nothing changing draw no frame", after[0] == before[0])
    # A compositor that redraws on a timer runs about 600 times in ten seconds.
    ev.check("the compositor's process barely ran in those ten seconds", after[1] - before[1] <= 20,
             f"it ran {after[1] - before[1]} times, of {after[1]} since it started")
    for step in range(10):
        qmp.pointer((300 + 20 * step) / 1280, 300 / 960)
    time.sleep(1)
    moved = drawing()
    ev.log(f"    frames drawn after the pointer moved: {moved[0]}")
    ev.check("moving the pointer, which the compositor draws, draws frames again", moved[0] > after[0])
    time.sleep(3)
    settled = drawing()
    time.sleep(5)
    ev.check("and the desktop is still again afterwards", drawing()[0] == settled[0])

    ev.log()
    ev.log("== plug a second monitor in")
    guest.plug_monitor(1, 1024, 768)
    plugged = wait_for(ev, "the second output",
                       lambda: "output connected name=Virtual-2" in compositor_log())
    ev.check("the hotplug reaches the compositor through udev",
             "udev says the DRM device changed" in compositor_log())
    ev.check("the second output comes up", plugged)
    outputs = ev.run(f"{AS_CLIENT} wayland-info | grep -A12 \"interface: 'wl_output'\" | grep -E \"name:|width:\"")
    ev.check("a client is told of both outputs",
             re.search(r"name: Virtual-1", outputs) and re.search(r"name: Virtual-2", outputs))
    time.sleep(1)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-2-first-output.png", 0)
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-3-second-output.png", 1)
    ev.log(f"screens: {guest.WORK}/compositor-{ev.stamp}-*.png")

    ev.log()
    ev.log("== Void's own foot, a client of the compartment's listener")
    ev.run(f"({AS_CLIENT} foot --working-directory=/home/{APP} sh -c 'echo foot on the CrowbarOS compositor; sleep 60' "
           ">/tmp/foot.out 2>&1 &); sleep 3; cat /tmp/foot.out; true")
    footing = ev.run("pgrep -x foot || echo 'foot is not running'", check=False)
    ev.check("foot opens a window and keeps running",
             "foot is not running" not in footing and "app_id=foot" in compositor_log())
    qmp.screendump(guest.WORK / f"compositor-{ev.stamp}-4-foot.png", 0)
    ev.run("pkill -x foot; true")

    ev.log()
    ev.log("== the Warden stops the compartment and lets go of its listener")
    # Every process of the compartment ends, and with the stand-in the listener.
    ev.run("pkill -x crowbar-probe; sleep 1; true")
    # The socket is still there, so it is the compositor that accepts nothing.
    # The probe has to say so itself: a probe that timed out says nothing.
    stopped = ev.run(f"test -S {LISTENER} && echo the socket is still there; "
                     f"{AS_CLIENT} timeout 3 crowbar-probe globals 2>&1; echo exit $?", check=False)
    ev.check("the listener of a stopped compartment accepts nothing",
             "the socket is still there" in stopped
             and "Connection refused" in stopped and stopped.rstrip().endswith("exit 1"))


if __name__ == "__main__":
    raise SystemExit(main())
