# Theme Studio finish review packet

Direction: `.impeccable/surfaces/apps-theme-studio-src-ui-rs.md`.
Established CrowbarOS native world, Operate, code-led. No new visual system.

All PNGs in this directory were opened and checked by the build thread.
`before-desktop.png` is the clean main desktop after Calculator merged
(7a2423f), before the palette button and Theme Studio existed. Other captures
show this issue's actual Void/GPUI/Fulcrum implementation. Source imagery is
CrowbarOS's existing bundled wallpapers, not new or external artwork.

- `studio-final-empty`: default Dark, empty editor and initial native input focus.
- `studio-final-extracted`: image loaded, extracted candidate accents, stable editor.
- `studio-final-saved`: a palette saved and applied; shell and compositor colors follow.
- `studio-light-preview-final`: Light candidate preview within a Dark desktop.
- `studio-load-error`: missing source image preserves the draft and shows an error.
- `studio-compact`, `studio-compact-scrolled`: 820 × 660 minimum window,
  scrolling body and fixed reachable footer; both backgrounds remain manageable.
- `switcher-custom-dark-final`: the custom theme in the installed list.
- `switcher-light-final`: live Light theme with its background pills.
- `switcher-keyboard-create-focus`: Shift+Tab reaches Create theme… .
- `studio-light-from-switcher`: Create theme… opens the app and dismisses the overlay.

Captures are 1280 × 960 desktop frames; ordinary app size is 1000 × 850,
compact app size 820 × 660. The shell popover is 380 pixels wide.

Two own batched inspections covered empty, loaded, save, failure, focus,
Light/Dark, compact, scrolling, and launch. Fixes between them added initial
input focus, the shared titlebar and Close, stable scrolling rather than
shrinking controls, and candidate spacing/selection. Both pointer and
keyboard Create theme launch paths were exercised. Full graphical checks
are running separately; these iteration captures are visual evidence, not
claims that the full check has passed. Native GPUI has no applicable HTML/CSS
detector targets.

Review source in apps/theme-studio/src/ui.rs and apps/shell/src/theme_switcher.rs
alongside the shared tokens and these captures. The generated palette uses
contrast guarantees; the saved custom palette's borders are more prominent
than the built-ins, so assess that result against the established world.
PRODUCT.md and DESIGN.md have pre-existing substrate-era wording; the current
vision and ADRs govern behavior. Preserve those system files.

## Concept provenance and scope

No `concept-seed` roll or decision-page lock was run, and no seed approval is
claimed. New-work step 3, under “Create a whole surface inside an established
world”, says: “Never run the script for a local extension or a precisely
specified narrow request; shape those directly.”

Applicability judgment: #368 specifies the native theming controls—installed
themes, image extraction, candidate accents, Dark/Light palettes, background
attachments, save/apply, and a compact shell switcher. The shell addition is
a local extension. Theme Studio uses that precise task and the existing
native list/editor pattern, without an open visual-world exercise. That is
why the build used the direct-shaping exception. This is the build thread’s
scope judgment, not a claim that a roll or user lock occurred. Calculator
and Studio Settings remain the quality authority; no durable system change
was proposed.
