---
version: 1
slug: "apps-shell-src-clipboard-picker-rs"
primary_target: "apps/shell/src/clipboard_picker.rs"
related_targets: ["apps/shell/src/symbols.rs"]
---

# Clipboard history and character picker

Operate mode. Local extension of the shell launcher for #367. People recall
copied text or find a character while writing in another app. Both built-in
Dark and Light themes serve the person's chosen ambient-light setting.

## Direction contract

THESIS: Recover text or find a glyph in one keyboard gesture; inherit the
launcher's search-and-results structure.

OWN-WORLD: Existing Manrope labels, native input, canvas/surface tokens,
focus-colored selection, 12 px panel radius, and plain quiet footer hints.
Emoji glyphs use Noto Color Emoji; history keeps the normal text fallback.

STORY: Open, type to narrow, move through results, press Enter to copy and
return to the prior app. History visibly says it clears on lock; Clear is
available by pointer and keyboard. Empty, loading, and failed reads are explicit.

FIRST VIEWPORT: A centered 520 px panel 56 px below the top, with a heading,
search field, six 44 px result rows, and an action footer. History previews
flatten whitespace only for display; copying preserves original text.
Character rows pair the actual glyph with its name and category. At high
display scales fewer rows keep search and Clear visible. Operation status
stays visible above populated rows; failed updates do not imply a cleared list.

FORM: Existing launcher extension, shaped directly under the new-work local
extension exception; no concept seed or new visual world. Keyboard selection
moves immediately without entrance motion or delayed focus.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
