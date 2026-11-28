# Clipboard picker documentation handoff

Implementation inspected: `58b5a23`, issue #367. This is an ordinary extension
of the existing native GPUI launcher. The implemented surface matches the
[direction contract](../../surfaces/apps-shell-src-clipboard-picker-rs.md);
no correction to that contract is needed. `DESIGN.md` and
`.impeccable/design.json` are preserved. Their Studio Settings measurements
remain scoped to that surface; this handoff records the picker's local
implementation without creating new global tokens or rules.

## Preserved system and implemented surface

- **Palette:** the picker resolves the shell's existing canvas, surface, text,
  muted, line, and focus roles. Dark retains Void/Abyss grounds, Frost text,
  and Ice selection; Light uses its pale grounds and darker focus color.
  Selected rows combine surface fill and a focus-colored border. Status
  messages use ordinary readable text rather than introducing an error color.
- **Type:** Manrope remains the interface family. The picker uses an 18px
  semibold heading, the existing 15px native search input, 13px result labels,
  12px request status, 11px details/footer, and the shared 10.5px semibold
  keycaps. Character glyphs occupy a 24px text size in a 34px column; Emoji
  cells explicitly request Noto Color Emoji. This local glyph treatment does
  not replace the Human Type Rule or turn emoji into functional UI icons.
- **Form:** the launcher supplies the centered 520px panel, 56px top offset,
  24px horizontal margins, 12px panel radius, one-pixel border, and dimmed
  backdrop. Result rows are 44px high with 6px corners; the search band is
  46px high. Six rows fit at ordinary scale. The renderer reduces the page
  size as viewport height falls and reserves additional space for status.
  These are overlay measurements, not a new desktop layout standard.
- **Interaction:** search, arrows, Enter, Escape, pointer selection, and
  immediate state changes follow the launcher. Tab reaches Clear and returns
  to search; arrow navigation returns focus to search. Enter's footer label
  follows Clear focus. Choosing a row copies its original contents and closes
  the overlay; it does not synthesize a paste. Preview whitespace is flattened
  and bounded, with ellipsis, while copying preserves the original text.
- **State and depth:** quiet dividers and tonal separation remain unchanged;
  no entrance animation or app-authored shadow is added. History keeps its
  visible memory/lock notice. Read and Clear status remain above populated
  results; pending and failed requests block copying. Clear retains rows until
  confirmed, dims its control while pending, and offers retry on failure.
  Failed reads offer close/reopen. These are operation-specific refinements
  consistent with truthful state and reachable actions, not new global rules.

Source sampled: `apps/shell/src/clipboard_picker.rs`, `picker_history.rs`,
`input.rs`, `theme.rs`, the launcher and shared keycaps in `main.rs`, and
`apps/shell/patches/gpui-0.2.2-emoji-families.patch`. The patch preserves known
emoji fonts that lack a Latin `m`, including subsequent ordinary text fallback.
ADR 0039 records bounded, volatile history and its lock/connection lifetime.
No raster asset is added; wallpaper visible behind the overlay is unchanged.

## Evidence and limits

[packet.md](packet.md), [fixes.md](fixes.md), and [verdict.md](verdict.md)
record the implementation and review sequence. The verdict resolves the two
scored findings: glyph rendering and request states. Its `ship` disposition
is explicitly limited to those fixes. The full-surface final review and full
desktop check were still pending at this documentation handoff; neither is
claimed as passed here. The coordinating agent reports the full compositor
check at 47 passed / 0 failed; this documenter did not run that check.

The native captures are in
`.lab/clipboard-iterations/20261127T225523-ea5pimjj/`. This documentation pass
directly inspected `characters-light-thinking-compact.png` (colored Thinking
face at 200%, with search and footer visible) and the replacement
`clipboard-clear-pending.png` (visible “Clearing history…”, retained rows,
and dimmed Clear). The fix packet and reviewer cover the corresponding Dark,
Light, ordinary-scale, compact, refresh-error, Clear-error, and confirmed
states. Their evidence is native Wayland output from the disposable Void
guest, not a component preview. Warm Paper, Studio, physical hardware, and
assistive-technology behavior are not newly validated by this handoff.

The original [captures.md](captures.md) describes `32c1124` and the earlier
glyph failure. It remains historical; the `58b5a23` fix packet and later
verdict qualify the reused capture paths. Focused iteration logs and files
named `results-not-evidence*.json` are not full graphical-check evidence.
The pending recapture's method and original-service restoration are recorded
in the fix packet and `.lab/clipboard-fault-sequence.log`.

## Drift left outside this extension

`PRODUCT.md` and the root design narrative still carry substrate-vision
claims about Tools and trusted surfaces, with links to the archived vision.
The current `docs/VISION.md` and `GLOSSARY.md` remain authoritative. The
sidecar also retains an old reading-page path and motion wording that differ
from the current root document. These pre-existing records were inspected
and left unchanged within the requested write boundary. No rendering defect,
historical product claim, or request-state implementation detail is canonized
as a new design-system requirement.
