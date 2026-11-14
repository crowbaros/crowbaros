---
version: 1
slug: "apps-shell-src-main-rs"
primary_target: "apps/shell/src/main.rs"
related_targets: []
---

# CrowbarOS first desktop shell

Mode: Operate. Native Linux, Rust/GPUI on Hyprland. Extend the approved desktop
composition and Studio Settings controls. User explicitly requests a working bar,
dock, launcher, RPM, and visible VM validation. This is an implementation slice
of an established design, not a new identity or concept-selection exercise.

## Direction contract

THESIS: Make the existing VM usable through a real desktop shell: see context,
switch workspace, find an installed app, launch it, and return to work.

OWN-WORLD: Preserve Void/Abyss, Ice state indicators, Frost text, Manrope,
open-stroke icons, the approved mark and Signal Feather wallpaper. Follow saved
appearance preferences, including Studio's neutral override.

STORY: The bar explains the current workspace/window and time. The centered dock
opens or focuses installed tools. Applications opens a search surface; typing,
arrows and Enter launch, Escape or clicking outside returns focus.

FIRST VIEWPORT: A 44px top bar; an unobtrusive centered bottom dock; open workspace
between them. Launcher appears above the desktop with a focused search field,
plain result rows, an explicit empty state and recoverable launch errors.
Bar/dock are Wayland layer surfaces with reserved space and no keyboard grab.

FORM: Direct code implementation of the approved desktop study. Seed not applicable
to this specified extension. Initial dock is persistent; only installed applications
appear. Full production shell features and multi-monitor certification remain later.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
