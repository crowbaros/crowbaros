# Monitor validation — November 26, 2026

Monitor ([issue #364](https://github.com/crowbaros/crowbaros/issues/364)) built
and passed the full Void desktop check: **264 passed, 0 failed**. The app ran
as the ordinary person on Fulcrum in Dark and Light.

The checked implementation was `258f582974dc199bc84a42a5188f09973f915b0a`,
based on `main` at `e1cdaafea15b89b1aeb8977649e4c3d5b3be8d9e`.
`origin/main` was unchanged when validation finished. The subsequent changes
only correct Base's desktop inventory and add this record.

## Checks

| Check | Result |
| --- | --- |
| `cargo fmt --manifest-path apps/monitor/Cargo.toml --check` | Passed |
| `cargo clippy --manifest-path apps/monitor/Cargo.toml --locked --all-targets -- -D warnings` | Passed |
| Monitor's locked Cargo tests in Base's Void build root | 18 library tests and 2 interface feedback tests passed |
| `cargo test --manifest-path apps/monitor/Cargo.toml --locked --no-default-features` | 18 library tests passed locally |
| Python unit tests under `base/` | 104 passed |
| Python unit tests under `lab/void/` | 50 passed |
| `.github/workflows/test_ci.py` | 21 passed |
| `python3 lab/void/desktop_check.py` (full build) | 264 passed, 0 failed; exit status 0 |

The native check built the image, checked the installed binaries against the
build, booted a fresh guest, exercised the desktop, and stopped the guest.
It was not a `--no-build` run. Earlier interrupted runs are not counted as
completed validation.

Monitor's tests cover CPU deltas, missing and reset counters, memory and traffic
parsing, process identity and ownership, exact PID search, sorting, bounded
history, rate formatting, stale service reports, and command output/time limits.
Process tests signal disposable owned children and reject stale identities,
root processes, and Monitor itself. Regression tests keep completed signal
feedback distinct from a pending confirmation and preserve metric warnings.

## Native behavior and visual review

In both Dark and Light, the check verified live CPU/memory samples and process
listing, searched for an owned child by PID, ended it with SIGTERM, cancelled
a Force Quit without killing its target, then confirmed SIGKILL. It selected
PID 1, verified that process actions were refused, and verified PID 1 remained
alive.

Services & Storage displayed Btrfs root space, runit status, and recent socklog
lines as the ordinary person. The root-owned service report was readable but
not writable, and runit's control endpoints remained inaccessible. A log
fixture established that the person could read the system log through Void's
existing `socklog` group.

The Overview, process table, Force Quit confirmation, and Services & Storage
captures were visually reviewed in both themes. Each motion recording contains
18 frames at 1280×960 over six seconds; decoded frames differ and show live
chart updates. An independent interface review identified ambiguous lingering
signal feedback. The checked implementation fixes it, and the follow-up review
accepted that fix without finding a regression in the reviewed captures.

## Evidence

Local evidence, outside git, is under:

```text
.lab/void-desktop-checks/20261126T170933-cj25i3gd/
```

- `build.log`: Void build and component tests.
- `desktop.log`: full desktop check and diagnostics.
- `results.json`: 264 passing checks, no failures.
- `monitor-dark-before.png` and `monitor-light-before.png`: desktop before launch.
- `monitor-dark-after.png` and `monitor-light-after.png`: Monitor Overview.
- `monitor-{dark,light}-processes.png`: process table.
- `monitor-{dark,light}-force-confirmation.png`: pending Force Quit.
- `monitor-{dark,light}-system-process.png`: protected system process selection.
- `monitor-{dark,light}-services.png`: read-only service and storage view.
- `monitor-{dark,light}-motion.mp4`: live chart recordings.

The local test logs are `.lab/monitor-core-tests.log`,
`.lab/monitor-clippy.log`, `.lab/monitor-base-tests.log`,
`.lab/monitor-lab-tests.log`, and `.lab/monitor-ci-tests.log`.

## Limits

This run used QEMU's virtual GPU and software rendering. Physical AMD and
NVIDIA utilization/VRAM telemetry was not qualified; the guest's unavailable
GPU metrics remain explicitly unavailable. Parser coverage does not substitute
for testing those drivers on hardware. The five-second inactive sampling
cadence is implemented, but this record makes no physical-machine power or
performance claim.

Model: gpt-6-astra. Harness: Codex.
