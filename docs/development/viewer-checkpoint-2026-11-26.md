# Viewer: first implementation checkpoint, November 26, 2026

Issue [#362](https://github.com/crowbaros/crowbaros/issues/362) is assigned to
`jasona`. It was the next unblocked open child authored by Jason in the default
apps plan: #356 still depended on #352, and #362's shared-library dependency
#355 was closed. No open pull request claimed it. Work started from
`e1cdaafea15b89b1aeb8977649e4c3d5b3be8d9e`, whose CI passed.

This is a local checkpoint, not completion of #362. The branch is
`feat/362-viewer`; no branch was pushed and no pull request was opened.

## Implemented

Viewer opens bounded PNG, JPEG, WebP, BMP, TIFF, SVG, and PDF inputs. The native
window supports image zoom, pan, rotation, sibling navigation, metadata and
copying; PDF pages, search and next-match navigation, nearby thumbnails, and
a selectable text panel. It follows the shared theme. Base installs Viewer,
Poppler, and image/PDF MIME defaults; CI builds and tests the app. The desktop
check exercises real PNG/PDF fixtures and records both appearances.

See `apps/viewer/README.md` for format limits and outstanding behavior.

## Verification

At code commit `c9a0422e2c1a64e1de4c50260b91664b96a3aa57`:

- Viewer: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  and `cargo test --locked` passed. The test suite has 29 tests, including real
  Poppler rendering/text extraction and unsafe-input refusal.
- Python suites passed: Base 104, CI selection 21, lab 50.
- The first Void build found a blank rendered PDF in its test because the
  build root lacked fallback fonts. Adding the fonts already present in the
  runtime image fixed that build/test failure.

The full desktop command was started, but was intentionally interrupted after
Viewer's Dark/Light checks while unrelated Agent-surface checks continued.
**It is not a passing full desktop check or final pre-PR evidence.**
Its built Viewer was commit `b188463`, before the final `c9a0422` fixes that
preserve the PDF page during layout changes and tolerate unavailable text.
Those final fixes passed local tests and still need a new native run.

The interrupted run is `.lab/void-desktop-checks/20261126T130455-av82jkev/`:

- `desktop.log` and `results.json` record the checks and interruption.
- `viewer-image-{dark,light}-{before,after}.png` show image viewing.
- `viewer-pdf-{dark,light}-{before,after}.png` show PDF viewing.
- `viewer-pdf-{dark,light}-selection.png` show selectable text. A pixel assertion
  passed for both appearances, after visual inspection found and fixed the
  pinned GPUI component's single-block scrolling-renderer problem.

Earlier unsuccessful/superseded runs remain unchanged in their own directories:
`20261126T125559-77rlytw0` (font-dependent PDF test failure) and
`20261126T125850-1kcjb5zo` (interrupted to rebuild the text-panel fix).

## Next work before completion

- Implement Set as wallpaper through a bounded handoff to the shell. Its
  separate account cannot read arbitrary paths in the person's private home;
  the shared custom-background path is also part of #368.
- Complete lazy thumbnail browsing beyond nearby pages and refine PDF search
  navigation/selection. Text selection currently uses a separate panel.
- Run the complete desktop check against the final code, finish the remaining
  interaction acceptance checks, and capture a short motion recording.
- Fill in the pull request template when Jason asks to publish the branch.

Model: gpt-6-astra. Harness: Codex in M3 Code.
