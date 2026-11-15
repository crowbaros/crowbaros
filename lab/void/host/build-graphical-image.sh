#!/bin/sh
# Assemble the graphical guest's disk from its Void root, inside `unshare -r`
# so files are recorded as owned by root. One Btrfs filesystem, no partition
# table: the lab boots the kernel directly.
# Inputs: ROOTFS WORK OUT BIN BINARIES PUBKEY ROOT_LABEL ROOT_GIB GUEST_USER
set -eu
S="$WORK/staging"
rm -rf "$S"
cp -a --reflink=auto "$ROOTFS" "$S"
rm -rf "$S/var/cache/xbps"
mkdir -p "$S/var/cache/xbps"

echo crowbar-graphical > "$S/etc/hostname"
cat > "$S/etc/fstab" <<FSTAB
LABEL=$ROOT_LABEL	/	btrfs	rw,relatime	0 0
tmpfs	/tmp	tmpfs	defaults,nosuid,nodev	0 0
FSTAB

# Services: runit starts udevd itself. seatd is the seat manager; there is no
# systemd and no elogind in this root.
SV="$S/etc/runit/runsvdir/default"
rm -f "$SV"/agetty-tty3 "$SV"/agetty-tty4 "$SV"/agetty-tty5 "$SV"/agetty-tty6
for s in agetty-ttyS0 dhcpcd sshd seatd; do ln -sfn "/etc/sv/$s" "$SV/$s"; done

install -d -m 700 "$S/root/.ssh"
install -m 600 "$PUBKEY" "$S/root/.ssh/authorized_keys"

# The person's account. seatd lets members of _seatd open a seat. The account
# is in neither `input` nor `video`: seatd opens the devices, so the person's
# programs get no direct access to them.
chroot "$S" useradd -m -u 1000 -G _seatd "$GUEST_USER"

for name in $BINARIES; do install -D -m 755 "$BIN/$name" "$S/usr/local/bin/$name"; done

rm -f "$OUT"
truncate -s "${ROOT_GIB}G" "$OUT"
mkfs.btrfs -q -f -L "$ROOT_LABEL" --rootdir "$S" "$OUT"
rm -rf "$S"
echo "Built $OUT"
