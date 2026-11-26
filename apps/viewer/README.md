# Viewer

Viewer (`crowbaros-viewer`, `com.crowbaros.Viewer`) opens an image or PDF in a
native GPUI window. It follows CrowbarOS's theme and uses the shared
`crowbar-ui` typography and palette. Base registers Viewer as the default for
PNG, JPEG, WebP, BMP, TIFF, SVG, and PDF files.

Images support zoom, fit, actual size, rotation, sibling-image navigation,
metadata inspection, and copying. PDF pages use Poppler's `pdfinfo`,
`pdftoppm`, and `pdftotext` utilities, supplied by Void's `poppler` package.
Rendering and file reads run away from the UI thread. PDFs offer page
navigation, thumbnails, text search, and a selectable text panel.

| Keys | Action |
| --- | --- |
| Ctrl+O | Open a path |
| Ctrl+= / Ctrl+- | Zoom in / out |
| Ctrl+0 / Ctrl+1 | Fit / actual size |
| Ctrl+R | Rotate an image clockwise |
| Left / Right | Previous / next sibling image |
| Ctrl+I | Toggle image metadata |
| Ctrl+C | Copy the image or selected text |
| Page Up / Page Down | Previous / next PDF page |
| Ctrl+G | Go to a PDF page |
| Ctrl+F, F3 | Find text in a PDF, move to the next match |
| Ctrl+B | Toggle PDF thumbnails |
| Ctrl+T | Toggle selectable PDF text |
| Escape | Dismiss a prompt |
| Ctrl+Q | Quit |

## Build and run on Void

With Base's GPUI build dependencies and `poppler` installed, from this
directory:

```sh
python3 ../shell/packaging/prepare-gpui.py
cargo build --release --locked
cargo test --locked
target/release/crowbaros-viewer --version
target/release/crowbaros-viewer picture.png
target/release/crowbaros-viewer document.pdf
```

`--inspect PATH` reads a document without opening a window and prints JSON
with its kind and dimensions or page count. It exercises the same input
guards used by the window. Non-regular files, oversized inputs, and images
whose dimensions exceed the decoder's budget are refused.

From the repository root, `lab/void/desktop_check.py` builds and tests Viewer
in Base's Void build root, verifies its binary hash and libraries, then opens
a real PNG and a two-page PDF through their MIME defaults on Fulcrum. It
exercises zoom, rotation, metadata, page navigation, and PDF search, and
retains Dark and Light captures with `desktop.log` and `results.json`.

## Image limits

Raster images are limited to 64 MiB on disk, 40 megapixels, 16,384 pixels on
either side, and 160 MiB of native decoded pixels. Dimensions must be readable
within the first 256 KiB before decoding begins. SVGs are limited to 4 MiB,
20,000 drawing nodes, 200,000 path segments, and 128 nested groups, with the
same final canvas size limits. Simple vectors, gradients, and text work.
Viewer refuses SVG document type declarations, linked or embedded images,
pattern fills/strokes, filters, masks, clipping, opacity groups, and blending:
those rendering paths can allocate intermediate images outside the canvas
budget. An unsupported SVG produces an inline error instead of a partial
rendering.

## Issue status

This is the first local implementation for #362. **Set as wallpaper remains
outstanding**: the shell runs as a separate account and currently accepts
only installed backgrounds. Custom images need the bounded transfer through
the launcher also planned in #368, rather than access to arbitrary paths in
the person's home. The issue stays open until that action and the remaining
interface acceptance work are verified.

PDF text selection is in the **Select text** panel; it does not overlay text
on the page image. The thumbnail rail currently reuses nearby rendered pages;
other pages show their number until visited. These are explicit limitations
of this checkpoint, not a completed Preview-equivalent interface.
