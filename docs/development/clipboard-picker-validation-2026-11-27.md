# Clipboard and character picker validation — November 27, 2026

Issue [#367](https://github.com/crowbaros/crowbaros/issues/367) is assigned to
`jasona`. The implementation is on `feat/367-clipboard-picker`, based on main
`e9505b41321540fc1f6fe7f3c54cc5bab0fda1cf`. Runtime source is committed as
`58b5a23530846745ef060459931dc63edaf51226`; later commits record review and
evidence only. Main was unchanged when fetched during validation.

The required full compositor check passed **47 checks, 0 failed**, exit 0.
Native app builds, focused picker checks, and visual review passed. The full
desktop check did not finish: the session was interrupted while it ran, and
the updated project process makes that check opt-in. It was not restarted.

## Behavior

Super+V opens searchable clipboard history; Super+. opens a searchable
catalogue of 76 characters and emoji. Both use the shell launcher's visual
language and return focus to the prior app on Escape or selection. Enter
copies the selection and closes the overlay. It does not paste into the app.

The compositor keeps text history only in memory, after the shell opts in:
50 entries, 8 KiB per entry, 64 KiB total. Consecutive duplicates are ignored.
Clear, lock, and the owning shell connection's disconnect clear the history
and cancel any pending capture. Binary, invalid UTF-8, empty, and oversized
selections are skipped. A stalled capture expires after two seconds. The
Agent has no history-reading operation. These decisions are recorded in
[ADR 0039](../adr/0039-the-shell-keeps-a-bounded-clipboard-history.md).

Read and Clear requests show their pending or failed status above existing
rows. A failed request retains those rows, blocks copying stale data, and
explains recovery. Clear shows an empty history only after confirmation.

## Checks

| Check | Result |
| --- | --- |
| Compositor locked tests, default and nested | Passed |
| Compositor fmt and clippy, default and nested, all targets, warnings denied | Passed |
| Shell locked tests in Void | 141 unit tests and 13 integration tests passed |
| Full Void release builds and tests | Passed, including the shared GPUI patch across desktop apps |
| Base Python tests | 106 passed |
| Lab Python tests | 57 passed |
| CI routing tests | 24 passed |
| Shell packaging tests | 7 passed |
| `python3 lab/void/compositor_check.py` | Full build; 47 passed, 0 failed; exit 0 |
| Focused native picker sequences, Dark and Light | Passed; iteration evidence, not a full desktop pass |
| `python3 lab/void/desktop_check.py` | Incomplete after interruption; not restarted under the updated opt-in process |

No `cargo fmt` was run in `apps/shell`.

The compositor tests exercise count and byte limits, deduplication, real
selection capture, lock and disconnect cleanup, stalled writers, invalid
text, unlocked-only gestures, and refusal of history operations on the
Agent channel. Shell tests cover catalogue ranking, event parsing, retained
rows after failed requests, Clear confirmation and retry, and refresh
coalescing.

The native desktop sequence exercises real Wayland transfers in Dark and
Light: copying two values, restoring an earlier value, searching history,
character search, paging and selection, emoji copying, chord toggles,
Escape and focus restoration, Clear, and lock/unlock behavior.

## Glyph regression and request-state review

The review exposed a GPUI font-family bug: requesting a known emoji font
without a Latin `m` removed the face from its font database. Explicit emoji
cells and subsequent ordinary history fallback could then show missing
glyphs. The reproducible source-preparation patch preserves known emoji
families while retaining the existing guard for other broken fonts.

Before the fix, the native Thinking-face assertion counted **0** colored
glyph pixels; after the fix it counted **456 in Dark and 481 in Light**.
That assertion is now part of the desktop check. Fontconfig also found
coverage for all 76 catalogue entries; coverage alone is not a rendered
visual test. The [before-fix capture](assets/clipboard-picker-2026-11-27/emoji-render-without-fix.png)
preserves the failure.

A disposable guest's connector was temporarily replaced by the
[review fixture](../../.impeccable/review/clipboard-picker/fault-proxy.py).
It forwarded real Wayland drawing and injected ordinary control errors or
a 0.4-second Clear delay. The shell contains no fault-injection path. The
guest's original shell service was restored and restarted afterward. The
[fixture log](assets/clipboard-picker-2026-11-27/fault-captures.log) records
the run. All clipboard text in the captures is synthetic test content.

An initial pending-state capture missed the pending frame because the QMP
key helper waited 0.3 seconds. Direct QMP press/release and a sequence of
raw captures corrected the evidence. The delivered pending image is the
unaltered `clipboard-clear-inflight-0.png` frame, copied to its named path.

The [fix verdict](../../.impeccable/review/clipboard-picker/verdict.md)
resolved both material findings. The subsequent
[full review](../../.impeccable/review/clipboard-picker/full-review-final.md)
returned `ship` with no remaining material fixes. The
[documentation handoff](../../.impeccable/review/clipboard-picker/documentation.md)
compares the finished extension with the incumbent design and preserves the
global design files. Its pending-review wording records the state at handoff;
the full review completed afterward without further implementation changes.

## Captures

All captures are native QEMU output at 1280×960. Compact captures use 200%
scale (640×480 logical); others use 100%. Before captures came from the
pre-feature image at `a2d9733`. Final captures use the shell built from
`58b5a23`. Existing wallpapers are unchanged, and the feature adds no
animation or raster artwork.

| State | Dark | Light |
| --- | --- | --- |
| Before Super+V | [Before](assets/clipboard-picker-2026-11-27/clipboard-dark-before.png) | [Before](assets/clipboard-picker-2026-11-27/clipboard-light-before.png) |
| Clipboard history with long text | [After](assets/clipboard-picker-2026-11-27/clipboard-dark-text.png) | [After](assets/clipboard-picker-2026-11-27/clipboard-light-text.png) |
| Before Super+. | [Before](assets/clipboard-picker-2026-11-27/characters-dark-before.png) | [Before](assets/clipboard-picker-2026-11-27/characters-light-before.png) |
| Character catalogue | [After](assets/clipboard-picker-2026-11-27/characters-dark-all.png) | [After](assets/clipboard-picker-2026-11-27/characters-light-all.png) |
| Thinking-face glyph | [Dark](assets/clipboard-picker-2026-11-27/characters-dark-thinking.png) | [Light](assets/clipboard-picker-2026-11-27/characters-light-thinking.png) |
| Emoji at 200% scale | [Dark](assets/clipboard-picker-2026-11-27/characters-dark-emoji-compact.png) | [Light](assets/clipboard-picker-2026-11-27/characters-light-emoji-compact.png) |
| Copied rocket in history | [Dark](assets/clipboard-picker-2026-11-27/clipboard-dark-rocket.png) | [Light](assets/clipboard-picker-2026-11-27/clipboard-light-rocket.png) |
| Failed populated refresh | [Dark](assets/clipboard-picker-2026-11-27/clipboard-dark-refresh-error.png) | [Light](assets/clipboard-picker-2026-11-27/clipboard-light-refresh-error.png) |
| Failed Clear | [Dark](assets/clipboard-picker-2026-11-27/clipboard-dark-clear-error.png) | [Light](assets/clipboard-picker-2026-11-27/clipboard-light-clear-error.png) |

Additional captures show [pending Clear](assets/clipboard-picker-2026-11-27/clipboard-clear-pending.png),
[confirmed Clear](assets/clipboard-picker-2026-11-27/clipboard-clear-confirmed.png),
[Clear keyboard focus](assets/clipboard-picker-2026-11-27/clipboard-light-clear-focus.png),
[no search results](assets/clipboard-picker-2026-11-27/characters-light-empty.png),
[keyboard paging](assets/clipboard-picker-2026-11-27/characters-light-next-page.png),
and [history at 200% scale](assets/clipboard-picker-2026-11-27/clipboard-light-rocket-compact.png).

## Full-run evidence and limits

Both graphical commands rebuilt Base and booted disposable UEFI guests.
The compositor check completed; the desktop check was interrupted. Neither
used `--no-build`. Source was `58b5a23`; review reports
and the surface contract were still being written, so the checkout was
reported as dirty. Compiled source files matched that commit.

Original local evidence stays unchanged:

```text
.lab/void-compositor-checks/20261127T231124-e0zc8ghk/compositor-20261128T061124Z.log
.lab/void-compositor-checks/20261127T231124-e0zc8ghk/build.log
.lab/void-desktop-checks/20261127T232109-r3hq44_1/desktop.log
.lab/void-desktop-checks/20261127T232109-r3hq44_1/build.log
```

The public [compositor log](assets/clipboard-picker-2026-11-27/compositor.log)
replaces four occurrences of the host checkout path with `<checkout>`.
The [incomplete desktop log](assets/clipboard-picker-2026-11-27/desktop-incomplete.log)
is retained separately and is not a passing result. The interrupted process
did not produce `results.json` or a final exit-status record; no substitute
results file was fabricated. The driver and its guest were already stopped
when inspected after the interruption. The separate iteration guest was
also stopped after visual review.

The [focused native log](assets/clipboard-picker-2026-11-27/focused-native.log)
contains the completed picker assertions in both themes and the subsequent
fault-fixture failure described below. Both public copies replace host
checkout paths with `<checkout>`; their original logs remain unchanged.
Guest account paths and QEMU network addresses describe disposable guests.
Generated Agent test markers are fixtures, not account credentials.

Earlier iterations and the stopped desktop run at `32c1124` are not the
final acceptance evidence. One combined focused run completed the normal
functional and glyph assertions, then failed its fault fixture because
`wl-copy` could not acquire keyboard focus while the overlay was open. The
corrected fixture emits the existing change event and completed successfully.

No physical-hardware, screen-reader, or native IME composition validation is
claimed. The branch remains local; remote PR CI has not run.

Implementation and validation: GPT-6 Astra through the Codex harness in M3 Code.
