# Viewer evidence after rebasing

This full-build run passed **305 checks, 0 failures**. See the
[validation record](../../viewer-rebase-validation-2026-11-26.md) for commits,
commands, results, and limits.

- [Desktop log](desktop.log), [results](results.json), and [Void build](build.log).
- [Initial component checks](viewer-refresh-components.log) and
  [checks repeated after main moved](viewer-current-main-components.log).
- [SHA-256 checksums](SHA256SUMS) cover the published logs, results, and media.

The original evidence remains unchanged. Publication copies replace the host
checkout path with `/workspace/crowbaros` and the host home path with
`/home/build`. Guest account paths and QEMU's own
addresses are retained. Clipboard/window secrecy markers and the subagent
environment fixture are synthetic test data, not credentials. The build uses
the repository's documented disposable lab password.

| Theme | Before | Image | PDF | Thumbnails | Motion |
| --- | --- | --- | --- | --- | --- |
| Dark | [Capture](viewer-image-dark-before.png) | [Capture](viewer-image-dark-after.png) | [Capture](viewer-pdf-dark-after.png) | [Capture](viewer-pdf-dark-thumbnails.png) | [Six seconds](viewer-image-dark-motion.mp4) |
| Light | [Capture](viewer-image-light-before.png) | [Capture](viewer-image-light-after.png) | [Capture](viewer-pdf-light-after.png) | [Capture](viewer-pdf-light-thumbnails.png) | [Six seconds](viewer-image-light-motion.mp4) |

PDF text selection: [Dark](viewer-pdf-dark-selection.png),
[Light](viewer-pdf-light-selection.png).
Wallpaper after shell restart: [Dark](viewer-wallpaper-dark-restarted.png),
[Light](viewer-wallpaper-light-restarted.png).
