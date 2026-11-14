#!/usr/bin/env python3
"""Recreate the reviewed GPUI backport from its verified crates.io source."""
import hashlib
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SHA = "979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707"
cache = ROOT / ".upstream-cache"
cache.mkdir(exist_ok=True)
archive = cache / "gpui-0.2.2.crate"
if not archive.exists():
    with urllib.request.urlopen("https://static.crates.io/crates/gpui/gpui-0.2.2.crate", timeout=60) as response:
        archive.write_bytes(response.read())
if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA:
    raise SystemExit("GPUI archive checksum mismatch")
destination = ROOT / "gpui-source"
if destination.exists() and not (destination / ".crowbaros-generated").exists():
    raise SystemExit("Refusing to replace an unrecognized GPUI source directory")
with tempfile.TemporaryDirectory(dir=ROOT) as temp:
    with tarfile.open(archive) as tar:
        tar.extractall(temp, filter="data")
    extracted = Path(temp) / "gpui-0.2.2"
    subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-d", str(extracted),
                    "-i", str(ROOT / "patches/gpui-0.2.2-layer-shell.patch")], check=True)
    (extracted / ".crowbaros-generated").write_text(SHA + "\n")
    if destination.exists():
        shutil.rmtree(destination)
    extracted.rename(destination)
print("Prepared GPUI 0.2.2 with the pinned upstream layer-shell backport")
