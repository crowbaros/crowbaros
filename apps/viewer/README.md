# Viewer

Viewer (`crowbaros-viewer`, `com.crowbaros.Viewer`) opens an image or PDF in a
native GPUI window. It follows CrowbarOS's theme and uses the shared
`crowbar-ui` typography and palette. Base registers Viewer as the default for
PNG, JPEG, WebP, BMP, TIFF, SVG, and PDF files.

Images support zoom, fit, actual size, rotation, sibling-image navigation,
metadata inspection, copying, and setting the desktop wallpaper. PDF pages
use Poppler's `pdfinfo`, `pdftoppm`, and `pdftotext` utilities, supplied by
Void's `poppler` package.
Rendering and file reads run away from the UI thread. PDFs offer page
navigation, independently rendered thumbnails, text search, and a selectable
text panel. Search moves through individual occurrences, including several
on the same page, and shows the current result with surrounding text.

| Keys | Action |
| --- | --- |
| Ctrl+O | Open a path |
| Ctrl+= / Ctrl+- | Zoom in / out |
| Ctrl+0 / Ctrl+1 | Fit / actual size |
| Ctrl+R | Rotate an image clockwise |
| Left / Right | Previous / next sibling image |
| Ctrl+I | Toggle image metadata |
| Ctrl+C | Copy the image or selected text |
| Ctrl+Shift+W | Set the image as wallpaper |
| Ctrl+Alt+W | Restore the built-in wallpaper |
| Page Up / Page Down | Previous / next PDF page |
| Ctrl+G | Go to a PDF page |
| Ctrl+F | Find text in a PDF |
| F3 / Shift+F3 | Next / previous text match |
| Ctrl+B | Toggle PDF thumbnails |
| Ctrl+T | Toggle PDF text; opening it focuses keyboard selection |
| Ctrl+Shift+C | Copy the current PDF page's text |
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
real PNGs and a twelve-page PDF through their MIME defaults on Fulcrum. It
exercises zoom, panning, rotation, metadata, sibling navigation, image and
selected-text copying, individual search matches, and distant thumbnails.
It also sets wallpaper, restarts the shell to check persistence, and restores
the built-in background. The person's original preferences are restored even
when a check fails. The run retains Dark and Light captures and short image
panning recordings beside `desktop.log` and `results.json`. Recording needs
`ffmpeg` on the host; the lab image supplies `wl-paste` for clipboard checks.

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

## Wallpaper and PDF text

**Set as wallpaper** saves the image with its current rotation. Viewer reduces
it to at most 4,194,304 pixels and 4,096 pixels on either side, then atomically
replaces `~/.config/crowbaros/wallpaper.rgba`. The launcher validates and sends
those normalized pixels to the shell, which follows changes and restores the
saved background when it starts. The shell never opens the source image or
runs its image decoder. **Restore wallpaper** removes that override and returns
to the background selected in Studio Settings.

PDF text selection is in the **Select text** panel; it does not overlay text
on the page image. Click or open that panel to select with the mouse, Shift
and the arrow keys, or Ctrl+A; Ctrl+C copies the selection. **Copy page** copies
all available text from that page. Scanned pages without text say so. The
thumbnail rail renders its own visible pages and neighbors, without requiring
the document to visit those pages first.
