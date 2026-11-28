#!/usr/bin/env python3
"""Recreate the reviewed GPUI backport from its verified crates.io source.

A source already prepared from the same archive and the same patches is left as
it is. Cargo rebuilds a path crate whose files are newer than its last build,
so writing the same source again would rebuild GPUI for nothing. A source
that differs from what the script made, as when someone has edited it, is made
again."""
import hashlib
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
URL = "https://static.crates.io/crates/gpui/gpui-0.2.2.crate"
SHA = "979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707"
# Applied in this order. See patches/README.md.
PATCHES = ["gpui-0.2.2-layer-shell.patch", "gpui-0.2.2-still-frames.patch",
           "gpui-0.2.2-closed-windows.patch", "gpui-0.2.2-builds-without-warnings.patch",
           "gpui-0.2.2-image-clipboard.patch", "gpui-0.2.2-optional-axis-source.patch",
           "gpui-0.2.2-emoji-families.patch"]
MARKER = ".crowbaros-generated"


def recipe(root, sha, patches):
    """What a prepared source is made from: the archive, then each patch by its content."""
    lines = [sha]
    for patch in patches:
        lines.append(f"{patch} {hashlib.sha256((root / 'patches' / patch).read_bytes()).hexdigest()}")
    return "\n".join(lines) + "\n"


def tree(directory):
    """A digest of every file under `directory` but the marker: its path, its kind, and its bytes."""
    digest = hashlib.sha256()
    for path in sorted(directory.rglob("*")):
        relative = path.relative_to(directory).as_posix()
        if relative == MARKER:
            continue
        if path.is_symlink():
            digest.update(f"link {relative} {path.readlink()}\0".encode())
        elif path.is_file():
            digest.update(f"file {relative} {path.stat().st_mode & 0o111:o}\0".encode())
            digest.update(hashlib.sha256(path.read_bytes()).digest())
        else:
            digest.update(f"dir {relative}\0".encode())
    return f"tree {digest.hexdigest()}\n"


def prepare(root=ROOT, url=URL, sha=SHA, patches=PATCHES):
    """Make `root`/gpui-source, and say whether it had to be made again."""
    cache = root / ".upstream-cache"
    cache.mkdir(exist_ok=True)
    archive = cache / "gpui-0.2.2.crate"
    if not archive.exists():
        with urllib.request.urlopen(url, timeout=60) as response:
            archive.write_bytes(response.read())
    if hashlib.sha256(archive.read_bytes()).hexdigest() != sha:
        raise SystemExit("GPUI archive checksum mismatch")
    destination = root / "gpui-source"
    marker = destination / MARKER
    if destination.exists() and not marker.exists():
        raise SystemExit("Refusing to replace an unrecognized GPUI source directory")
    wanted = recipe(root, sha, patches)
    if marker.exists() and marker.read_text() == wanted + tree(destination):
        return False
    with tempfile.TemporaryDirectory(dir=root) as temp:
        with tarfile.open(archive) as tar:
            tar.extractall(temp, filter="data")
        extracted = Path(temp) / "gpui-0.2.2"
        for patch in patches:
            subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-d", str(extracted),
                            "-i", str(root / "patches" / patch)], check=True)
        (extracted / MARKER).write_text(wanted + tree(extracted))
        if destination.exists():
            shutil.rmtree(destination)
        extracted.rename(destination)
    return True


if __name__ == "__main__":
    if prepare():
        print(f"Prepared GPUI 0.2.2 with the pinned upstream layer-shell backport and our {len(PATCHES) - 1} patches")
    else:
        print("GPUI 0.2.2 is already prepared from these patches")
