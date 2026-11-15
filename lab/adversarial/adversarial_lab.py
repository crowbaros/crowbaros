#!/usr/bin/env python3
"""Milestone 2 adversarial lab: deterministic fixture services and scripted asks.

Stdlib only, no sudo, no VM. Every fixture listens on 127.0.0.1. Records, the
synthetic home folder, and smoke logs live in <repo>/.lab/adversarial (outside
git).

    adversarial_lab.py serve [--run NAME]   start every fixture, reset, write services.json, wait
    adversarial_lab.py reset [--run NAME]   reset the running suite and start a new record
    adversarial_lab.py smoke                reset the suite, check every fixture and the
                                            record format, twice, and compare the runs
"""
import argparse
import json
from pathlib import Path
import signal
import sys
import threading
import time
import urllib.error
import urllib.request

LAB = Path(__file__).resolve().parent
sys.path.insert(0, str(LAB))

from fixtures import WORK  # noqa: E402
import smoke as smoke_check  # noqa: E402
from suite import Suite  # noqa: E402

MANIFEST = WORK / "services.json"


def serve(run):
    suite = Suite(WORK)
    suite.start()
    suite.reset(run)
    MANIFEST.write_text(json.dumps(suite.manifest(), indent=2) + "\n")
    for name, service in suite.manifest()["services"].items():
        print(f"{name:9} {service['host']:22} {service['url']}  {service['title']}")
    print(f"Manifest: {MANIFEST}")
    print(f"Record:   {suite.record.path}")
    print(f"Home:     {suite.home}")
    print("Serving. Ctrl-C stops.", flush=True)
    stop = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    try:
        stop.wait()
    except KeyboardInterrupt:
        pass
    finally:
        suite.stop()
        MANIFEST.unlink(missing_ok=True)
    return 0


def reset(run):
    try:
        control = json.loads(MANIFEST.read_text())["services"]["control"]["url"]
    except FileNotFoundError:
        raise SystemExit(f"No suite is running ({MANIFEST} is missing). Start one with serve.")
    body = json.dumps({"run": run} if run else {}).encode()
    request = urllib.request.Request(f"{control}/reset", body, method="POST", headers={
        "Content-Type": "application/json", "Host": "control.fixture.test"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            print(json.dumps(json.loads(response.read()), indent=2))
    except urllib.error.HTTPError as error:
        raise SystemExit(f"reset refused: {error.read().decode()}")
    except urllib.error.URLError as error:
        raise SystemExit(f"No suite answers at {control} ({error.reason}). Start one with serve.")
    return 0


def smoke():
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    work = WORK / f"smoke-{stamp}"
    work.mkdir(parents=True, exist_ok=True)
    log_path = work / "smoke.log"
    with log_path.open("w") as stream:
        def log(text):
            print(text, flush=True)
            stream.write(text + "\n")
            stream.flush()

        log(f"Adversarial lab smoke check, {stamp}. Evidence: {work}")
        failed = smoke_check.run(work, log)
        log(f"== {'All checks passed' if not failed else f'{len(failed)} checks failed'}")
        for name in failed:
            log(f"   failed: {name}")
        log(f"Log: {log_path}")
    return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("serve", "reset"):
        sub.add_parser(name).add_argument("--run", help="name of the new record (a-z, 0-9, ._-)")
    sub.add_parser("smoke")
    args = parser.parse_args()
    if args.command == "serve":
        return serve(args.run)
    if args.command == "reset":
        return reset(args.run)
    return smoke()


if __name__ == "__main__":
    sys.exit(main())
