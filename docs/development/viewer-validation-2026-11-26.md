# Viewer validation, November 26, 2026

Issue [#362](https://github.com/crowbaros/crowbaros/issues/362) remains assigned
to `jasona`. Its implementation is ready for review locally on
`feat/362-viewer`. This record follows the earlier
[checkpoint](viewer-checkpoint-2026-11-26.md), which remains unchanged.

## Result

`lab/void/desktop_check.py` completed with **282 passed, 0 failed**. It built
and tested Base and the native apps in Void, then exercised the complete
desktop in a software-rendered QEMU guest, including reboot, display changes,
clipboard transfers, and Agent recovery.

The run used clean code commit `597e1a7d3fe61094109b16a510857f874d911c86`,
based on `e1cdaafea15b89b1aeb8977649e4c3d5b3be8d9e`. A fetch after the run
confirmed that `origin/main` had not moved. The branch has not been pushed,
and hosted CI has not run for it.

Evidence is retained in
`.lab/void-desktop-checks/20261126T152957-7hpg9_af/`:

- `desktop.log`, `results.json`, `run.json`, and `build.log` record the full run.
- `viewer-image-{dark,light}-{before,after}.png` show image viewing and metadata.
- `viewer-pdf-{dark,light}-{before,after}.png` show PDF viewing and navigation.
- `viewer-pdf-{dark,light}-selection.png` and
  `viewer-pdf-{dark,light}-thumbnails.png` show text and distant thumbnails.
- `viewer-wallpaper-{dark,light}.png` and
  `viewer-wallpaper-{dark,light}-restarted.png` show wallpaper persistence.
- `viewer-image-{dark,light}-motion.mp4` are six-second, 1280×960 H.264
  recordings of real pointer-driven image panning.

The Dark and Light captures were visually inspected. Both recordings were
checked with `ffprobe` for encoding, dimensions, and duration.

## What the checks established

Viewer opens image and PDF MIME defaults, navigates sibling images, copies
PNG pixels to another Wayland client, pans and rotates images, and displays
metadata. Its bounded custom wallpaper survives closing Viewer and restarting
the shell; restoring the built-in wallpaper removes the override.

PDF checks use twelve pages and twenty-four search occurrences. They verify
continuous scrolling, page jumps, forward and backward navigation between
matches on the same page, keyboard text selection and clipboard delivery,
and rendering and opening distant thumbnails without first visiting those
pages. The run restores the original appearance and wallpaper preferences.

The native checks exposed two pinned GPUI defects: image copies advertised
only text, and scroll events without optional axis-source metadata were
dropped. Maintained patches now transfer the requested image bytes and handle
continuous and discrete scrolling. Regression tests cover their shared
logic, and the full native run verifies both fixes alongside existing text,
primary-selection, and drag-and-drop transfers.

## Other verification

- Viewer: 36 Rust tests, formatting, and
  `cargo clippy --locked --all-targets -- -D warnings` passed. Tests include
  bounded decoding, real Poppler operations, stale thumbnail results,
  extensionless PDFs, and a failed page beside readable pages.
- Python: Base 104, lab 53, CI classification 21, and GPUI preparation 7
  tests passed; the scripts also compiled.
- The Void build passed the native app, helper, compositor, and Lever tests.
  Targeted GPUI regression tests passed all 13 cases.
- Lever's formatting, Clippy, and locked test commands passed locally.

An earlier full run (`20261126T151922-hpyaz74o`) failed at image clipboard
delivery. Two subsequent `--no-build` diagnostic runs remain explicitly
marked **NOT EVIDENCE**. The passing full run above supersedes them.

PDF selection uses the separate **Select text** panel. SVG and raster limits
are documented in [Viewer's README](../../apps/viewer/README.md). Physical-GPU
qualification was outside this VM validation.

Model: gpt-6-astra. Harness: Codex in M3 Code.
