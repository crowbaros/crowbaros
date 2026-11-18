#!/usr/bin/env python3
"""What the Void lab's checks share: the pinned static xbps and the package cache.

Stdlib only, no sudo. Downloads live in ~/.cache/crowbaros/void, outside git.
The graphical guest in compositor_guest.py builds its roots with these.

    void_lab.py fetch       download and verify the static xbps
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile

LAB = Path(__file__).resolve().parent
ROOT = LAB.parents[1]
CFG = json.loads((LAB / "config.json").read_text())
CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "crowbaros/void"


def run(*args, **kwargs):
    print("+", shlex.join(str(a) for a in args), flush=True)
    return subprocess.run([str(a) for a in args], check=True, text=True, **kwargs)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def xbps_dir():
    return CACHE / "xbps-static"


def fetch():
    CACHE.mkdir(parents=True, exist_ok=True)
    url = CFG["xbps_static_url"]
    tarball = CACHE / url.rsplit("/", 1)[1]
    if not tarball.exists():
        part = tarball.with_name(tarball.name + ".part")
        run("curl", "-fL", "--retry", "3", "-o", part, url)
        part.replace(tarball)
    if sha256(tarball) != CFG["xbps_static_sha256"]:
        raise SystemExit(f"{tarball} does not match the pinned SHA-256; refusing to use it")
    target = xbps_dir()
    if not (target / "usr/bin/xbps-install.static").exists():
        shutil.rmtree(target, ignore_errors=True)
        target.mkdir()
        with tarfile.open(tarball) as archive:
            archive.extractall(target, filter="data")
    print(f"Verified static xbps at {target}")


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("fetch")
    parser.parse_args()
    fetch()


if __name__ == "__main__":
    main()
