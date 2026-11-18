# Picking up the work

Where the buildout paused, how to run everything, what trips people up, and how to start each next piece. [The plan](PLAN.md) has where each milestone stands. This page is how to get moving again. Keep it current when the work pauses.

## Where it paused

On November 18, 2026, Fulcrum's `main` held everything built so far, and CrowbarOS pinned Fulcrum `c99eaca`. No pull request was open in either repository.

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

Two pieces can be built before any record. Each is one pull request.

### Drag and drop inside one compartment

ADR 0020 offers the data device under the clipboard's rules: inside one compartment it works as apps expect, and between compartments nothing crosses before slice 4. Copy and paste (#38) and the primary selection (#43) already work that way. Today `StartDrag` in `src/data_device.rs` cancels the source at once, and `tests/data_device.rs` checks that (`a_drag_is_refused_as_it_starts`). That test changes with this work.

How it fits:

- **Starting.** A drag starts only while a button is held with the pointer on one of the client's surfaces. That stands for the implicit grab the protocol asks for. Otherwise the source is cancelled, as now.
- **The pointer.** A drag holds the pointer the way a move or resize does. The surface under the pointer gets `wl_pointer.leave`, and no client hears pointer events until the button is let go.
  - Move and resize live in `src/grabs.rs`, as `State::grab`, and `src/seat.rs` hands motion and buttons to it (`pointer_moved_at`, `pointer_button`).
  - The drag can be a variant of `Grab` or a field of its own. `Grab` is matched in a few places, so check them all.
- **Targets.** Only surfaces of the drag's group are targets: the same compartment, or the shell's clients for a shell drag (`group_of`). Over anything else there is no target, and nothing is offered.
- **Events.**
  - On `wl_data_device`: `enter` with a new offer, then `motion`, `leave`, and `drop`.
  - At version 3, the offer carries `source_actions`.
  - `wl_data_offer.accept` sends the source `target`.
  - `set_actions` chooses an action from what both sides allow, the destination's preferred first. The offer and the source each hear `action`.
- **The drop.**
  - Letting go over a target that accepted a type, and an action other than none, sends `drop`. The source hears `dnd_drop_performed`, and later `dnd_finished` when the offer calls `finish`.
  - Otherwise the target hears `leave`, and the source `cancelled`.
  - `finish` before a drop, or with no action, is `invalid_finish`.
- **Ends early.** Locking the session ends a drag, as `lock()` ends a grab. So does the source being destroyed, or its client or the origin going away.
- **The icon.** It is a client's surface with a role of its own (`Role` in `src/surface.rs`), drawn at the pointer plus its offset.
  - It is drawn in `cursor_elements` in `src/cursor.rs`. Like a client's cursor surface, it is clipped so it covers no frame (ADR 0020).
  - While there is an icon, `pointer_image` returns `Composited`, so the TTY backend draws it with the frame instead of the cursor plane.
  - It gets frame callbacks where the cursor surface does, in `src/redraw.rs`.
  - `queue_redraw_for` treats it as the cursor.
  - `destroy_surface` lets go of it.
- **Tests,** in `tests/data_device.rs`:
  - a drag and drop between two clients of one compartment, with the bytes received;
  - none offered to another compartment's window under the pointer;
  - a drop with nothing accepted, cancelled;
  - a drag ended by lock;
  - the action negotiation;
  - the protocol errors;
  - the icon drawn and not covering a frame.

  `tests/common/mod.rs` has `pointer` and `click`. A drag needs a press and a release apart: `pointer_button` called through `Compositor::with`.

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
- **CrowbarOS's guest checks,** from a CrowbarOS checkout, one at a time, since they share the guest's ports and work directory:
  - `lab/void/compositor_guest.py check` builds Fulcrum at the pin, runs its tests in the Void guest, and checks what the compositor serves. It takes about 3 minutes.
  - `lab/void/compositor_guest.py desktop-check --build-cache .lab/void-graphical` runs the whole Alpha desktop in a fresh guest. It takes 4 to 8 minutes, and rebuilds GPUI the first time.
  - For a Fulcrum commit that is not pinned yet, set `FULCRUM_REPOSITORY` to a Fulcrum clone and `FULCRUM_REVISION` to the full commit ID. Get the ID from `git rev-parse`; never type one.
  - Evidence (logs, screenshots, `results.json`) lands under `.lab/`. Attach the log to the pull request.
- **The Alpha desktop's lock:** it starts locked. Press Ctrl+Alt+Enter, then type the lab account's password, `crowbar` (the account is `crowbar-person`), and press Enter.

## Releasing a change to CrowbarOS

1. Merge the Fulcrum pull request, with the guest check's evidence for the commit.
2. Open a CrowbarOS pull request that changes `compositor/fulcrum.json` to the merged commit. Run the guest check, and the desktop check if the desktop changes.
   - When CrowbarOS needs its own half of the change (the shell, the image, the lab), it has gone in the same pull request as the pin. crowbaros#206 and #207 did that. Without the new pin, that half does not work.
3. Offering a global to a client takes a row in `src/policy.rs`, a test of who is offered it, and the matching list in CrowbarOS's guest check, which reads the `NOBODY` row.

## Things that trip people up

- **The proposals span both repositories:**
  - in Fulcrum, the shell's messages are in `src/control.rs`, lock and the unlock authority in `src/lock.rs`, and the layout of displays in `src/layout.rs` and `src/output.rs`;
  - in CrowbarOS, the shell's half is in `apps/shell/src/session.rs` and `main.rs` (`LockScreen`);
  - the lab's half is in `lab/void/guest/`: `alpha-unlock.py`, `unlock-run`, `zzz-lock`, `compositor-run`, and `shell-run`, with the account set up in `lab/void/host/build-graphical-image.sh`.
- **CrowbarOS's shell is not rustfmt-clean.** `cargo fmt` in `apps/shell` reformats files a change does not touch, so do not run it there. Its CI runs `cargo test` only.
- **Virtual GPUs hide the cursor plane** unless the compositor sets `DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT` and fills in the hotspot. Fulcrum does both; keep them.
- **The unlock helper reads `/etc/shadow` through libcrypt by `ctypes`.** Python 3.13 removed the `crypt` module, and the Void guest runs 3.14.
- **Squash merges** leave local branches that `git branch --merged` does not count as merged. Delete them by name once their pull request is merged.
