#!/bin/sh
# Build and test the desktop (Fulcrum and the GPUI apps) against Void's own
# libraries, inside the Void build root. Runs as fake root in a user and mount
# namespace (`unshare -r -m`), so nothing here needs sudo and no mount outlives
# the script.
# Inputs: B (build root), SRC (repository root), TARGET (cargo target dir),
# PROJECTS (what to build: `path:subdir` for each, where `subdir` is the part
# of TARGET that project builds in, or nothing for TARGET itself),
# CARGO_REGISTRY and CARGO_GIT (the host's downloaded crates; the build is
# offline and takes exactly what Cargo.lock names). CARGO_BUILD_JOBS, if set,
# limits how many jobs Cargo runs at once; unset, it runs one for each CPU.
set -eu
mount --make-rslave /
mkdir -p "$B/src" "$B/target" "$B/cargo/registry" "$B/cargo/git"
mount --rbind /proc "$B/proc"
mount --rbind /dev "$B/dev"
mount --bind "$SRC" "$B/src"
mount --bind "$TARGET" "$B/target"
mount --bind "$CARGO_REGISTRY" "$B/cargo/registry"
mount --bind "$CARGO_GIT" "$B/cargo/git"
chroot "$B" /usr/bin/env -i HOME=/root PATH=/usr/bin:/usr/sbin \
  CARGO_HOME=/cargo ${CARGO_BUILD_JOBS:+CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} PROJECTS="$PROJECTS" \
  sh -ec '
    rustc --version
    for entry in $PROJECTS; do
      project=${entry%%:*}
      export CARGO_TARGET_DIR="/target/${entry#*:}"
      cargo build --release --locked --offline --manifest-path "/src/$project/Cargo.toml"
      cargo test --release --locked --offline --manifest-path "/src/$project/Cargo.toml"
    done
  '
