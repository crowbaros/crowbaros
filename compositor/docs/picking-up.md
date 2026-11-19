# Picking up the work

Where the buildout paused, how to run everything, what trips people up, and how to start each next piece. [The plan](PLAN.md) has where each milestone stands. This page is how to get moving again. Keep it current when the work pauses.

## Where it paused

Fulcrum came back to CrowbarOS's `compositor/` on November 18, 2026 ([ADR 0031](../../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)). It has drag and drop inside one compartment, which was not yet in a guest check when it was imported. Its tests cover the pointer grab, action negotiation and data transfer, lock and unmap cancellation, compartment isolation, and the icon's placement below trusted frames.

- **Built:**
  - Milestones 1, 2, and 5;
  - Milestone 3, though not yet run on real GPUs;
  - the parts of Milestones 4, 6, 7, and 8 that CrowbarOS's records decide.
- **Built as proposals:** each passes CrowbarOS's checks and waits for a decision record ([the list](PLAN.md#decisions-this-needs-in-crowbaros)).
- **Waiting on CrowbarOS:**
  - a record of what replaces the broker;
  - records on capture by the shell and on input devices' settings;
  - records on the proposals.
- **Waiting on hardware:** the RTX 4090, Intel, and Surface runs ([how](hardware-runs.md)).

## Next, without a new record

### A compositor restart, in CrowbarOS's desktop check

Milestone 8's exit check restarts the compositor during a session. The session must come back locked before an app gets input. Base starts Fulcrum with `--start-locked`, so this should already hold. What is missing is the check.

- In CrowbarOS's `lab/void/desktop_check.py`, after the shell restart check:
  1. Run `sv restart crowbar-compositor`.
  2. Wait for the shell to come back. It loses its Wayland connection, exits, and runit starts it again.
  3. Check that the session is locked and the lock screen shows.
  4. Unlock with `self.unlock()`.
- Apps lose their connection too, and close, so relaunch any the later checks need.

## Running everything

- **Fulcrum's own checks:** the commands under the README's [Build and test](../README.md#build-and-test). CI runs the same. The GL tests draw with llvmpipe in CI. Run them locally with `LIBGL_ALWAYS_SOFTWARE=1` as well as on your GPU.
- **CrowbarOS's guest checks,** from the repository's root, one at a time, since they share the guest's ports and work directory:
  - `lab/void/compositor_guest.py check` builds `compositor/`, runs its tests in the Void guest, and checks what the compositor serves. It takes about 3 minutes.
  - `lab/void/compositor_guest.py desktop-check` builds Base's image and runs the whole Alpha desktop on a copy of it, booted under UEFI. It takes 4 to 8 minutes, and rebuilds GPUI the first time.
  - Evidence (logs, screenshots, `results.json`) lands under `.lab/`. Attach the log to the pull request.
- **The Alpha desktop's lock:** it starts locked. Press Ctrl+Alt+Enter, then type the lab account's password, `crowbar` (the account is `crowbar-person`), and press Enter.

## Getting a change into Base

1. A change reaches Base when its pull request merges. Run the guest check, and the desktop check if the desktop changes, and attach the evidence.
2. When the shell, the image, or the lab needs its own half of the change, it goes in the same pull request.
3. Offering a global to a client takes a row in `src/policy.rs`, a test of who is offered it, and the matching list in `lab/void/compositor_check.py`, which reads the `NOBODY` row.

## Things that trip people up

- **The proposals span the compositor, the shell, and the lab:**
  - in `compositor/`, the shell's messages are in `src/control.rs`, lock and the unlock authority in `src/lock.rs`, and the layout of displays in `src/layout.rs` and `src/output.rs`;
  - the shell's half is in `apps/shell/src/session.rs` and `main.rs` (`LockScreen`);
  - Base's half is in `base/desktop/`: `unlock.py`, `unlock-run`, `zzz-lock`, `compositor-run`, and `shell-run`, with the account set up in `base/host/image.sh`.
- **The shell is not rustfmt-clean.** `cargo fmt` in `apps/shell` reformats files a change does not touch, so do not run it there. Its CI runs `cargo test` only.
- **Virtual GPUs hide the cursor plane** unless the compositor sets `DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT` and fills in the hotspot. Fulcrum does both; keep them.
- **The unlock helper reads `/etc/shadow` through libcrypt by `ctypes`.** Python 3.13 removed the `crypt` module, and the Void guest runs 3.14.
- **Squash merges** leave local branches that `git branch --merged` does not count as merged. Delete them by name once their pull request is merged.
