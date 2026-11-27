# Viewer integration validation — November 26, 2026

The Viewer branch was rebased onto `main` at
`68abb8adf7e6ef7feeb24e92e7b4e115e827fb4d`. The full Void desktop check
passed **305 checks with no failures** on clean implementation commit
`bae0330977df7cf0c2058c265908523391db8ae9`. A fetch after the run confirmed
that `main` had not moved. Subsequent changes only publish this evidence.

## Integration and CI diagnosis

The rebase preserves both Monitor and Viewer in Base's build and installed
desktop entries, the CI interface matrix and boot classification, and the
Dark/Light desktop checks. It also retains the newer Agent subagent and
message-steering work from `main`.

The original hosted failure in PR #417 happened during `apt-get update`,
before Viewer compiled: the runner's preconfigured Microsoft package
repository returned HTTP 403. The other interface jobs were cancelled by
the matrix. No Viewer code change is warranted by that package-metadata
failure; pushing the integrated branch starts a fresh CI run.

An initial integration run passed 303 checks on `main` commit
`6d4f789de1c4e535c7eed55726420854060ff328`. Because #420 landed during that
run and changed paths covered by the desktop check, the branch was rebased
again and the full check repeated. The 305-check run is the final evidence.
Earlier dated records and original local logs remain unchanged.

## Verification

| Check | Result |
| --- | --- |
| `python3 lab/void/desktop_check.py` with full build | 305 passed, 0 failed; exit 0 |
| Viewer `cargo test --locked` | 36 passed locally and in Void |
| Viewer `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| Local interface matrix, `cargo test --locked` | Shell 148, Files 45, Editor 5, Monitor 20, Viewer 36, Studio Settings 42, shared UI 6 passed |
| Theme library `cargo test --locked` | 13 passed |
| Desktop helpers: format, Clippy, locked tests | Passed; 54 tests |
| Lever: format, Clippy, locked tests | Passed; 231 tests |
| Python compilation and tests | Base 105, lab 54, CI classification 22, GPUI preparation 7 passed |

The first local matrix ran after integrating Monitor and subagent support.
After #420 landed, the changed shell, Lever, and lab checks were rerun; the
table reports those final counts. The full Void build and native suite ran
again on the final tree.

Both themes passed Viewer image and PDF inspection, sibling image copying,
continuous PDF scrolling, selectable-text copying, individual search hits,
distant thumbnails, and wallpaper persistence across a shell restart. The
check restores the person's original appearance preferences. The same run
also exercised Monitor and the desktop's Agent, message-steering, subagent,
and recovery scenarios.

The image/PDF captures were visually inspected. Both image-panning videos
are 1280×960, six seconds long, with 18 distinct frames. This is QEMU software
rendering evidence; physical GPU qualification was not run. PDF selection
remains in the dedicated text panel, as documented in Viewer’s README.

## Evidence

[Published evidence](evidence/viewer-rebase-2026-11-26/README.md) includes
`desktop.log`, `results.json`, the Void build log, local component logs,
Dark/Light captures, recordings, and checksums. Host checkout paths in the
publication copies are replaced with `/workspace/crowbaros`, and host home
paths with `/home/build`; no original log is edited.

The original full run is under
`.lab/void-desktop-checks/20261126T215856-u5pc_zmt/`.

Model: gpt-6-astra. Harness: Codex in M3 Code.
