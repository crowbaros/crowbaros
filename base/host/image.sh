#!/bin/sh
# Turn the Void root into a bootable UEFI disk, inside `unshare -r --mount` so
# every file is recorded as owned by root, and no mount outlives the script.
#
#   p1  ESP, FAT32, label $ESP_LABEL: a standalone GRUB at \EFI\BOOT\BOOTX64.EFI,
#       the path any UEFI firmware boots with no boot entry
#   p2  Btrfs, label $ROOT_LABEL: the Void root; GRUB reads /boot/grub/grub.cfg here
#
# Inputs: ROOTFS WORK OUT (void_base.py moves the disk from there into place
# once this script has finished it) PUBKEY HOSTNAME ROOT_LABEL ESP_LABEL ESP_MIB ROOT_GIB,
# KVER (the kernel to boot, which void_base.py checked), KERNEL_ARGS (added to
# the kernel's command line), NVIDIA_OPTIONS (for NVIDIA's `nvidia` module),
# and for the desktop DESKTOP (base/desktop), REPO_ROOT, and OPENCODE (the
# pinned OpenCode binary, installed unchanged on PATH as /usr/local/bin/opencode).
# The person's account is PERSON_UID, PERSON, and PERSON_FULL_NAME, with
# PERSON_PASSWORD_HASH from the account file or, for the lab account,
# PERSON_PASSWORD, and PERSON_SSH_KEYS, its public keys from the account file,
# one per line. Its arguments are the desktop's programs. For the lab's image,
# LAB_PACKAGES are installed with the static xbps in XBPS, from REPOS, through
# the package cache PKGCACHE, and STUB_MODEL (if set) is installed for checks
# and omitted from every person's image.
set -eu
R="$WORK/staging"
rm -rf "$R" "$WORK/esp.img" "$WORK/root.img"
cp -a --reflink=auto "$ROOTFS" "$R"
rm -rf "$R/var/cache/xbps"
mkdir -p "$R/var/cache/xbps"

# The kernel and the initramfs that xbps built for it.
test -f "$R/boot/vmlinuz-$KVER" || {
  echo "no kernel $KVER in $ROOTFS/boot; install the root again with: base/void_base.py build --force" >&2
  exit 1
}
test -f "$R/boot/initramfs-$KVER.img" || { echo "no initramfs for kernel $KVER" >&2; exit 1; }

# Identity, mounts, services, and access.
echo "$HOSTNAME" > "$R/etc/hostname"
cat > "$R/etc/fstab" <<FSTAB
LABEL=$ROOT_LABEL	/	btrfs	rw,relatime	0 0
tmpfs	/tmp	tmpfs	defaults,nosuid,nodev	0 0
FSTAB
SV="$R/etc/runit/runsvdir/default"
for s in agetty-ttyS0 dhcpcd sshd; do ln -sfn "/etc/sv/$s" "$SV/$s"; done
# The system's log: socklog takes what services send to /dev/log, nanoklogd
# the kernel's messages, and svlogd keeps both in /var/log/socklog on the root,
# in rotated files, so a boot that went wrong can be read on the next one.
for s in socklog-unix nanoklogd; do ln -sfn "/etc/sv/$s" "$SV/$s"; done
install -d -m 700 "$R/root/.ssh"
install -m 600 "$PUBKEY" "$R/root/.ssh/authorized_keys"
# SSH by key alone: the person's account has a password, and it is not for SSH.
printf 'PasswordAuthentication no\nKbdInteractiveAuthentication no\n' > "$R/etc/ssh/sshd_config.d/10-crowbaros.conf"

# NVIDIA's module saves all video memory across a suspend, in a file under
# /var/tmp, on the root's disk; /tmp is in memory. Its own defaults keep only
# some, and the desktop wakes grey. The module loads from the root, not the
# initramfs, so it reads this. Not named nvidia.conf, which would hide the
# package's own in /usr/lib/modprobe.d, the one that blacklists nouveau.
echo "options nvidia $NVIDIA_OPTIONS" > "$R/etc/modprobe.d/crowbaros-nvidia.conf"

