#!/bin/sh
# Install a Void Linux root with the static xbps, as fake root in a user
# namespace (run through `unshare -r --mount`). No host packages or sudo involved.
# Inputs: XBPS (static xbps bin dir), ROOTFS, REPOS (space-separated), KEYS,
# PKGCACHE, PACKAGES.
# Package downloads go to PKGCACHE, outside the root, so none end up in the image.
set -eu
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS/var/db/xbps/keys" "$ROOTFS/dev" "$ROOTFS/etc/dracut.conf.d"
cp "$KEYS"/*.plist "$ROOTFS/var/db/xbps/keys/"
# Packages' scripts run in the root, and need a real /dev: without one,
# /dev/null is a plain file, and NVIDIA's DKMS build reads garbage from it and
# fails. The bind ends with this mount namespace, so nothing of it is left in
# the root.
mount --rbind /dev "$ROOTFS/dev"
# With /dev there, dracut would make an initramfs for this host's disks and
# drivers. The image boots on other machines, so it gets the generic one.
echo 'hostonly="no"' > "$ROOTFS/etc/dracut.conf.d/10-crowbaros.conf"
set --
for repo in $REPOS; do set -- "$@" -R "$repo"; done
# shellcheck disable=SC2086
XBPS_ARCH=x86_64 "$XBPS/xbps-install.static" -S -y -r "$ROOTFS" -c "$PKGCACHE" "$@" $PACKAGES
