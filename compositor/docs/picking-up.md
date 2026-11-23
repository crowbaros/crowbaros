# Picking up the work

Where the buildout paused, how to run everything, what trips people up, and how to start each next piece. [The plan](PLAN.md) has where each milestone stands. This page is how to get moving again. Keep it current when the work pauses.

## Where it paused

Fulcrum runs CrowbarOS's desktop on Base's image. It has two kinds of client, the shell and the person's apps ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)), each at a socket of its own that takes one user ID. Copy and paste, the primary selection, and drag and drop work between the shell and every app, and a new window takes the keyboard, as on any desktop. The desktop starts locked, the person's password unlocks it through Base's helper, and a compositor restart comes back locked. CrowbarOS's desktop check shows each of these on a copy of Base's image, booted under UEFI.

- **Built:** Milestones 1 to 6, 8, and 9, and Milestone 7 but for input devices' settings. Milestones 3, 7, and 8 have run on the test machine's RTX 5060 and its integrated Radeon, with one monitor ([the records](../../docs/development/README.md)).
- **Waiting on CrowbarOS's records:** capture by the shell, and input devices' settings. What Fulcrum offers the Agent is [ADR 0035](../../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md), proposed. Of it, the summon gesture is built: Super+Space, and the keyboard's way to the Agent's surface and back.
- **Waiting on hardware:** the run on Jason Alexander's Mesa machine ([crowbaros#228](https://github.com/crowbaros/crowbaros/issues/228)), and on either machine the rows no record has yet: a second monitor, with mixed scale and locked across suspend, and a dense panel's scale.

## Next, without a new record

### The hardware runs

Base's image boots a real machine from a USB disk, and the desktop runs there as it does in the VM. The test machine has its runs and records, each with one monitor. What is missing is a run on the Mesa machine, and the rows that need a second monitor or a dense panel, each with a dated record.

1. Build the image with your own account file, write it to a disk, and boot the machine, as [Base on a real machine](../../base/HARDWARE.md) says.
2. Run each row of [Running Fulcrum on real hardware](hardware-runs.md), and read the compositor's log over SSH.
3. Write the dated record the guide asks for, with the log beside it. A row that fails opens an issue.

## Running everything

- **Fulcrum's own checks:** the commands under the README's [Build and test](../README.md#build-and-test). CI runs the same. The GL tests draw with llvmpipe in CI. Run them locally with `LIBGL_ALWAYS_SOFTWARE=1` as well as on your GPU.
- **CrowbarOS's guest checks,** from the repository's root, one at a time (both share `.lab/base/` with Base's build):
  - `lab/void/compositor_check.py` builds Base's image, boots a copy under UEFI, and checks what the compositor serves.
  - `lab/void/desktop_check.py` builds Base's image and runs Base's boot checks and the whole desktop on a copy of it, booted under UEFI.
  - Evidence (logs, screenshots, `results.json`) lands under `.lab/`. Attach the log to the pull request.
- **The desktop's lock:** it starts locked. Type the person's password, and press Enter. The checks build the lab account, `crowbar`, whose password is `crowbar`.

## Getting a change into Base

1. A change reaches Base when its pull request merges. Run the guest check, and the desktop check if the desktop changes, and attach the evidence.
2. When the shell, the image, or the lab needs its own half of the change, it goes in the same pull request.
3. A new global is offered to every client unless `src/policy.rs` refuses it to one. Refusing one takes an entry in `APPS_REFUSED` or `SHELL_REFUSED`, a test of who is refused it, and the matching list in `lab/void/compositor_check.py`, which reads `APPS_REFUSED`. A global the compositor makes for every client also goes in the lists of `tests/suite/table.rs` and `lab/void/compositor_check.py`.

## Things that trip people up

- **The messages ADR 0032 accepted span the compositor, the shell, and Base:**
  - in `compositor/`, the shell's messages are in `src/control.rs`, lock and the unlock authority in `src/lock.rs`, and the layout of displays in `src/layout.rs` and `src/output.rs`;
  - the shell's half is in `apps/shell/src/session.rs` and `main.rs` (`LockScreen`);
  - Base's half is the unlock helper, `base/helpers/src/unlock.rs`, and in `base/desktop/`: `unlock-run`, `zzz-lock`, `compositor-run`, and `shell-run`, with the account set up in `base/host/image.sh`.
- **The shell is not rustfmt-clean.** `cargo fmt` in `apps/shell` reformats files a change does not touch, so do not run it there. Its CI runs `cargo test` only.
- **Virtual GPUs hide the cursor plane** unless the compositor sets `DRM_CLIENT_CAP_CURSOR_PLANE_HOTSPOT` and fills in the hotspot. Fulcrum does both; keep them.
- **The unlock helper reads `/etc/shadow` through libcrypt** (libxcrypt's `crypt_rn`), linked from Base's helpers, `base/helpers/src/crypt.rs`. Base's image has no Python.
- **Squash merges** leave local branches that `git branch --merged` does not count as merged. Delete them by name once their pull request is merged.