# Real machines. Wi-Fi is iwd, which joins a network; dhcpcd, which already
# serves every interface, gets the address. iwd answers on the system bus, to
# root and to the `netdev` group, which the person's account joins below. On
# every boot, once the root is writable, the root grows to fill its disk.
for s in dbus iwd bluetoothd; do ln -sfn "/etc/sv/$s" "$SV/$s"; done
chroot "$R" groupadd -r netdev
chroot "$R" getent group bluetooth >/dev/null || chroot "$R" groupadd -r bluetooth
install -D -m 755 "$REPO_ROOT/base/system/grow-root" "$R/usr/local/sbin/crowbar-grow-root"
cat > "$R/etc/runit/core-services/04-crowbar-grow-root.sh" <<'GROW'
# Grow the root to fill its disk: see /usr/local/sbin/crowbar-grow-root.
[ -n "$IS_CONTAINER" ] && return 0
msg "Growing the root to fill its disk..."
/usr/local/sbin/crowbar-grow-root || msg_warn "The root could not grow to fill its disk"
GROW

# The desktop. Three accounts: the compositor, which alone is in `_seatd`; the
# shell; and the person's, with a home and a login shell, in `netdev` to join
# Wi-Fi and `wheel` for sudo, whose password unlocks the desktop and as whom
# every app runs. The socklog group lets Monitor read system logs using Void's
# existing read permission. Its UID is fixed, as the desktop's services find it by UID.
# runit starts the seat manager, the compositor on virtual terminal 2 in place
# of its getty, the shell, the launcher, as the person, the unlock helper, and
# the service that reboots for Ctrl+Alt+Del; see desktop/ and helpers/. The
# compositor's service stops a compositor its openvt left behind.
ln -sfn /etc/sv/seatd "$SV/seatd"
rm -f "$SV/agetty-tty2"
chroot "$R" useradd -m -u 1000 -G _seatd crowbar-compositor
chroot "$R" useradd -m -u 1001 crowbar-shell
chroot "$R" useradd -m -u "$PERSON_UID" -G netdev,wheel,audio,bluetooth,socklog -s /bin/bash -c "$PERSON_FULL_NAME" "$PERSON"
for service in compositor shell launcher unlock ctrl-alt-del lever pipewire monitor-status; do
  install -D -m 755 "$DESKTOP/$service-run" "$R/etc/sv/crowbar-$service/run"
  ln -sfn "/etc/sv/crowbar-$service" "$SV/crowbar-$service"
done
install -m 755 "$DESKTOP/compositor-finish" "$R/etc/sv/crowbar-compositor/finish"
for path in "$@"; do install -D -m 755 "$path" "$R/usr/local/bin/$(basename "$path")"; done
# OpenCode, the bundled harness (ADR 0036), installed unchanged on PATH.
install -D -m 755 "$OPENCODE" "$R/usr/local/bin/opencode"
# The four helpers are one program, which takes the name it runs by.
for helper in unlock launcher shell-connect ctrl-alt-del; do
  ln -sfn crowbar-helpers "$R/usr/local/bin/crowbar-$helper"
