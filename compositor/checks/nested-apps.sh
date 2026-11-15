#!/usr/bin/env bash
# Runs the GPUI shell and Studio Settings under the compositor, nested on this
# host, and watches that they keep drawing.
#
# GPUI 0.2.2 has a known failure on NVIDIA under Smithay compositors: the
# first frame draws and then nothing updates. A client that has stopped
# drawing stops committing, so this check counts commits per surface every
# five seconds for a minute. Midway it starts a second window on the host so
# the host resizes ours, which is also what was reported to un-stick a frozen
# client.
#
#   checks/nested-apps.sh /path/to/crowbaros-shell /path/to/crowbaros-studio-settings
#
# The shell reads its wallpaper from /usr/share/crowbaros-shell. Nothing is
# installed on the host: the shell runs in a private mount namespace where the
# repository's assets sit at that path.
set -euo pipefail
: "${WAYLAND_DISPLAY:?set WAYLAND_DISPLAY to the Wayland session to nest in}"

here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/.." && pwd)
shell=${1:?usage: checks/nested-apps.sh SHELL_BINARY STUDIO_BINARY}
studio=${2:?usage: checks/nested-apps.sh SHELL_BINARY STUDIO_BINARY}
bin=${CROWBAR_BIN:-$here/target/debug}
out=$root/.lab/compositor
mkdir -p "$out"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
log=$out/nested-apps-$stamp.log
work=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$work"' EXIT

say() { printf '%s\n' "$*" | tee -a "$log"; }

mkdir -p "$work/upper/crowbaros-shell" "$work/ovl-work"
cp "$root/apps/shell/assets/wallpaper.png" "$root/apps/shell/assets/mark.png" "$work/upper/crowbaros-shell/"
cat >"$work/shell.sh" <<SH
#!/bin/sh
exec unshare --user --map-root-user --mount sh -c '
  mount -t overlay overlay -o lowerdir=/usr/share,upperdir=$work/upper,workdir=$work/ovl-work /usr/share &&
  RUST_LOG=info exec "$shell"'
SH
cat >"$work/studio.sh" <<SH
#!/bin/sh
RUST_LOG=info exec "$studio"
SH
chmod +x "$work/shell.sh" "$work/studio.sh"

capture() { # capture NAME: our own window only, through the host's toplevel capture
  command -v grim >/dev/null || return 0
  local id
  id=$("$bin/crowbar-probe" toplevels 2>/dev/null | awk -F'\t' '$3=="CrowbarOS compositor spike"{print $1}' | head -1)
  [ -n "$id" ] || return 0
  grim -T "$id" "$out/nested-apps-$stamp-$1.png" 2>/dev/null || return 0
  say "capture $1: $(file -b "$out/nested-apps-$stamp-$1.png" | cut -d, -f2 | tr -d ' ')"
}

say "# The GPUI apps under the nested compositor, $(date -u +%FT%TZ)"
say "host: $(uname -sr)"
say "gpu: $(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null || echo 'no nvidia-smi')"
say "compositor: sha256 $(sha256sum "$bin/crowbar-compositor" | cut -d' ' -f1)"
say "shell: sha256 $(sha256sum "$shell" | cut -d' ' -f1)"
say "studio: sha256 $(sha256sum "$studio" | cut -d' ' -f1)"

# Both apps log at info, so the warning blade prints for a stale swapchain
# would reach the log. The compositor's own filter is set apart from theirs.
"$bin/crowbar-compositor" --backend winit --shell "$work/shell.sh" --spawn "$work/studio.sh" >"$work/run.log" 2>&1 &
main=$!
sleep 10
capture 1-start
say "starting a second host window, which makes the host resize ours"
timeout 7 "$bin/crowbar-compositor" --backend winit >/dev/null 2>&1 &
sleep 3
capture 2-resized
sleep 7
capture 3-restored
sleep 41
kill "$main" 2>/dev/null || true
wait "$main" 2>/dev/null || true

say ""
say "== GPU the apps chose"
grep -h "Adapter:" "$work/run.log" | sort | uniq -c | sed 's/^/   /' | tee -a "$log"
say ""
say "== room the shell's layer surfaces left for windows (the bar reserves 44, the dock 96)"
grep -h "usable area is now" "$work/run.log" | sed 's/^\([^ ]*\) .*usable area is now /   \1 /' | tee -a "$log"
say ""
say "== commits per surface, every five seconds"
grep -h "commits so far" "$work/run.log" | sed 's/^\([^ ]*\) .*commits so far: /   \1 /' | tee -a "$log"
errors=$(grep -c "out of date\|Acquire failed" "$work/run.log" || true)
say ""
say "swapchain errors logged by the apps ('out of date', 'Acquire failed'): $errors"

# Every surface the shell and Studio Settings draw must have committed in
# every interval.
fail=0
for surface in "layer crowbaros-bar" "layer crowbaros-dock" "layer crowbaros-wallpaper" "com.crowbaros.StudioSettings"; do
  counts=$(grep -h "commits so far" "$work/run.log" | sed -n "s/.*$surface: \([0-9]*\).*/\1/p")
  intervals=$(printf '%s\n' "$counts" | grep -c . || true)
  stalled=$(printf '%s\n' "$counts" | awk 'NR>1 && $1<=prev {n++} {prev=$1} END {print n+0}')
  say "$surface: $intervals intervals, $stalled without a new commit"
  { [ "$intervals" -ge 10 ] && [ "$stalled" = 0 ]; } || fail=1
done
[ "$errors" = 0 ] || fail=1

say ""
if [ "$fail" = 0 ]; then say "RESULT: pass"; else say "RESULT: FAIL"; fi
say "log: $log"
exit "$fail"
