#!/usr/bin/env bash
# Protocol checks against the compositor running nested on this host.
#
# It starts the compositor in a window on the current Wayland session and asks
# each kind of client what it is offered: the shell, a stand-in for the
# Warden, and a client of a compartment's listener that the stand-in
# registers. `wayland-info` does the asking, so the answer does not depend on
# our own probe. Then the client of the listener asks for what it is not
# offered.
#
#   checks/nested.sh /path/to/wayland-info
#
# Every process here runs as one user, so this check cannot show a peer
# refused on its user ID. The tests and the Void guest check do.
#
# The log goes to .lab/compositor/ in the repository root, outside git.
set -euo pipefail
: "${WAYLAND_DISPLAY:?set WAYLAND_DISPLAY to the Wayland session to nest in}"
: "${XDG_RUNTIME_DIR:?set XDG_RUNTIME_DIR to the runtime directory of that session}"

here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/.." && pwd)
info=${1:?usage: checks/nested.sh /path/to/wayland-info}
bin=${CROWBAR_BIN:-$here/target/debug}
out=$root/.lab/compositor
mkdir -p "$out"
log=$out/nested-$(date -u +%Y%m%dT%H%M%SZ).log
work=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$work"' EXIT

say() { printf '%s\n' "$*" | tee -a "$log"; }
fail=0
check() { # check NAME COMMAND...: the command's success is the result
  local name=$1
  shift
  if "$@"; then say "   PASS: $name"; else say "   FAIL: $name"; fail=1; fi
}

interfaces() { sed -n "s/^interface: '\([^']*\)',.*/\1/p" "$1" | sort -u; }
exactly() { # exactly FILE NAME...: the file lists these interfaces and no other
  local file=$1
  shift
  [ -s "$file" ] && [ "$(interfaces "$file" | tr '\n' ' ')" = "$(printf '%s\n' "$@" | sort -u | tr '\n' ' ')" ]
}
report() { # report NAME FILE
  say ""
  say "== $1: $(interfaces "$2" | wc -l) globals"
  interfaces "$2" | sed 's/^/   /' | tee -a "$log"
}

# What each client is offered: the rows of ADR 0020's table, less the globals
# the compositor does not create yet. tests/table.rs holds the same lists.
compartment=(wl_compositor wl_output wl_seat wl_shm wl_subcompositor wp_viewporter xdg_wm_base)
shell=("${compartment[@]}" zwlr_layer_shell_v1)
warden=(wp_security_context_manager_v1)
# The "Nobody" row of the table, read from the compositor's source.
mapfile -t nobody < <(sed -n '/^pub const NOBODY/,/^];/p' "$here/src/policy.rs" | grep -o '"[a-z0-9_]*"' | tr -d '"')
[ "${#nobody[@]}" -ge 20 ] || { say "could not read the Nobody row from src/policy.rs"; exit 1; }

say "# Nested protocol checks, $(date -u +%FT%TZ)"
say "host: $(uname -sr), session ${XDG_SESSION_TYPE:-unknown} on $WAYLAND_DISPLAY"
say "compositor: $bin/crowbar-compositor  sha256 $(sha256sum "$bin/crowbar-compositor" | cut -d" " -f1)"
say "wayland-info: $($info --version 2>/dev/null || echo "$info")"

sockets_before=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s | sort)
uid=$(id -u)
"$bin/crowbar-compositor" --backend winit \
  --shell-socket "$work/shell" --shell-uid "$uid" \
  --warden-socket "$work/warden" --warden-uid "$uid" 2>"$work/compositor.log" &
for _ in $(seq 100); do
  grep -q "compositor is up" "$work/compositor.log" && break
  sleep 0.1
done
if ! grep -q "compositor is up" "$work/compositor.log"; then
  say "compositor did not start:"
  tee -a "$log" <"$work/compositor.log"
  exit 1
fi
# The nested backend offers linux-dmabuf only when the host names a render node.
grep -q "linux-dmabuf is not offered" "$work/compositor.log" || shell+=(zwp_linux_dmabuf_v1)

as_shell() { WAYLAND_DISPLAY=$work/shell "$@"; }
as_warden() { WAYLAND_DISPLAY=$work/warden "$@"; }
# in_compartment ID COMMAND...: the stand-in registers a listener for a new
# compartment and runs the command as a client of it.
in_compartment() {
  local id=$1
  shift
  as_warden "$bin/crowbar-probe" warden org.crowbaros.Probe "$id" "$work/$id" "$@"
}

as_shell "$info" >"$work/shell.txt" 2>&1 || true
as_warden "$info" >"$work/warden.txt" 2>&1 || true
in_compartment compartment-1 "$info" >"$work/compartment.txt" 2>&1 || true

report "the shell (the shell's socket)" "$work/shell.txt"
report "a stand-in for the Warden (the Warden's socket)" "$work/warden.txt"
report "a client of a compartment's listener" "$work/compartment.txt"

say ""
say "== the whole list each client sees, against the table"
check "the shell is offered exactly the table's list" exactly "$work/shell.txt" "${shell[@]}"
check "the Warden is offered exactly the table's list" exactly "$work/warden.txt" "${warden[@]}"
check "a client of a listener is offered exactly the table's list" exactly "$work/compartment.txt" "${compartment[@]}"
check "the compositor took the listener with its principal ID and compartment ID" \
  grep -q "compartment listener registered principal=org.crowbaros.Probe compartment=compartment-1" "$work/compositor.log"

say ""
say "== ADR 0016, attack 11: no socket any process can find"
check "the compositor made no socket in the runtime directory" \
  [ "$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s | sort)" = "$sockets_before" ]

say ""
say "== ADR 0020, attack 1, and ADR 0016, attack 12: a client of a listener asks for what it is not offered"
forbidden=("${nobody[@]}" zwlr_layer_shell_v1 wp_security_context_manager_v1 zwp_linux_dmabuf_v1)
in_compartment compartment-2 "$bin/crowbar-probe" guess "${forbidden[@]}" >"$work/guess.txt" 2>&1 && guessed=0 || guessed=$?
sed 's/^/   /' "$work/guess.txt" | grep -v "^   listening at" | tee -a "$log"
check "each of the ${#forbidden[@]} is refused at every global number it could guess" \
  [ "$guessed" = 0 -a "$(grep -c ": refused at each of" "$work/guess.txt")" = "${#forbidden[@]}" ]
in_compartment compartment-3 "$bin/crowbar-probe" layer >"$work/layer-compartment.txt" 2>&1 || true
check "a client of a listener cannot create a layer surface" grep -q "not offered" "$work/layer-compartment.txt"
in_compartment compartment-4 "$bin/crowbar-probe" warden nested nested-1 "$work/nested" true >"$work/nested.txt" 2>&1 || true
check "a client of a listener cannot register a listener of its own" grep -q "not offered" "$work/nested.txt"
as_shell "$bin/crowbar-probe" layer >"$work/layer-shell.txt" 2>&1 || true
check "the shell can create a layer surface" grep -q "layer surface created" "$work/layer-shell.txt"

say ""
if [ "$fail" = 0 ]; then say "RESULT: pass"; else say "RESULT: FAIL"; fi
say "log: $log"
exit "$fail"
