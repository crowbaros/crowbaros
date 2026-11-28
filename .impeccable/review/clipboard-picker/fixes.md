# Fix batch for the finish verdict

Implementation commit: `58b5a23` (built in the Void guest). Review only the
first review's two material findings. No new visual direction or polishing
pass was undertaken.

## 1. Glyph rendering

GPUI rejected and removed any explicitly requested font without a Latin `m`.
The reproducible `gpui-0.2.2-emoji-families.patch` exempts its existing known
emoji-font predicate. Noto Color Emoji remains available for explicit glyph
cells and ordinary history fallback. The source preparation script applies
the patch with zero fuzz; no generated source is committed.

The native Thinking-face pixel assertion reported zero colored glyph pixels
before the fix, then 456 in Dark and 481 in Light after it. The assertion is
now part of `Check.clipboard_picker_check`. Fontconfig also found coverage
for all 76 catalogue entries; that is coverage evidence, not a substitute
for rendered captures.

In `.lab/clipboard-iterations/20261127T225523-ea5pimjj/`, inspect:

- `characters-dark-thinking.png`, `characters-light-thinking.png`
- `characters-dark-emoji.png`, `characters-light-emoji.png`
- `clipboard-dark-rocket.png`, `clipboard-light-rocket.png`
- `characters-dark-thinking-compact.png`, `characters-light-thinking-compact.png`
- `characters-dark-emoji-compact.png`, `characters-light-emoji-compact.png`
- `clipboard-dark-rocket-compact.png`, `clipboard-light-rocket-compact.png`

The compact captures are 200% scale (640×480 logical). Ordinary rocket
history was captured after opening the explicit emoji family, exercising
the former font-database removal and subsequent text fallback.

## 2. Operation states

`picker_history.rs` holds request state independently of the renderer. Read
and Clear have distinct pending/failure messages, rendered above results
regardless of count. A failed update retains cached rows but disables copying
until a successful update. Clear does not empty the display until confirmed.
A failed Clear offers Clear again; a failed read offers close and reopen.
The normal empty-history state appears only after a successful read/clear.

Three unit tests cover populated refresh failure, unsuccessful Clear and
retry, and coalescing changes while a request is in flight. They pass.

The review fixture `fault-proxy.py` runs only in the disposable guest in
place of its connector. It leaves Wayland drawing untouched, returns an
ordinary control error for the selected operation, and can delay a request.
The shell has no injected test path. The original guest service was restored
and restarted afterward. These captures exercise the actual error handling:

- `clipboard-dark-refresh-error.png`, `clipboard-light-refresh-error.png`
- `clipboard-dark-clear-error.png`, `clipboard-light-clear-error.png`
- `clipboard-clear-pending.png`, `clipboard-clear-confirmed.png`

The final fault-capture run is `.lab/clipboard-fault-captures-final.log` and
completed successfully. The earlier combined fix run's final fixture step
failed because a new `wl-copy` could not acquire keyboard focus while the
exclusive overlay was up. Its completed functional/glyph assertions remain
in `.lab/clipboard-review-native.log`; the corrected proxy sends the existing
change event instead, so this capture does not depend on focus stealing.

The original issue is now supplied as `issue.md`. A separate quality-bar
card is inapplicable to this precisely scoped extension of the existing
launcher; its visual authority and contract remain those named in packet.md.

## Pending-state recapture

The exact `clipboard-clear-pending.png` path now contains raw native frame
`clipboard-clear-inflight-0.png` from `.lab/clipboard-fault-sequence.log`.
The proxy delayed Clear by 0.4 seconds. A direct QMP press/release avoided
the convenience key helper’s 0.3-second delay. The frame visibly shows
“Clearing history…”, both retained rows, and disabled Clear. The original
guest shell service was restored after capture.
