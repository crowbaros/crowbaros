# Clipboard history and character picker review

Issue #367. Ordinary extension of the established shell launcher in Operate
mode; no replacement identity, concept seed, generated artwork, or web UI.

Direction contract: `.impeccable/surfaces/apps-shell-src-clipboard-picker-rs.md`.
Visual authority: `DESIGN.md`, `.impeccable/design.json`, and the existing
launcher in `apps/shell/src/main.rs`. Product truth: `docs/VISION.md` and
`PRODUCT.md`; pre-existing substrate wording is historical drift, not part
of this change.

Implementation: `apps/shell/src/clipboard_picker.rs`, `symbols.rs`, `input.rs`,
`session.rs`, and the small integration in `main.rs`. Privacy/lifetime:
`compositor/src/clipboard.rs` and ADR 0039. Native check:
`Check.clipboard_picker_check` in `lab/void/desktop_check.py`.

The initial visual pass found mixed font fallback in emoji rows. Emoji now
explicitly use Noto Color Emoji. Rows adapt to display height, preserving
search and Clear at 200% scale. Arrow navigation returns focus to search;
the Enter hint names Clear while that button has focus. No entrance motion
is added. The second pass checks Dark, Light, empty and filtered states,
Clear focus, paging, and 200% scale after this batch.

Native captures and focused check logs will be listed in `captures.md`.
They are evidence from QEMU on Void, using real Wayland clipboard transfers,
not a mockup. A host build alone is not the validation claim. Full graphical
checks will be recorded separately after they finish.

Please review the complete overlay at the captured sizes and states against
the contract and incumbent design, including keyboard navigation, readable
text, bounded previews, privacy wording, empty/error/loading behavior, and
clipping. Scope is the new picker; unrelated pre-existing app/desktop UI is
context only. Do not repair shared DESIGN.md or its sidecar in this extension.
There are no new raster assets shipped by the implementation. Existing
wallpaper visible behind the picker is unchanged.
