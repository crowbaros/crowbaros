#!/bin/sh
# Assemble the graphical guest's disk from its Void root, inside `unshare -r`
# so files are recorded as owned by root. One Btrfs filesystem, no partition
# table: the lab boots the kernel directly.
# Inputs: ROOTFS WORK OUT BINARIES (paths) PUBKEY ROOT_LABEL ROOT_GIB REPO_ROOT
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
# `_seatd`; the shell; and one for every client. The compositor check starts
# what it needs itself.
chroot "$S" useradd -m -u 1000 -G _seatd crowbar-compositor
chroot "$S" useradd -m -u 1001 crowbar-shell
chroot "$S" useradd -m -u 1002 crowbar-app

for path in $BINARIES; do install -D -m 755 "$path" "$S/usr/local/bin/$(basename "$path")"; done
install -d "$S/usr/share/crowbaros-shell" "$S/usr/share/applications" "$S/usr/share/fonts/crowbaros" "$S/usr/share/icons/default"
install -m 644 "$REPO_ROOT/apps/shell/assets/wallpaper.png" "$REPO_ROOT/apps/shell/assets/nightwatch.png" "$REPO_ROOT/apps/shell/assets/mark.png" "$S/usr/share/crowbaros-shell/"
if [ -f "$REPO_ROOT/apps/shell/assets/quiet-signal.png" ]; then
  install -m 644 "$REPO_ROOT/apps/shell/assets/quiet-signal.png" "$S/usr/share/crowbaros-shell/quiet-signal.png"
else
  install -m 644 "$REPO_ROOT/docs/branding/crowbaros-brand-kit/wallpapers/quiet-signal-4k.png" "$S/usr/share/crowbaros-shell/quiet-signal.png"
fi
install -m 644 "$REPO_ROOT/apps/studio-settings/packaging/com.crowbaros.StudioSettings.desktop" "$S/usr/share/applications/"
install -m 644 "$REPO_ROOT/apps/shell/fonts/IBMPlexMono-Regular.ttf" "$REPO_ROOT"/apps/shell/fonts/Manrope-*.ttf "$S/usr/share/fonts/crowbaros/"
if [ -x "$S/usr/bin/fc-cache" ]; then
  chroot "$S" fc-cache -f
fi
install -D -m 644 "$REPO_ROOT/base/desktop/foot.ini" "$S/etc/xdg/foot/foot.ini"
install -D -m 755 "$REPO_ROOT/base/desktop/chromium-wrapper" "$S/usr/local/bin/chromium"
install -D -m 644 "$REPO_ROOT/base/desktop/mimeapps.list" "$S/etc/xdg/mimeapps.list"
if [ -f "$S/usr/share/applications/chromium.desktop" ]; then
  sed -i 's|^Exec=\(/usr/bin/\)\?chromium\b|Exec=/usr/local/bin/chromium|g' "$S/usr/share/applications/chromium.desktop"
fi
cat > "$S/usr/share/icons/default/index.theme" <<'CURSOR'
[Icon Theme]
Inherits=Vanilla-DMZ
CURSOR
chmod 644 "$S/usr/share/icons/default/index.theme"

rm -f "$OUT"
truncate -s "${ROOT_GIB}G" "$OUT"
mkfs.btrfs -q -f -L "$ROOT_LABEL" --rootdir "$S" "$OUT"
rm -rf "$S"
echo "Built $OUT"
