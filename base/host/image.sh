#!/bin/sh
# Turn the Void root into a bootable UEFI disk, inside `unshare -r` so every
# file is recorded as owned by root.
#
#   p1  ESP, FAT32, label $ESP_LABEL: a standalone GRUB at \EFI\BOOT\BOOTX64.EFI,
#       the path any UEFI firmware boots with no boot entry
#   p2  Btrfs, label $ROOT_LABEL: the Void root; GRUB reads /boot/grub/grub.cfg here
#
# Inputs: ROOTFS WORK OUT PUBKEY HOSTNAME ROOT_LABEL ESP_LABEL ESP_MIB ROOT_GIB
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

# The boot menu, on the root.
mkdir -p "$R/boot/grub"
cat > "$R/boot/grub/grub.cfg" <<GRUB
set timeout=2
serial --unit=0 --speed=115200
terminal_input console serial
terminal_output console serial
menuentry "CrowbarOS base (Void Linux, kernel $KVER)" {
    search --no-floppy --label $ROOT_LABEL --set=root
    linux /boot/vmlinuz-$KVER root=LABEL=$ROOT_LABEL rw rootfstype=btrfs loglevel=4 console=tty0 console=ttyS0,115200
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
