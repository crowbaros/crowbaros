#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
rpm -q crowbaros-shell
install -d -m0755 /etc/crowbaros-lab
case "${1:-system}" in
 system) printf '' > /etc/crowbaros-lab/shell-vulkan.env ;;
 intel-virtio)
  icd=/opt/crowbaros-lab/mesa-intel-virtio/share/vulkan/icd.d/intel_icd.x86_64.json
  test -r "$icd"
  printf 'VK_DRIVER_FILES=%s\n' "$icd" > /etc/crowbaros-lab/shell-vulkan.env ;;
 software) printf 'VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json\n' > /etc/crowbaros-lab/shell-vulkan.env ;;
 *) exit 1 ;;
esac
chmod 0644 /etc/crowbaros-lab/shell-vulkan.env
restorecon /etc/crowbaros-lab/shell-vulkan.env
uid=$(id -u desktop)
as_desktop=(runuser -u desktop -- env "XDG_RUNTIME_DIR=/run/user/$uid" "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$uid/bus")
read -r socket signature < <("${as_desktop[@]}" hyprctl instances -j | python3 -c '
import json,sys
v=json.load(sys.stdin)
if len(v)!=1: raise SystemExit("Expected one Hyprland session")
print(v[0]["wl_socket"],v[0]["instance"])
')
[[ $socket =~ ^wayland-[0-9]+$ && $signature =~ ^[a-zA-Z0-9_]+$ ]] || exit 1
"${as_desktop[@]}" systemctl --user daemon-reload
"${as_desktop[@]}" env "WAYLAND_DISPLAY=$socket" "HYPRLAND_INSTANCE_SIGNATURE=$signature" XDG_CURRENT_DESKTOP=Hyprland XDG_SESSION_TYPE=wayland /usr/bin/crowbaros-shell --start-session
"${as_desktop[@]}" hyprctl -i 0 reload
"${as_desktop[@]}" hyprctl -i 0 configerrors
"${as_desktop[@]}" systemctl --user status --no-pager crowbaros-shell.service
