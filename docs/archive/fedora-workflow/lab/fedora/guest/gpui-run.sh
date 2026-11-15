#!/usr/bin/env bash
set -euo pipefail
[[ $(hostname) == crowbaros-test ]] || exit 1
rpm -q crowbaros-gpui-probe
driver_env=()
case "${1:-system}" in
  system) ;;
  intel-virtio)
    icd=/opt/crowbaros-lab/mesa-intel-virtio/share/vulkan/icd.d/intel_icd.x86_64.json
    test -r "$icd"
    driver_env+=(--setenv="VK_DRIVER_FILES=$icd")
    ;;
  software)
    driver_env+=(--setenv=VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json)
    ;;
  *) printf 'Expected Vulkan mode: system, intel-virtio, or software\n' >&2; exit 1 ;;
esac
desktop_uid=$(id -u desktop)
runtime="/run/user/$desktop_uid"
as_desktop=(runuser -u desktop -- env "XDG_RUNTIME_DIR=$runtime" "DBUS_SESSION_BUS_ADDRESS=unix:path=$runtime/bus")
wayland_socket=$("${as_desktop[@]}" hyprctl instances -j | python3 -c '
import json, sys
instances = json.load(sys.stdin)
if len(instances) != 1:
    raise SystemExit("Expected exactly one test desktop session")
print(instances[0]["wl_socket"])
')
[[ $wayland_socket =~ ^wayland-[0-9]+$ ]] || exit 1
if "${as_desktop[@]}" systemctl --user is-active --quiet crowbaros-gpui-probe.service; then
  printf 'GPUI probe is already running; its driver has not changed. Stop the user service before switching drivers.\n'
  exit 0
fi
"${as_desktop[@]}" systemd-run --user --unit=crowbaros-gpui-probe --collect \
  --property=Type=exec --setenv="WAYLAND_DISPLAY=$wayland_socket" --setenv=RUST_LOG=info \
  "${driver_env[@]}" \
  /usr/bin/crowbaros-gpui-probe
