#!/bin/sh
# Assemble the graphical guest's disk from its Void root, inside `unshare -r`
# so files are recorded as owned by root. One Btrfs filesystem, no partition
# table: the lab boots the kernel directly.
# Inputs: ROOTFS WORK OUT BINARIES (paths) PUBKEY ROOT_LABEL ROOT_GIB REPO_ROOT DESKTOP
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

# Three users, as ADR 0016 has them: the compositor, which alone is in
# `_seatd`; the shell; and one for every client. The desktop image also runs
# them as services. The compositor check starts what it needs itself.
chroot "$S" useradd -m -u 1000 -G _seatd crowbar-compositor
chroot "$S" useradd -m -u 1001 crowbar-shell
chroot "$S" useradd -m -u 1002 crowbar-app
if [ "$DESKTOP" = 1 ]; then
  rm -f "$SV/agetty-tty2"
  for service in compositor shell launcher unlock; do
    install -D -m 755 "$REPO_ROOT/lab/void/guest/$service-run" "$S/etc/sv/crowbar-$service/run"
    ln -sfn "/etc/sv/crowbar-$service" "$SV/crowbar-$service"
  done
  install -D -m 755 "$REPO_ROOT/lab/void/guest/alpha-launcher.py" "$S/usr/local/bin/crowbar-alpha-launcher"
  install -D -m 755 "$REPO_ROOT/lab/void/guest/alpha-shell.py" "$S/usr/local/bin/crowbar-alpha-shell"
  install -D -m 755 "$REPO_ROOT/lab/void/guest/alpha-unlock.py" "$S/usr/local/bin/crowbar-alpha-unlock"
  install -D -m 755 "$REPO_ROOT/lab/void/guest/zzz-lock" "$S/etc/zzz.d/suspend/50-crowbar-lock"
  # The person's account, whose password unlocks the session: the lab's
  # password is `crowbar`. It has no login shell or home to sign in to.
  chroot "$S" useradd -M -u 1003 -s /bin/false crowbar-person
  chroot "$S" usermod -p "$(chroot "$S" /usr/local/bin/crowbar-alpha-unlock hash crowbar)" crowbar-person
fi

for path in $BINARIES; do install -D -m 755 "$path" "$S/usr/local/bin/$(basename "$path")"; done
install -d "$S/usr/share/crowbaros-shell" "$S/usr/share/applications"
install -m 644 "$REPO_ROOT/apps/shell/assets/wallpaper.png" "$REPO_ROOT/apps/shell/assets/mark.png" "$S/usr/share/crowbaros-shell/"
install -m 644 "$REPO_ROOT/apps/studio-settings/packaging/com.crowbaros.StudioSettings.desktop" "$S/usr/share/applications/"

rm -f "$OUT"
truncate -s "${ROOT_GIB}G" "$OUT"
mkfs.btrfs -q -f -L "$ROOT_LABEL" --rootdir "$S" "$OUT"
rm -rf "$S"
echo "Built $OUT"