done
install -D -m 755 "$DESKTOP/zzz-lock" "$R/etc/zzz.d/suspend/50-crowbar-lock"
# The person is in `wheel`, so sudo takes their password. Rebooting and powering
# off take none, as on a desktop with logind: `reboot` and `poweroff` in
# /usr/local/bin ask sudo for them.
install -m 440 "$DESKTOP/sudoers" "$R/etc/sudoers.d/10-crowbaros"
sed -i "s/@PERSON@/$PERSON/" "$R/etc/sudoers.d/10-crowbaros"
install -D -m 755 "$DESKTOP/power" "$R/usr/local/bin/reboot"
ln -sfn reboot "$R/usr/local/bin/poweroff"
chroot "$R" visudo -cq
HASH=$PERSON_PASSWORD_HASH
if [ -z "$HASH" ]; then HASH=$(chroot "$R" /usr/local/bin/crowbar-unlock hash "$PERSON_PASSWORD"); fi
chroot "$R" usermod -p "$HASH" "$PERSON"
# SSH as the person takes the build's key, as root's does, and the keys the
# account file lists. Whoever holds the build's key is root already.
install -d -m 700 "$R/home/$PERSON/.ssh"
{ cat "$PUBKEY"; printf '%s' "$PERSON_SSH_KEYS"; } > "$R/home/$PERSON/.ssh/authorized_keys"
chmod 600 "$R/home/$PERSON/.ssh/authorized_keys"
chroot "$R" chown -R "$PERSON:$PERSON" "/home/$PERSON/.ssh"
install -d "$R/usr/share/crowbaros-shell" "$R/usr/share/applications" "$R/usr/share/fonts/crowbaros" "$R/usr/share/icons/default"
install -m 644 "$REPO_ROOT/apps/shell/assets/wallpaper.png" "$REPO_ROOT/apps/shell/assets/nightwatch.png" "$REPO_ROOT/apps/shell/assets/mark.png" "$REPO_ROOT/apps/shell/assets/quiet-signal.png" "$R/usr/share/crowbaros-shell/"
install -m 644 "$REPO_ROOT/apps/studio-settings/packaging/com.crowbaros.StudioSettings.desktop" "$R/usr/share/applications/"
install -m 644 "$REPO_ROOT/apps/editor/packaging/com.crowbaros.Editor.desktop" "$R/usr/share/applications/"
install -m 644 "$REPO_ROOT/apps/monitor/packaging/com.crowbaros.Monitor.desktop" "$R/usr/share/applications/"
install -m 644 "$REPO_ROOT/apps/files/packaging/com.crowbaros.Files.desktop" "$R/usr/share/applications/"
install -m 644 "$DESKTOP/com.crowbaros.AgentSignIn.desktop" "$R/usr/share/applications/"
install -D -m 644 "$REPO_ROOT/apps/files/packaging/com.crowbaros.Files.svg" "$R/usr/share/icons/hicolor/scalable/apps/com.crowbaros.Files.svg"
# The shell's fonts for foot and the apps, and the cursor theme the desktop's
# services name, as the default for anything that names none.
install -m 644 "$REPO_ROOT/apps/shell/fonts/IBMPlexMono-Regular.ttf" "$REPO_ROOT"/apps/shell/fonts/Manrope-*.ttf "$R/usr/share/fonts/crowbaros/"
chroot "$R" fc-cache -f
install -D -m 644 "$DESKTOP/foot.ini" "$R/etc/xdg/foot/foot.ini"
# PipeWire spawns WirePlumber and pipewire-pulse, and takes default ALSA output.
install -d "$R/etc/pipewire/pipewire.conf.d" "$R/etc/alsa/conf.d"
ln -sfn /usr/share/examples/wireplumber/10-wireplumber.conf "$R/etc/pipewire/pipewire.conf.d/10-wireplumber.conf"
ln -sfn /usr/share/examples/pipewire/20-pipewire-pulse.conf "$R/etc/pipewire/pipewire.conf.d/20-pipewire-pulse.conf"
ln -sfn /usr/share/alsa/alsa.conf.d/50-pipewire.conf "$R/etc/alsa/conf.d/50-pipewire.conf"
ln -sfn /usr/share/alsa/alsa.conf.d/99-pipewire-default.conf "$R/etc/alsa/conf.d/99-pipewire-default.conf"
# mpv is the default video/audio player, native on Wayland with PipeWire audio.
install -D -m 644 "$DESKTOP/mpv.conf" "$R/etc/mpv/mpv.conf"
# Chromium is the default browser, native on Wayland through a wrapper.
install -D -m 755 "$DESKTOP/chromium-wrapper" "$R/usr/local/bin/chromium"
install -D -m 644 "$DESKTOP/mimeapps.list" "$R/etc/xdg/mimeapps.list"
sed -i 's|^Exec=\(/usr/bin/\)\?chromium\b|Exec=/usr/local/bin/chromium|g' "$R/usr/share/applications/chromium.desktop"
printf '[Icon Theme]\nInherits=Vanilla-DMZ\n' > "$R/usr/share/icons/default/index.theme"

# The lab's image alone gets LAB_PACKAGES, which its checks need in the guest
# and a person's image never has. They come from the root's own package index,
# so they match what the root was installed with, and through the same cache.
# Their scripts get the host's /dev, as the root's install does: Python's
# byte-compiles its library, which needs /dev/shm. It is unbound before the
# root is packed.
if [ -n "$LAB_PACKAGES" ]; then
  mount --make-rslave /
  mount --rbind /dev "$R/dev"
  # shellcheck disable=SC2086
  XBPS_ARCH=x86_64 "$XBPS/xbps-install.static" -y -r "$R" -c "$PKGCACHE" -R "$REPOS" $LAB_PACKAGES
  # The host's mounts under /dev come locked to the bind, so they leave with it.
  umount -l "$R/dev"
fi
if [ -n "${STUB_MODEL:-}" ]; then
  install -D -m 755 "$STUB_MODEL" "$R/usr/local/lib/crowbar-lab/stub-model.py"
fi

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
