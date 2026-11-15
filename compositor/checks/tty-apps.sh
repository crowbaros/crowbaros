#!/usr/bin/env bash
# Runs the GPUI shell and Studio Settings under the compositor on a TTY,
# through DRM, and watches that they keep drawing. It is nested-apps.sh for a
# machine with no desktop session: the compositor takes VT 2 through seatd.
#
# With --suspend it also suspends the machine through zzz partway through, has
# the RTC wake it, and watches whether every surface draws again.
#
#   sudo -v && checks/tty-apps.sh [--suspend] SHELL_BINARY STUDIO_BINARY
#
# Run it over SSH as the user who owns the seat, with passwordless sudo: it
# needs root for openvt, chvt, the runtime directory, and zzz. The compositor
# itself runs as that user with the _seatd group, as lab/void/compositor_check.py
# does.
#
# COMPOSITOR_ARGS adds to the compositor's command line, for example
# COMPOSITOR_ARGS="--mode HDMI-A-1=6144x2560". WATCH_SECONDS sets how long a
# run without --suspend watches (default 68). There is no host window to
# resize on a TTY, so this check resizes nothing; a person at the screen can.
set -euo pipefail

suspend=0
if [ "${1:-}" = --suspend ]; then suspend=1; shift; fi
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/.." && pwd)
shell=$(realpath "${1:?usage: checks/tty-apps.sh [--suspend] SHELL_BINARY STUDIO_BINARY}")
studio=$(realpath "${2:?usage: checks/tty-apps.sh [--suspend] SHELL_BINARY STUDIO_BINARY}")
bin=${CROWBAR_BIN:-$here/target/debug}
out=$root/.lab/compositor
mkdir -p "$out"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
name=tty-apps; [ "$suspend" = 0 ] || name=tty-suspend
log=$out/$name-$stamp.log
run=$out/$name-$stamp.run.log
work=$(mktemp -d)
user=$(id -un)
uid=$(id -u)
runtime=/run/user/$uid
main=
# A second run would take the VT from the first, and the first's exit would
# switch it away again: the other compositor keeps running, paused, unseen.
if pgrep -f "^[^ ]*/crowbar-compositor --backend tty" >/dev/null; then
  echo "another crowbar-compositor is running on a TTY; stop it first" >&2
  exit 1
fi
stop() {
  [ -z "$main" ] || sudo kill "$main" 2>/dev/null || true
  sleep 1
  sudo chvt 1 2>/dev/null || true
  rm -rf "$work"
}
trap stop EXIT

say() { printf '%s\n' "$*" | tee -a "$log"; }

# Nothing on Void creates the runtime directory: elogind would, and Base has none.
sudo install -d -m 700 -o "$user" -g "$user" "$runtime"

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
cat >"$work/compositor.sh" <<SH
#!/bin/sh
exec chpst -u $user:$user:_seatd env HOME=$HOME XDG_RUNTIME_DIR=$runtime LIBSEAT_BACKEND=seatd \
  RUST_LOG=info "$bin/crowbar-compositor" --backend tty ${COMPOSITOR_ARGS:-} --shell "$work/shell.sh" --spawn "$work/studio.sh" \
  >"$run" 2>&1
SH
chmod +x "$work/shell.sh" "$work/studio.sh" "$work/compositor.sh"

