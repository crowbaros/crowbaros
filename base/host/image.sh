#!/bin/sh
# Turn the Void root into a bootable UEFI disk, inside `unshare -r` so every
# file is recorded as owned by root.
#
#   p1  ESP, FAT32, label $ESP_LABEL: a standalone GRUB at \EFI\BOOT\BOOTX64.EFI,
#       the path any UEFI firmware boots with no boot entry
#   p2  Btrfs, label $ROOT_LABEL: the Void root; GRUB reads /boot/grub/grub.cfg here
#
# Inputs: ROOTFS WORK OUT PUBKEY HOSTNAME ROOT_LABEL ESP_LABEL ESP_MIB ROOT_GIB
# KERNEL_ARGS (added to the kernel's command line),
# and for the desktop DESKTOP (base/desktop) and REPO_ROOT. Its arguments are
# the desktop's programs.
set -eu
R="$WORK/staging"
rm -rf "$R" "$WORK/esp.img" "$WORK/root.img"
cp -a --reflink=auto "$ROOTFS" "$R"
rm -rf "$R/var/cache/xbps"
mkdir -p "$R/var/cache/xbps"

# The kernel and the initramfs that xbps built for it.
ls "$R"/boot/vmlinuz-* >/dev/null 2>&1 || {
  echo "no kernel in $ROOTFS/boot; install the root again with: base/void_base.py build --force" >&2
  exit 1
}
KERNEL=$(cd "$R/boot" && ls vmlinuz-* | sort -V | tail -n 1)
KVER=${KERNEL#vmlinuz-}
test -f "$R/boot/initramfs-$KVER.img" || { echo "no initramfs for kernel $KVER" >&2; exit 1; }

# Identity, mounts, services, and access.
echo "$HOSTNAME" > "$R/etc/hostname"
cat > "$R/etc/fstab" <<FSTAB
LABEL=$ROOT_LABEL	/	btrfs	rw,relatime	0 0
tmpfs	/tmp	tmpfs	defaults,nosuid,nodev	0 0
FSTAB
SV="$R/etc/runit/runsvdir/default"
for s in agetty-ttyS0 dhcpcd sshd; do ln -sfn "/etc/sv/$s" "$SV/$s"; done
install -d -m 700 "$R/root/.ssh"
install -m 600 "$PUBKEY" "$R/root/.ssh/authorized_keys"
# SSH by key alone: the person's account has a password, and it is not for SSH.
printf 'PasswordAuthentication no\nKbdInteractiveAuthentication no\n' > "$R/etc/ssh/sshd_config.d/10-crowbaros.conf"

# The desktop. Four accounts: the compositor, which alone is in `_seatd`; the
# shell; one for every app; and the person's, whose password unlocks the
# desktop. In the lab it is `crowbar`. The person's account has no login shell
# or home yet. runit starts the seat manager, the compositor on virtual
# terminal 2 in place of its getty, the shell, the launcher, and the unlock
# helper; see desktop/.
ln -sfn /etc/sv/seatd "$SV/seatd"
rm -f "$SV/agetty-tty2"
chroot "$R" useradd -m -u 1000 -G _seatd crowbar-compositor
chroot "$R" useradd -m -u 1001 crowbar-shell
chroot "$R" useradd -m -u 1002 crowbar-app
chroot "$R" useradd -M -u 1003 -s /bin/false crowbar-person
for service in compositor shell launcher unlock; do
  install -D -m 755 "$DESKTOP/$service-run" "$R/etc/sv/crowbar-$service/run"
  ln -sfn "/etc/sv/crowbar-$service" "$SV/crowbar-$service"
done
install -D -m 755 "$DESKTOP/launcher.py" "$R/usr/local/bin/crowbar-launcher"
install -D -m 755 "$DESKTOP/shell-connect.py" "$R/usr/local/bin/crowbar-shell-connect"
install -D -m 755 "$DESKTOP/unlock.py" "$R/usr/local/bin/crowbar-unlock"
install -D -m 755 "$DESKTOP/zzz-lock" "$R/etc/zzz.d/suspend/50-crowbar-lock"
HASH=$(chroot "$R" /usr/local/bin/crowbar-unlock hash crowbar)
chroot "$R" usermod -p "$HASH" crowbar-person
for path in "$@"; do install -D -m 755 "$path" "$R/usr/local/bin/$(basename "$path")"; done
install -d "$R/usr/share/crowbaros-shell" "$R/usr/share/applications" "$R/usr/share/fonts/crowbaros" "$R/usr/share/icons/default"
install -m 644 "$REPO_ROOT/apps/shell/assets/wallpaper.png" "$REPO_ROOT/apps/shell/assets/nightwatch.png" "$REPO_ROOT/apps/shell/assets/mark.png" "$REPO_ROOT/apps/shell/assets/quiet-signal.png" "$R/usr/share/crowbaros-shell/"
install -m 644 "$REPO_ROOT/apps/studio-settings/packaging/com.crowbaros.StudioSettings.desktop" "$R/usr/share/applications/"
# The shell's fonts for foot and the apps, and the cursor theme the desktop's
# services name, as the default for anything that names none.
install -m 644 "$REPO_ROOT/apps/shell/fonts/IBMPlexMono-Regular.ttf" "$REPO_ROOT"/apps/shell/fonts/Manrope-*.ttf "$R/usr/share/fonts/crowbaros/"
chroot "$R" fc-cache -f
install -D -m 644 "$DESKTOP/foot.ini" "$R/etc/xdg/foot/foot.ini"
# Chromium is the default browser, native on Wayland through a wrapper.
install -D -m 755 "$DESKTOP/chromium-wrapper" "$R/usr/local/bin/chromium"
install -D -m 644 "$DESKTOP/mimeapps.list" "$R/etc/xdg/mimeapps.list"
sed -i 's|^Exec=\(/usr/bin/\)\?chromium\b|Exec=/usr/local/bin/chromium|g' "$R/usr/share/applications/chromium.desktop"
printf '[Icon Theme]\nInherits=Vanilla-DMZ\n' > "$R/usr/share/icons/default/index.theme"

# The boot menu, on the root.
mkdir -p "$R/boot/grub"
cat > "$R/boot/grub/grub.cfg" <<GRUB
set timeout=2
serial --unit=0 --speed=115200
terminal_input console serial
terminal_output console serial
menuentry "CrowbarOS base (Void Linux, kernel $KVER)" {
    search --no-floppy --label $ROOT_LABEL --set=root
    linux /boot/vmlinuz-$KVER root=LABEL=$ROOT_LABEL rw rootfstype=btrfs loglevel=4 console=tty0 console=ttyS0,115200 $KERNEL_ARGS
    initrd /boot/initramfs-$KVER.img
}
GRUB

# A standalone GRUB whose embedded config finds the root by label and reads
# the menu from it. Built with the guest's own GRUB.
cat > "$R/tmp/early.cfg" <<EARLY
search --no-floppy --label $ROOT_LABEL --set=root
configfile (\$root)/boot/grub/grub.cfg
EARLY
chroot "$R" grub-mkstandalone -O x86_64-efi -o /tmp/BOOTX64.EFI \
  --modules="part_gpt btrfs fat search search_label configfile linux normal serial terminal echo test gzio all_video efi_gop" \
  "boot/grub/grub.cfg=/tmp/early.cfg"
truncate -s "${ESP_MIB}M" "$R/tmp/esp.img"
mkfs.vfat -F 32 -n "$ESP_LABEL" "$R/tmp/esp.img" >/dev/null
chroot "$R" mmd -i /tmp/esp.img ::/EFI ::/EFI/BOOT
chroot "$R" mcopy -i /tmp/esp.img /tmp/BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
mv "$R/tmp/esp.img" "$WORK/esp.img"
rm -f "$R/tmp/early.cfg" "$R/tmp/BOOTX64.EFI"

truncate -s "${ROOT_GIB}G" "$WORK/root.img"
mkfs.btrfs -q -f -L "$ROOT_LABEL" --rootdir "$R" "$WORK/root.img"

# Both partitions into one GPT disk.
ROOT_MIB=$((ROOT_GIB * 1024))
rm -f "$OUT"
truncate -s "$((1 + ESP_MIB + ROOT_MIB + 1))M" "$OUT"
sfdisk -q "$OUT" <<PARTS
label: gpt
start=2048, size=$((ESP_MIB * 2048)), type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="$ESP_LABEL"
start=$(((1 + ESP_MIB) * 2048)), size=$((ROOT_MIB * 2048)), type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, name="$ROOT_LABEL"
PARTS
dd if="$WORK/esp.img" of="$OUT" bs=1M seek=1 conv=notrunc,sparse status=none
dd if="$WORK/root.img" of="$OUT" bs=1M seek=$((1 + ESP_MIB)) conv=notrunc,sparse status=none
rm -f "$WORK/esp.img" "$WORK/root.img"
rm -rf "$R"
echo "Built $OUT with kernel $KVER"
