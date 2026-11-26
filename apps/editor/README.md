# Editor

Editor opens one tab per path passed to `crowbaros-editor`. With no paths it
opens an empty document. Files and other desktop clients can use `xdg-open`
for plain text, Markdown, and JSON. Editor reads UTF-8 regular files up to
32 MiB. A dot beside a tab name marks unsaved edits.

| Keys | Action |
| --- | --- |
| Ctrl+N, Ctrl+O | Create a tab, or type a path to open |
| Ctrl+S, Ctrl+Shift+S | Save, or type a new path to save as |
| Ctrl+Tab, Ctrl+Shift+Tab | Switch to the next or previous tab |
| Ctrl+W, Ctrl+Q | Close a tab or quit, with confirmation for each unsaved document |
| Ctrl+Z, Ctrl+Shift+Z | Undo or redo |
| Ctrl+X, Ctrl+C, Ctrl+V | Cut, copy, or paste through the desktop clipboard |
| Ctrl+F | Find text, then Enter or Shift+Enter to move between matches |
| Ctrl+H | Type the search text, press Enter, type its replacement, then press Enter to replace the next match |
| Ctrl+G | Go to a line by its one-based number |
| Ctrl+Shift+P | Switch between the buffer and Markdown preview |
| Alt+L, Alt+Z | Toggle line numbers or soft wrap |
| Esc | Cancel a prompt |
| Ctrl+Enter | Confirm discarding edits or replacing an existing Save As destination |

Editor syncs a temporary file in the destination directory before renaming it
over the original and syncing the directory. Open paths resolve to their
target. The buffer and syntax colors follow the active CrowbarOS theme.

## Build and run on Void

From this directory, with the GPUI build dependencies used by Base installed:

```sh
python3 ../shell/packaging/prepare-gpui.py
cargo build --release --locked
cargo test --locked
target/release/crowbaros-editor README.md Cargo.toml
```

From the repository root, `lab/void/desktop_check.py` builds Editor in Base’s
Void build root, boots the image, and exercises file association, keyboard
editing, saving, and closing on Fulcrum. The run directory contains the Dark
and Light captures, `desktop.log`, and `results.json`.
