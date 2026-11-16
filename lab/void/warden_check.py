#!/usr/bin/env python3
"""Run the Warden's root-only tests inside the Void lab guest.

The Warden runs as root, and nobody needs root on their own desktop
(ADR 0016). This builds the tests of `crowbar-sandbox` that start real
compartments, copies them into the lab guest, runs them there as root, and
records what they print.

    warden_check.py              boot a fresh guest, run every test, power it off
    warden_check.py --keep       use the guest that is running, and leave it running
    warden_check.py -- FILTER    pass arguments on to the test program

Stdlib only, no sudo. The evidence log goes to <repo>/.lab/void/.
"""
import argparse
import json
import subprocess
import sys
import time

import void_lab as lab

GUEST_DIR = "/root/warden"
PROGRAMS = ("crowbar-warden", "crowbar-sandbox-probe")


def build():
    """Builds the tests and their programs as static files. Returns their paths."""
    command = ["cargo", "test", "--locked", "-p", "crowbar-sandbox", "--test", "compartments",
               "--no-run", "--target", lab.MUSL, "--message-format=json"]
    print("+", " ".join(command), flush=True)
    built = subprocess.run(command, cwd=lab.WORKSPACE, check=True, text=True,
                           stdout=subprocess.PIPE).stdout
    tests = [message["executable"] for message in map(json.loads, built.splitlines())
             if message.get("reason") == "compiler-artifact" and message.get("executable")
             and message["target"]["name"] == "compartments"]
    if len(tests) != 1:
        raise SystemExit(f"expected one test program, found {tests}")
    programs = [lab.WORKSPACE / "target" / lab.MUSL / "debug" / name for name in PROGRAMS]
    return [(tests[0], "compartments")] + [(path, path.name) for path in programs]


def push(files):
    lab.ssh(f"rm -rf {GUEST_DIR} && mkdir -p {GUEST_DIR}")
    for source, name in files:
        with open(source, "rb") as stream:
            subprocess.run(lab.ssh_args() + [f"cat > {GUEST_DIR}/{name} && chmod 755 {GUEST_DIR}/{name}"],
                           stdin=stream, check=True, timeout=300)


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--keep", action="store_true",
                        help="use the running guest and leave it running")
    parser.add_argument("test_args", nargs="*", help="arguments for the test program")
    args = parser.parse_args()

    files = build()
    fresh = not (args.keep and lab.running())
    if fresh:
        if lab.running():
            lab.stop()
        if not lab.BASE_DISK.exists():
            lab.image()
        lab.start(fresh=True)
    ev = lab.Evidence()
    ev.path.rename(ev.path.with_name(ev.path.name.replace("milestone-", "warden-")))
    ev.path = ev.path.with_name(ev.path.name.replace("milestone-", "warden-"))
    ev.log(f"Evidence log: {ev.path}")
    code = 1
    try:
        if not lab.wait_ssh(ev):
            raise RuntimeError("the guest did not come up")
        ev.guest("uname -r; cat /sys/kernel/security/lsm; echo; findmnt -no FSTYPE /sys/fs/cgroup")
        push(files)
        ev.guest(f"sha256sum {GUEST_DIR}/*")
        # Only the tests that need root are ignored by default, so
        # `--ignored` runs exactly those.
        tests = " ".join(["--ignored"] + [f"'{arg}'" for arg in args.test_args])
        started = time.time()
        result = ev.guest(f"cd {GUEST_DIR} && CROWBAR_SANDBOX_BIN_DIR={GUEST_DIR} "
                          f"CROWBAR_SANDBOX_TEST_DIR=/var/tmp ./compartments {tests} 2>&1",
                          check=False)
        ev.check(f"the Warden's tests pass in the guest ({time.time() - started:.0f}s)",
                 result.returncode == 0)
        code = result.returncode
    except Exception as exc:  # Record the failure as evidence.
        ev.check("the check ran to its end", False, repr(exc))
    finally:
        if fresh:
            lab.stop()
    ev.log(f"Evidence: {ev.path}")
    return code


if __name__ == "__main__":
    sys.exit(main())
