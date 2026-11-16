#!/usr/bin/env bash
# Protocol checks against the compositor running nested on this host.
#
# It starts the compositor in a window on the current Wayland session and asks
# three kinds of client what they are offered: the shell, an ordinary client,
# and a client started through security-context. `wayland-info` does the
# asking, so the answer does not depend on our own probe.
#
#   checks/nested.sh /path/to/wayland-info
#
# The log goes to .lab/compositor/ in the repository root, outside git.
set -euo pipefail
: "${WAYLAND_DISPLAY:?set WAYLAND_DISPLAY to the Wayland session to nest in}"

here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
info=${1:?usage: checks/nested.sh /path/to/wayland-info}
bin=${CROWBAR_BIN:-$here/target/debug}
out=$root/.lab/compositor
mkdir -p "$out"
log=$out/nested-$(date -u +%Y%m%dT%H%M%SZ).log
work=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$work"' EXIT

# Globals that ADR 0014 says no Tool or app may reach.
forbidden=(
  zwp_virtual_keyboard_manager_v1
  zwlr_virtual_pointer_manager_v1
  zwp_input_method_manager_v2
  zwp_input_method_v1
  zwlr_data_control_manager_v1
  ext_data_control_manager_v1
  ext_image_copy_capture_manager_v1
  ext_output_image_capture_source_manager_v1
  ext_foreign_toplevel_image_capture_source_manager_v1
  zwlr_screencopy_manager_v1
  zwlr_export_dmabuf_manager_v1
  zwlr_layer_shell_v1
)

say() { printf '%s\n' "$*" | tee -a "$log"; }

# start_compositor SHELL_COMMAND...: starts it and sets $socket.
start_compositor() {
  : >"$work/compositor.log"
  "$bin/crowbar-compositor" --backend winit --shell "$*" 2>"$work/compositor.log" &
  compositor=$!
  for _ in $(seq 100); do
    socket=$(sed -n 's/.*compositor is up socket="\([^"]*\)".*/\1/p' "$work/compositor.log")
    [ -n "$socket" ] && return 0
    sleep 0.1
  done
  say "compositor did not start:"; cat "$work/compositor.log" | tee -a "$log"; exit 1
}

stop_compositor() { kill "$compositor" 2>/dev/null || true; wait "$compositor" 2>/dev/null || true; }

interfaces() { sed -n "s/^interface: '\([^']*\)',.*version: *\([0-9]*\),.*/\1 \2/p" "$1" | sort; }

report() { # report NAME FILE
  say ""
  say "== $1: $(interfaces "$2" | wc -l) globals"
  interfaces "$2" | sed 's/^/   /' | tee -a "$log"
}

say "# Nested protocol checks, $(date -u +%FT%TZ)"
say "host: $(uname -sr), session ${XDG_SESSION_TYPE:-unknown}${WAYLAND_DISPLAY:+ on $WAYLAND_DISPLAY}"
say "compositor: $bin/crowbar-compositor  sha256 $(sha256sum "$bin/crowbar-compositor" | cut -d" " -f1)"
say "wayland-info: $($info --version 2>/dev/null || echo "$info")"

# The shell's view. The compositor hands the "shell" its connected socket, so
# wayland-info run as the shell sees what the shell sees.
cat >"$work/shell-info.sh" <<SH
#!/bin/sh
exec "$info" >"$work/shell.txt" 2>&1
SH
chmod +x "$work/shell-info.sh"
start_compositor "$work/shell-info.sh"
for _ in $(seq 50); do [ -s "$work/shell.txt" ] && break; sleep 0.1; done

WAYLAND_DISPLAY=$socket "$info" >"$work/ordinary.txt" 2>&1
WAYLAND_DISPLAY=$socket "$bin/crowbar-probe" security-context "$info" >"$work/confined.txt" 2>&1

report "the shell (socket handed over by the compositor)" "$work/shell.txt"
report "an ordinary client (public socket $socket)" "$work/ordinary.txt"
report "a client started through security-context" "$work/confined.txt"

say ""
say "== forbidden globals"
printf '   %-56s %-9s %-9s %-16s\n' global shell ordinary security-context | tee -a "$log"
fail=0
for name in "${forbidden[@]}"; do
  row=()
  for who in shell ordinary confined; do
    if interfaces "$work/$who.txt" | grep -q "^$name "; then row+=(OFFERED); else row+=(absent); fi
  done
  printf '   %-56s %-9s %-9s %-9s\n' "$name" "${row[@]}" | tee -a "$log"
  # Layer-shell is for the shell alone. Nothing else on the list is for anyone.
  if [ "$name" = zwlr_layer_shell_v1 ]; then
    [ "${row[*]}" = "OFFERED absent absent" ] || fail=1
  else
    [ "${row[*]}" = "absent absent absent" ] || fail=1
  fi
done

# The forbidden list names what must be absent. This compares the whole list
# each client sees with the table in src/policy.rs, so a global that reached
# a client without passing the table shows up here.
say ""
say "== the whole list each client sees, against the table"
confined_set=(wl_compositor wl_output wl_seat wl_shm wl_subcompositor
  wp_fractional_scale_manager_v1 wp_viewporter xdg_wm_base zwp_linux_dmabuf_v1
  zxdg_output_manager_v1)
ordinary_set=("${confined_set[@]}" wl_data_device_manager wp_security_context_manager_v1)
shell_set=("${ordinary_set[@]}" zwlr_layer_shell_v1)
exactly() { # exactly WHO FILE NAME...
  local who=$1 file=$2 seen wanted
  shift 2
  seen=$(interfaces "$file" | cut -d" " -f1 | sort -u | tr "\n" " ")
  wanted=$(printf "%s\n" "$@" | sort -u | tr "\n" " ")
  if [ -n "$seen" ] && [ "$seen" = "$wanted" ]; then
    say "   $who: exactly the table's list"
  else
    say "   $who: NOT the table's list. Saw: $seen"
    fail=1
  fi
}
exactly "the shell" "$work/shell.txt" "${shell_set[@]}"
exactly "an ordinary client" "$work/ordinary.txt" "${ordinary_set[@]}"
exactly "a security-context client" "$work/confined.txt" "${confined_set[@]}"

say ""
say "== an ordinary client asks for a layer surface"
if WAYLAND_DISPLAY=$socket "$bin/crowbar-probe" layer >"$work/layer-ordinary.txt" 2>&1; then
  fail=1
fi
sed 's/^/   /' "$work/layer-ordinary.txt" | tee -a "$log"
stop_compositor

say ""
say "== the shell asks for a layer surface"
cat >"$work/shell-layer.sh" <<SH
#!/bin/sh
exec "$bin/crowbar-probe" layer >"$work/layer-shell.txt" 2>&1
SH
chmod +x "$work/shell-layer.sh"
start_compositor "$work/shell-layer.sh"
for _ in $(seq 50); do [ -s "$work/layer-shell.txt" ] && break; sleep 0.1; done
sed 's/^/   /' "$work/layer-shell.txt" | tee -a "$log"
grep -q "layer surface created" "$work/layer-shell.txt" || fail=1
stop_compositor

say ""
if [ "$fail" = 0 ]; then say "RESULT: pass"; else say "RESULT: FAIL"; fi
say "log: $log"
exit "$fail"
