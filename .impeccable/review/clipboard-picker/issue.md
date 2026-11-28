The shell opens clipboard history with Super+V and a character picker with Super+.

## Parent

#354

## What to build

Add two fast, keyboard-first overlays to `apps/shell` (and keybindings in Fulcrum) matching the command-palette launcher's design:

- **Clipboard History (`Super+V`)**: Maintains an in-memory bounded history (e.g. last 50 text items, max item size cap, never written to disk by default, cleared on lock/logout or via a **Clear** button). Arrow keys + `Enter` puts the chosen entry back onto the active Wayland clipboard.
- **Character & Emoji Picker (`Super+.`)**: Searchable picker for common emoji, typographic symbols (`—`, `·`, `×`, `→`, `⌘`), and math/currency characters; `Enter` copies the selected character to the Wayland clipboard and closes cleanly.

## Acceptance criteria

- [ ] `Super+V` toggles the clipboard history overlay and `Super+.` toggles the character/emoji picker; neither opens while the desktop is locked, and `Escape` closes both cleanly returning keyboard focus.
- [ ] Clipboard history stays strictly in memory (never written to disk), enforces entry count and byte-size bounds, deduplicates consecutive copies, and offers a one-click **Clear** action.
- [ ] Character picker searches by name and keyword with instant keyboard navigation and copies the selected glyph to the clipboard.
- [ ] Unit tests cover history deduplication, size/count capping, clear-on-lock behavior, and emoji/symbol search ranking.
- [ ] `lab/void/desktop_check.py` verifies `Super+V` shows copied items and restores a previous entry, and `Super+.` opens, filters, and closes cleanly.
- [ ] Before and after captures (Dark and Light) are in the pull request; do not run `cargo fmt` in `apps/shell`.
- [ ] Fill in the pull request template and run the checks `AGENTS.md`'s table asks for this change.

