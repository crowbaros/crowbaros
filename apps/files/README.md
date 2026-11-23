# Files

CrowbarOS's default file manager (`crowbaros-files`): a quiet, keyboard-first
native Rust/GPUI window that follows CrowbarOS's appearance preferences
(`Dark`, `Light`, `Warm Paper`, and neutral `Studio Mode`) and reads folders on
demand.

## Features

- **Three views**: Finder-style Miller columns (`Ctrl+1`), a detail list with
  item counts, sizes, and modified dates (`Ctrl+2`, default), and a preview
  grid with image thumbnails (`Ctrl+3`). Thumbnails are for still images, and
  stop at a budget of 64 megapixels, about 256 MB decoded, a pane.
- **Inspector pane**: Live preview of images up to 40 megapixels and of
  text/code files,
  file metadata, and a primary `Open folder` / `Open` / `Restore` action
  (`Ctrl+I` to toggle).
- **Space quick preview**: Instant full-size preview overlay for images,
  text/code files, and folder contents.
- **Sidebar**: `HOME` directories, pinned folders (`Ctrl+P`), `Views`
  (`Recents` and `Trash` with restore), `Drives`, and the `?` shortcuts sheet.
- **Split pane**: Side-by-side browsing (`Ctrl+Alt+N`, `Tab` to switch).
- **Safe file operations**: Copy, cut, paste, duplicate, rename, new folder,
  and FreeDesktop Trash with restore. Operations never overwrite an existing
  target, even one that appears while they run. A move or a trash between
  filesystems copies first and removes the original only once the copy is
  whole; a copy that fails partway removes what it made. Files does not copy
  pipes, sockets, or devices, and never reads them for a preview.

## Build and run on Void

Files uses the shell's patched GPUI 0.2.2 (`apps/shell/gpui-source`) and the
bundled `Manrope` and `IBM Plex Mono` fonts in `apps/shell/fonts/`. From this
directory:

```sh
python3 ../shell/packaging/prepare-gpui.py
cargo build --release --locked
cargo test --release --locked
target/release/crowbaros-files --version
```

Pass an optional path or `file://` URI to open directly at that location:

```sh
target/release/crowbaros-files ~/Code
```

## Keyboard shortcuts

| Input | Action |
| --- | --- |
| Arrows, Enter | Move selection, open folder or file |
| Space | Quick preview |
| Backspace | Parent folder |
| Alt+Left / Alt+Right | Back / Forward |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Columns / List / Grid view |
| Ctrl+L | Type a path |
| / | Search in current folder and below |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy, cut, paste |
| Ctrl+D | Duplicate selected item |
| F2 | Rename |
| Delete / Shift+Delete | Move to Trash / Delete permanently |
| Ctrl+Shift+N | New folder |
| Ctrl+P | Pin or unpin folder |
| Ctrl+H | Show or hide hidden files |
| Ctrl+I | Toggle inspector pane |
| Ctrl+Alt+N, Tab | Split pane, switch active pane |
| Alt+Enter | Context menu |
| Ctrl+Shift+T | Open terminal here |
| ? | Shortcuts help |
| Escape | Dismiss overlay, prompt, or search |
| Ctrl+Q | Quit |

## Licensing

Files' source is MIT ([LICENSE](LICENSE)). `src/input.rs` adapts GPUI 0.2.2's
input example and keeps its Apache-2.0 terms
([licenses/GPUI-APACHE-2.0](licenses/GPUI-APACHE-2.0)). Files uses the shell's
Manrope and IBM Plex Mono fonts, which keep their OFL notices in
[`apps/shell/fonts/`](../shell/fonts/). Its icons are original
([assets/README.md](assets/README.md)).
