# CrowbarOS project blueprint

The deliverable is **`../crowbaros.html`**. Open that file directly in a current browser. It is approximately 3.41 MiB and works offline; it can be moved or shared by itself. This source directory is for maintenance and is not needed to view the page.

The HTML contains its CSS, JavaScript, Manrope and IBM Plex Mono fonts, initial images, complete source Markdown, and compressed WebP previews for the desktop and wallpaper galleries. Small text references and original SVG logo downloads are embedded too. The production brand-kit archive and PDF are deliberately excluded. No server, CDN, adjacent folder, or network request is required. Interactive galleries and downloads require JavaScript.

## Content and navigation

Nine chapters cover the overview, principles, brand, desktop, architecture, software, governance, roadmap, and resources. Search includes the full reference text and reveals matching collapsed details. Use `/` or `Ctrl/Cmd+K` to search, arrow keys to select, and Enter to jump. A compact Contents menu replaces the chapter rail on smaller screens.

The page includes all eight desktop scenes, six wallpapers, four logo colorways, copyable palette values, a filterable system package table, governance and architecture diagrams, expandable milestones, and the complete rendered blueprint. The original diagram notation is retained alongside the visual summaries. Native OS behavior is presented as a planned concept. The selected stack is Fedora (non-Atomic, RPM/DNF), a maintained Hyprland fork, Rust for CrowbarOS-owned apps and services, and GPUI for their interfaces and shell UI. Fedora 45 is the local build/test VM baseline; production signing/hosting, the installer, support lifetime, and recovery implementation remain to be finalized. Existing side-by-side desktop images predate the floating-window default and remain visual studies.

## Sources and regeneration

- `../community_foss_distro_plan.md` is the canonical plan. `build.mjs` derives the complete reference, system package rows, core app rows, and downloadable source from it.
- `page.template.html`, `page.css`, and `page.js` define the editorial presentation and interactions.
- `embedded.js` reads the compact embedded asset map and creates local download URLs.
- `../branding/crowbaros-brand-kit/` and `../branding/CrowbarOS-brand-kit.zip` are the approved artwork and brand references. Raster provenance and font notices remain in that kit.
- The build currently uses Marked and Sharp from the installed Codex Node runtime; no package is required by the delivered HTML.

From the project root in this workspace:

```sh
rtk /home/jasona/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node docs/crowbaros-page/build.mjs
```

After changing canonical content, update any corresponding editorial summaries in the template before rebuilding. After changing brand assets, rebuild to update all previews. `visual-provenance.json` records each preview’s source, compression quality, dimensions, and size. Original assets remain untouched. Do not edit the generated HTML or its encoded assets directly.

## Verification

`verify.cjs` uses installed Playwright and Chromium. It copies only the HTML into an empty temporary folder, opens it through `file://`, and blocks any unexpected resource request. It checks original source equality, embedded asset access, byte-identical SVG/text downloads, WebP preview downloads, skip-link focus, a file size below 5 MiB, all scenes and wallpapers, search, package filtering, image dialogs, mobile navigation, and viewport overflow. Evidence is under `../../.impeccable/review/crowbaros/`; `content-coverage.json` records source coverage and its SHA-256.

```sh
rtk /home/jasona/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node docs/crowbaros-page/verify.cjs
```

Chromium may require the workspace's local browser execution permission. The verification was performed in Chromium; other browser engines were not exercised.
