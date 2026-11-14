CROWBAROS — BRAND KIT 1.0
Created 13 November 2026
Architecture notes updated 14 November 2026

START HERE
  CrowbarOS-brand-overview.png — one-page visual overview
  guide/CrowbarOS-brand-guide.pdf — 14-page usage and design guide
  mockups/desktop.html — local interactive desktop studies

Extract the complete ZIP before opening desktop.html. All fonts, images and
styles are local. Scene selection, appearance buttons and Studio Mode work in
the viewer. App chrome and other operating-system controls are illustrative.
Nothing in this kit installs or changes a desktop configuration.
The PDF, PNGs, and exported ZIP are version-1.0 visual snapshots. For current
architecture decisions, use ../../community_foss_distro_plan.md. Existing
horizontal compositions are references for optional tiling/scrolling.

IDENTITY
  Name: CrowbarOS (one word, capital C, O and S)
  Brand line: Quiet surface. Sharp mind.
  Descriptor: A community-built Linux desktop.
  Void #080D12 / Abyss #17212B / Ice #49D7E8 / Steel #6D8191 / Frost #D8E5EC
  Original colors: Nullkeel. Original crow concept: Rookslash.

LOGOS
  18 true SVG masters with outlined lettering; 31 PNG exports.
  Four colorways: primary (Ice + Frost), dark (Void), frost, ice.
  Horizontal, stacked, mark-only and wordmark-only in each colorway.
  Extra micro mark and app icon. PNG icon sizes: 16–1024 pixels.
  Standard logo PNGs have alpha; app icons have a rounded Void ground.
  Use the dark version on light grounds. Do not use Frost on white.
  Clear space: at least one-quarter of visible symbol height.
  Minimums: horizontal 160 px; stacked 140 px; standard mark 24 px.
  Use the supplied micro mark at 16–23 px.

MASCOT
  mascot/crow-master.png — native 1024 × 1536 RGBA.
  Full engraved crow, right-facing, one cyan wing feather, slate perch.
  Transparency checked on dark and light grounds.

WALLPAPERS
  Signal Feather — 1672 × 941 PNG, native generated raster.
  Nightwatch — 1672 × 941 PNG, native generated raster.
  Signal Feather Mobile — 941 × 1672 PNG, native generated raster.
  Quiet Signal — 3840 × 2160 PNG plus scalable SVG.
  Quiet Signal Ultrawide — 3440 × 1440 PNG plus scalable SVG.
  CrowbarOS Signature — 3840 × 2160 PNG plus scalable SVG.
  Vector wallpaper exports are rendered directly at the listed dimensions.
  Raster originals are not upscaled. Use fit/contain if a display crop removes
  the crow or its perch. Native sizes are recorded in asset-manifest.json.

DESKTOP STUDIES
  Eight PNGs at 1600 × 900: workspace, Zen, dark settings, light settings,
  Warm Paper settings, neutral Studio Mode, lock screen and boot splash.
  Original layout studies predate the floating-window default. The selected
  native stack is Fedora (non-Atomic), Hyprland, Rust, and GPUI. Five default apps:
  Zen Browser, Nautilus Files, Ghostty, Studio Settings and Bazaar.
  Glyphs are original illustrative role icons, not upstream app logos.
  Demo values, filenames, clock, user and app content are synthetic.
  Zen's writing sample represents an optional user-installed editor.
  Studio's neutral colors are a visual treatment, not ICC calibration.

EXTRAS
  social/ — 1200 × 630 share card and 1500 × 500 profile header.
  fonts/ — unmodified open-source fonts, OFL texts and source URLs.
  tokens/ — CSS, JSON and calculated contrast checks.
  DESIGN.md + .impeccable/design.json — documented built system.
  source/ — exact prompts, source artwork provenance, render scripts and checks.

REBUILD
  build_identity.py uses Python 3 + fontTools (temporary import path can be
  adapted). Run it first; it also creates temporary fonts for build_guide.py.
  render_assets.cjs uses Sharp + Playwright and local Chromium.
  render_layouts.cjs renders the overview and social layouts.
  build_guide.py uses ReportLab + Pillow (Pillow reads dimensions only).
  Runtime paths reflect the authoring workstation; adjust them for another
  machine. Shipped assets and the local viewer need no build dependencies.

PROVENANCE AND RIGHTS
  Four new raster assets were created with the built-in image generator using
  the approved cyan-crow study as reference. Exact prompts are preserved in
  source/image-prompts.json. SVGs and UI/layouts are editable native artwork.
  The PNG mascot is raster art; it is not an editable vector illustration.
  Fonts retain their included OFL licenses. No blanket artwork license is
  assigned here; the project owner can select the distribution policy.
  No trademark clearance, domain purchase or OS release is implied.
