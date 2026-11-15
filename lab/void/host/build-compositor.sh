#!/bin/sh
# Build the compositor against Void's own libraries, inside the Void build
# root. Runs as fake root in a user and mount namespace (`unshare -r -m`), so
# nothing here needs sudo and no mount outlives the script.
# Inputs: B (build root), SRC (compositor source), TARGET (cargo target dir),
# CARGO_REGISTRY and CARGO_GIT (the host's downloaded crates; the build is
# offline and takes exactly what Cargo.lock names).
set -eu
mkdir -p "$B/src" "$B/target" "$B/cargo/registry" "$B/cargo/git"
mount --rbind /proc "$B/proc"
mount --rbind /dev "$B/dev"
mount --bind "$SRC" "$B/src"
mount --bind "$TARGET" "$B/target"
mount --bind "$CARGO_REGISTRY" "$B/cargo/registry"
mount --bind "$CARGO_GIT" "$B/cargo/git"
chroot "$B" /usr/bin/env -i HOME=/root PATH=/usr/bin:/usr/sbin \
  CARGO_HOME=/cargo CARGO_TARGET_DIR=/target \
  sh -c 'rustc --version && cargo build --release --locked --offline --manifest-path /src/Cargo.toml'