say "# The GPUI apps under the compositor on a TTY, $(date -u +%FT%TZ)"
say "host: $(uname -sr), $(. /etc/os-release && echo "$NAME")"
say "kernel command line: $(cat /proc/cmdline)"
say "gpu: $(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null || echo 'no nvidia-smi')"
say "nvidia PreserveVideoMemoryAllocations: $(awk '/PreserveVideoMemoryAllocations/{print $2}' /proc/driver/nvidia/params 2>/dev/null)"
say "drm devices: $(ls /dev/dri | tr '\n' ' ')"
say "connected: $(for c in /sys/class/drm/card*-*; do [ "$(cat "$c/status")" = connected ] && echo "${c##*/}"; done | tr '\n' ' ')"
say "compositor: sha256 $(sha256sum "$bin/crowbar-compositor" | cut -d' ' -f1)"
say "shell: sha256 $(sha256sum "$shell" | cut -d' ' -f1)"
say "studio: sha256 $(sha256sum "$studio" | cut -d' ' -f1)"

sudo openvt -c 2 -s -f -- "$work/compositor.sh"
sleep 2
main=$(pgrep -u "$user" -f "^$bin/crowbar-compositor --backend tty" | head -1 || true)
[ -n "$main" ] || { say "the compositor did not start"; cat "$run" | tee -a "$log"; exit 1; }
say "compositor pid $main on VT $(cat /sys/class/tty/tty0/active)"

if [ "$suspend" = 1 ]; then
  sleep 28
  say ""
  say "suspending through zzz at $(date -u +%T), RTC wake in 25 seconds"
  sudo rtcwake -m no -s 25 >/dev/null
  sudo zzz
  # zzz returns before the machine sleeps; wait until the clock jumps.
  sleep 40
  say "resumed by $(date -u +%T); last sleep: $(sudo dmesg | grep -E 'PM: suspend (entry|exit)' | tail -2 | tr '\n' ' ')"
  resumed=$(date -u +%FT%T)
  sleep 30
else
  sleep "${WATCH_SECONDS:-68}"
fi
sudo kill "$main" 2>/dev/null || true
sleep 1
main=

say ""
say "== DRM device and outputs"
grep -h "tty: DRM device opened\|tty: session\|output\b.*mode\|connector" "$run" | sed 's/^/   /' | tee -a "$log" || true
say ""
say "== whether a flip waits for its drawing (Smithay's needs_sync), per output"
grep -h "needs_sync" "$run" | grep -o "output=[^ ]* needs_sync=[a-z]*" | sort | uniq -c | sed 's/^/   /' | tee -a "$log" || true
say ""
say "== GPU the apps chose"
grep -h "Adapter:" "$run" | sort | uniq -c | sed 's/^/   /' | tee -a "$log" || true
say ""
say "== commits per surface, every five seconds"
grep -h "commits so far" "$run" | sed 's/^\([^ ]*\) .*commits so far: /   \1 /' | tee -a "$log" || true
errors=$(grep -c "out of date\|Acquire failed" "$run" || true)
drm_errors=$(grep -c "tty: DRM error\|did not reactivate\|did not resume" "$run" || true)
# A render fails while the session is paused (another VT, or going to sleep).
# That is expected between a pause and a resume. Any other render failure, or
# a pause with no resume after it, means the output was not being drawn, even
# though the clients went on committing.
render_errors=$(grep "tty: render failed" "$run" | grep -vc "Device is currently paused" || true)
paused_renders=$(grep -c "tty: render failed.*Device is currently paused" "$run" || true)
pauses=$(grep -c "tty: session paused" "$run" || true)
last_session=$(grep -ho "tty: session \(paused\|resumed\)" "$run" | tail -1 || true)
say ""
say "swapchain errors logged by the apps ('out of date', 'Acquire failed'): $errors"
say "DRM errors logged by the compositor: $drm_errors"
say "renders that failed: $render_errors, besides $paused_renders while the session was paused"
say "session pauses: $pauses; last: ${last_session:-none}"

# Every surface must have committed in every interval, apart from the one
# that spans the suspend.
fail=0
for surface in "layer crowbaros-bar" "layer crowbaros-dock" "layer crowbaros-wallpaper" "com.crowbaros.StudioSettings"; do
  lines=$(grep -h "commits so far" "$run" || true)
  counts=$(printf '%s\n' "$lines" | sed -n "s/.*$surface: \([0-9]*\).*/\1/p")
  intervals=$(printf '%s\n' "$counts" | grep -c . || true)
  stalled=$(printf '%s\n' "$counts" | awk 'NR>1 && $1<=prev {n++} {prev=$1} END {print n+0}')
  say "$surface: $intervals intervals, $stalled without a new commit"
  { [ "$intervals" -ge 10 ] && [ "$stalled" = 0 ]; } || fail=1
  if [ "$suspend" = 1 ]; then
    after=$(printf '%s\n' "$lines" | awk -v t="$resumed" '$1 > t' | sed -n "s/.*$surface: \([0-9]*\).*/\1/p")
    gained=$(printf '%s\n' "$after" | awk 'NR==1 {first=$1} {last=$1} END {print last-first+0}')
    say "   after resume: $gained new commits"
    [ "$gained" -gt 0 ] || fail=1
  fi
done
[ "$errors" = 0 ] || fail=1
[ "$drm_errors" = 0 ] || fail=1
[ "$render_errors" = 0 ] || fail=1
[ "$last_session" != "tty: session paused" ] || fail=1
[ "$suspend" = 1 ] || [ "$pauses" = 0 ] || fail=1

say ""
if [ "$fail" = 0 ]; then say "RESULT: pass"; else say "RESULT: FAIL"; fi
say "log: $log"
say "compositor and app log: $run"
exit "$fail"
