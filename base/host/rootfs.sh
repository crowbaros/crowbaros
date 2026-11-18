#!/bin/sh
# Install a Void Linux root with the static xbps, as fake root in a user
# namespace (run through `unshare -r`). No host packages or sudo involved.
# Inputs: XBPS (static xbps bin dir), ROOTFS, REPO, KEYS, PKGCACHE, PACKAGES.
# Package downloads go to PKGCACHE, outside the root, so none end up in the image.
set -eu
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS/var/db/xbps/keys"
cp "$KEYS"/*.plist "$ROOTFS/var/db/xbps/keys/"
# shellcheck disable=SC2086
XBPS_ARCH=x86_64 "$XBPS/xbps-install.static" -S -y -r "$ROOTFS" -c "$PKGCACHE" -R "$REPO" $PACKAGES
