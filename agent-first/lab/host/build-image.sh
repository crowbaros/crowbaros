#!/bin/sh
# Assemble a bootable UEFI disk image from the Void root, inside `unshare -r`
# so files are recorded as owned by root. Layout:
#   p1 ESP (FAT32): standalone GRUB that loads /crowbar/grub.cfg from p2
#   p2 Btrfs, label $ROOT_LABEL: /gens/1 subvolume + /crowbar records
# Inputs: ROOTFS WORK OUT CROWBAR STATE PUBKEY ROOT_LABEL ESP_MIB ROOT_GIB
set -eu
STAGING="$WORK/staging"
G1="$STAGING/gens/1"
rm -rf "$STAGING" "$WORK/esp.img" "$WORK/root.img"
mkdir -p "$STAGING/gens" "$STAGING/crowbar"
cp -a --reflink=auto "$ROOTFS" "$G1"
# Package downloads from an older rootfs build do not belong in the image.
rm -rf "$G1/var/cache/xbps"
mkdir -p "$G1/var/cache/xbps"

# Identity, fstab, services, access.
echo crowbar-lab > "$G1/etc/hostname"
cat > "$G1/etc/fstab" <<FSTAB
# Root is whichever generation subvolume the boot menu selected (rootflags=subvol=...).
LABEL=$ROOT_LABEL	/	btrfs	rw,relatime	0 0
tmpfs	/tmp	tmpfs	defaults,nosuid,nodev	0 0
FSTAB
SV="$G1/etc/runit/runsvdir/default"
rm -f "$SV"/agetty-tty2 "$SV"/agetty-tty3 "$SV"/agetty-tty4 "$SV"/agetty-tty5 "$SV"/agetty-tty6
for s in agetty-ttyS0 dhcpcd sshd; do ln -sfn "/etc/sv/$s" "$SV/$s"; done
install -d -m 700 "$G1/root/.ssh"
install -m 600 "$PUBKEY" "$G1/root/.ssh/authorized_keys"
install -D -m 755 "$CROWBAR" "$G1/usr/local/bin/crowbar"
install -d "$G1/root/states"
cp "$(dirname "$STATE")"/*.toml "$G1/root/states/"

# Register generation 1 and render the boot menu with the same engine the
# guest uses.
"$CROWBAR" --top "$STAGING" init-image "$STATE"

# Standalone GRUB with an embedded config that finds the root by label.
cat > "$G1/tmp/early.cfg" <<GRUB
search --no-floppy --label $ROOT_LABEL --set=root
set prefix=(\$root)/crowbar
configfile (\$root)/crowbar/grub.cfg
GRUB
chroot "$G1" grub-mkstandalone -O x86_64-efi -o /tmp/BOOTX64.EFI \
  --modules="part_gpt btrfs fat search search_label configfile linux normal serial terminal echo test" \
  "boot/grub/grub.cfg=/tmp/early.cfg"
truncate -s "${ESP_MIB}M" "$G1/tmp/esp.img"
mkfs.vfat -F 32 -n CROWBAR-EFI "$G1/tmp/esp.img" >/dev/null
chroot "$G1" mmd -i /tmp/esp.img ::/EFI ::/EFI/BOOT
chroot "$G1" mcopy -i /tmp/esp.img /tmp/BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
mv "$G1/tmp/esp.img" "$WORK/esp.img"
rm -f "$G1/tmp/early.cfg" "$G1/tmp/BOOTX64.EFI"

truncate -s "${ROOT_GIB}G" "$WORK/root.img"
mkfs.btrfs -q -f -L "$ROOT_LABEL" --rootdir "$STAGING" --subvol rw:gens/1 "$WORK/root.img"

ROOT_MIB=$((ROOT_GIB * 1024))
rm -f "$OUT"
truncate -s "$((1 + ESP_MIB + ROOT_MIB + 1))M" "$OUT"
sfdisk -q "$OUT" <<PARTS
label: gpt
start=2048, size=$((ESP_MIB * 2048)), type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="CROWBAR-EFI"
start=$(((1 + ESP_MIB) * 2048)), size=$((ROOT_MIB * 2048)), type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="$ROOT_LABEL"
PARTS
dd if="$WORK/esp.img" of="$OUT" bs=1M seek=1 conv=notrunc,sparse status=none
dd if="$WORK/root.img" of="$OUT" bs=1M seek=$((1 + ESP_MIB)) conv=notrunc,sparse status=none
rm -f "$WORK/esp.img" "$WORK/root.img"
rm -rf "$STAGING"
echo "Built $OUT"
