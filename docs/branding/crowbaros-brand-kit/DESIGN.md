---
name: CrowbarOS
description: Quiet surface. Sharp mind. — Nullkeel palette and Rookslash crow.
colors:
  void: '#080D12'
  abyss: '#17212B'
  ice: '#49D7E8'
  steel: '#6D8191'
  frost: '#D8E5EC'
  secondary-text: '#A8BAC7'
  dark-line: '#364957'
  recessed-surface: '#111B24'
  selected-surface: '#263B48'
  files-selected: '#273B48'
  light-surface: '#EAF0F3'
  light-secondary-text: '#3E515D'
  light-line: '#B2C2CC'
  paper-surface: '#EAE3D7'
  paper-text: '#211F1C'
  paper-secondary-text: '#5C564B'
  paper-line: '#BCB3A3'
  studio-background: '#111111'
  studio-surface: '#212121'
  studio-text: '#E5E5E5'
  studio-secondary-text: '#B8B8B8'
  studio-line: '#555'
  studio-selected: '#3B3B3B'
typography:
  display:
    fontFamily: Manrope, sans-serif
    fontSize: 42px
    fontWeight: 700
    lineHeight: 1.18
    letterSpacing: -0.03em
  headline:
    fontFamily: Manrope, sans-serif
    fontSize: 38px
    fontWeight: 700
    lineHeight: 1.13
    letterSpacing: -0.035em
  title:
    fontFamily: Manrope, sans-serif
    fontSize: 28px
    fontWeight: 700
    letterSpacing: -0.025em
  body:
    fontFamily: Manrope, sans-serif
    fontSize: 16px
    fontWeight: 400
  caption:
    fontFamily: Manrope, sans-serif
    fontSize: 15px
    fontWeight: 400
    lineHeight: 1.7
  writing:
    fontFamily: Manrope, sans-serif
    fontSize: 17px
    fontWeight: 400
    lineHeight: 1.85
  label:
    fontFamily: Manrope, sans-serif
    fontSize: 14px
    fontWeight: 650
  small:
    fontFamily: Manrope, sans-serif
    fontSize: 12px
    fontWeight: 400
  code:
    fontFamily: IBM Plex Mono, monospace
    fontSize: 13px
    fontWeight: 400
    lineHeight: 2
rounded:
  window: 16px
  control: 10px
  theme: 9px
  thumbnail: 8px
  compact: 6px
  switch: 20px
  circle: 50%
spacing:
  space-4: 4px
  space-8: 8px
  space-12: 12px
  space-16: 16px
  space-24: 24px
  space-32: 32px
  space-48: 48px
  space-64: 64px
components:
  scene-button:
    backgroundColor: transparent
    textColor: '{colors.secondary-text}'
    typography: '{typography.label}'
    rounded: '{rounded.control}'
    padding: 12px 19px
  scene-button-hover:
    backgroundColor: '{colors.abyss}'
    textColor: '{colors.frost}'
  scene-button-active:
    backgroundColor: '{colors.ice}'
    textColor: '{colors.void}'
    typography: '{typography.label}'
    rounded: '{rounded.control}'
    padding: 12px 19px
  theme-button:
    backgroundColor: transparent
    textColor: '{colors.frost}'
    rounded: '{rounded.theme}'
    padding: 10px 20px
  theme-button-active:
    backgroundColor: '{colors.ice}'
    textColor: '{colors.void}'
    rounded: '{rounded.theme}'
    padding: 10px 20px
  studio-switch:
    backgroundColor: '{colors.dark-line}'
    rounded: '{rounded.switch}'
    padding: 3px
    width: 44px
    height: 25px
  studio-switch-on:
    backgroundColor: '{colors.ice}'
    rounded: '{rounded.switch}'
    padding: 3px
    width: 44px
    height: 25px
  window:
    backgroundColor: '{colors.abyss}'
    textColor: '{colors.frost}'
    rounded: '{rounded.window}'
  status-tag:
    backgroundColor: '{colors.abyss}'
    textColor: '{colors.frost}'
    typography: '{typography.small}'
    rounded: '{rounded.compact}'
    padding: 4px 9px
  password-study:
    backgroundColor: '{colors.void}'
    textColor: '{colors.secondary-text}'
    rounded: '{rounded.control}'
    padding: 15px 17px
  dock:
    backgroundColor: '{colors.abyss}'
    textColor: '{colors.frost}'
    rounded: '{rounded.window}'
    padding: 12px 18px 10px
  scene-navigation:
    padding: 12px 0 20px
---

# Design System: CrowbarOS

## Overview

**Creative North Star: "Quiet surface. Sharp mind."**

CrowbarOS pairs a restrained technical surface with an observant engraved crow. The approved Nullkeel colors establish a quiet blue-black field; precise feather texture and a controlled cyan signal provide character. Proportional type keeps the identity approachable to everyday users, writers, creatives, makers and technically fluent Linux users.

This document records the completed brand kit and its local HTML review viewer for native Linux desktop concepts. It is not a shipped operating system or a production website. The selected native stack is Fedora (non-Atomic), Hyprland, Rust, and GPUI; ../../community_foss_distro_plan.md is authoritative for architecture. The viewer does not install software or change system settings. Its existing horizontal compositions predate the floating-window default and remain optional-layout studies. The approved direction is recorded in source/direction-contract.txt and source/finish-review.txt. Prior visual authorities are ../round-02-dark/assets/05-rookslash.png and ../round-03-cyan-crow/cyan-crow-name-studies.png.

The YAML frontmatter is the portable token snapshot. Its five brand colors, families and base scales preserve tokens/crowbaros.tokens.json and tokens/crowbaros.css; additional utility colors and component treatments are extracted from mockups/desktop.css. The sidecar extends this snapshot with rendering samples and properties outside the frontmatter schema. Keep these documents synchronized with their implementation sources.

**Key Characteristics:**

- Quiet surfaces with one clear accent for focus and selection.
- Organic engraved artwork against precise, restrained interface geometry.
- Proportional UI type; monospace reserved for code, paths and technical data.
- Dark identity with Light, Warm Paper and neutral Studio choices preserved.

## Colors

The five Nullkeel base colors are fixed brand assets. Utility colors provide readable UI across the appearance variants; they are not extra brand swatches. Exact values live in the frontmatter.

### Primary

**Ice** is the controlled signal for the slash, active window outline, selected controls, progress and the crow’s accent feather. On filled controls, pair Ice with Void text.

### Neutral

**Void** is the canvas and terminal ground. **Abyss** carries windows and raised chrome. **Frost** is primary dark-mode text. **Steel** is decorative structure and muted graphical detail. **Secondary Text** is the brighter functional support color for readable secondary text. **Dark Line** supplies separators; **Recessed Surface**, **Selected Surface** and **Files Selected** describe the authored sidebar and reading-plane treatments.

| Appearance | Surface | Primary text | Secondary text | Separators | Accent |
| --- | --- | --- | --- | --- | --- |
| Dark | abyss | frost | secondary-text | dark-line | ice |
| Light | light-surface | void | light-secondary-text | light-line | ice |
| Warm Paper | paper-surface | paper-text | paper-secondary-text | paper-line | ice |
| Studio | studio-surface | studio-text | studio-secondary-text | studio-line | studio-text |

Studio additionally uses studio-background for the bar, dock, tags and inactive width chips, and studio-selected for selected settings navigation. The appearance study changes the settings window surfaces. Studio neutralizes its chrome and mark treatment; the image wallpaper remains artwork. This prototype does not demonstrate display calibration or color-management implementation.

**The Signal Rule.** Use Ice to identify focus, selection and a controlled artwork accent. Keep Steel decorative; use secondary-text for normal secondary text on dark surfaces.

**The Choice Rule.** Dark is the brand default. Preserve Light and Warm Paper as utility palettes and Studio Mode as a neutral chrome override. A dark identity does not cancel those choices.

The finish review records normal-text contrast checks above 4.5:1 for the authored Dark, Light, Warm Paper and Studio secondary-text pairings and the active button. Preserve those pairings and recheck contrast when changing surfaces. Do not infer that every decorative color is a normal-text color.

## Typography

**Display, body and UI:** Manrope, with a sans-serif fallback. The bundled variable font supports weights 200–800. **Code:** IBM Plex Mono Regular, with a monospace fallback. The fonts are loaded locally; no network font request is required.

The hierarchy is deliberately proportional and calm. The frontmatter records the actual viewer sizes and tracking: display for the centered writing headline, headline for the welcome composition, title for settings and captions, body for the base UI, caption for explanatory copy, writing for long-form text, label for chrome, small for metadata, and code for the terminal. It is an observed hierarchy rather than a mathematical type scale.

Welcome paragraphs use 14px type at 1.7 line-height with a 47ch measure. External viewer captions use the caption role with a 72ch maximum measure. Native scene metadata remains 12–14px; do not make it smaller to fit a phone preview. The lock clock is a special 126px, weight-500 treatment with line-height 1 and -0.04em tracking. Both clocks use tabular numerals.

The wordmark derives from Manrope 700 with custom spacing and outlined lettering. Supplied SVG paths are authoritative; typing “CrowbarOS” in Manrope is not an equivalent logo. UI copy uses sentence case and direct language. Technical fluency should not become gatekeeping or unsupported security promises.

**The Human Type Rule.** Use Manrope for the interface and IBM Plex Mono for code, paths and technical data. Always use the supplied outlined master for the wordmark.

## Layout

The base spacing scale is recorded in the frontmatter. It is a rhythm, not a claim that every authored coordinate uses the scale. Local optical adjustments remain in the scene source.

The desktop studies use a fixed 1600 × 900 canvas. In the workspace, adjacent windows form the horizontal ribbon: Files is 408px wide, the active browser 764px, and the terminal 546px. Windows are 650px high, start 98px below the top edge, and have 22px gaps. The intentionally cropped terminal edge indicates more horizontal workspace. These are composition measurements for this study, not mandatory dimensions for every native window.

The top bar is 46px high with 24px horizontal padding. Window title bars are 54px high with 20px horizontal padding. The compact five-app dock sits 22px from the bottom; its items have 82px widths and 12px gaps. The dock roles are Zen Browser, Nautilus Files, Ghostty, Studio Settings and Bazaar.

Zen removes the top bar and dock, dims the wallpaper, and centers a 794px-wide document with 58px vertical and 88px horizontal padding. The writing sample represents a user-installed editor and does not add a sixth bundled app. Lock uses a quiet left field for time and sign-in; boot gives the outlined logo and a single progress rule generous space.

The review wrapper has a 1680px maximum width and 40px side padding, a 16:9 stage, a separate horizontally scrollable scene selector, and explanatory captions below. At 700px and below, wrapper side padding becomes 20px, navigation and captions reduce in size, and the caption columns stack. A ResizeObserver scales the complete desktop canvas to fit its container. Phones see the full desktop study and readable external explanation; the in-scene desktop does not reflow into a mobile operating system. The raw query mode hides review framing for scene capture.

## Elevation & Depth

Depth combines distinct surface tones with soft offset shadows. Engraved feather detail belongs to the wallpaper and mascot; windows remain quiet opaque planes. The focus outline identifies the active window without turning every surface into a glowing object.

- Window shadow: `0 18px 50px #0006`.
- Dock shadow: `0 14px 36px #0008`.
- Zen document shadow: `0 20px 60px #0006`.
- Zen wallpaper dimmer: `#080D12bb`.
- Active window: 2px Ice outline with zero offset.
- Viewer keyboard focus: 3px Ice outline with 5px offset.

The only authored entrance is a 400ms brightness change from 0.8 to 1 using `cubic-bezier(.16,1,.3,1)`. It runs only when reduced motion is not requested. The default remains visible without animation. No additional motion system or generic hover lift is implied.

## Shapes

Windows, the dock and the focused writing document share the window radius. Viewer scene buttons and the password study use the control radius. Theme choices and wallpaper previews use the theme radius; thumbnails and settings rows use the thumbnail radius. Status tags use the compact radius. The switch track is a pill with a circular 19px thumb. Borders are restrained one-pixel separators; active controls use a visible fill or outline.

The logo’s slash and simplified crow head supply directional geometry. Keep their proportions, angle and orientation intact. Do not replace this identity with generic geometric ornament. UI role glyphs are consistent open strokes: a 24-unit viewBox, 1.7-unit stroke, round caps and joins, typically displayed at 20px; dock glyphs are 27px. These are illustrative role glyphs, not upstream application trademarks or a completed native icon theme.

## Components

### Logo family

The supplied families are **horizontal**, **stacked**, **wordmark-only** and **symbol-only**. Each has primary, Frost, Ice and dark variants. Primary pairs the cyan slash with Frost on Void or Abyss; dark uses Void on light surfaces. Frost and Ice are the authored single-color alternatives. App icons place the mark on a solid Void ground; other logo PNGs preserve alpha.

Keep clear space equal to one quarter of the visible symbol height. Minimum digital widths are 160px for horizontal (240px or more preferred in headers) and 140px for stacked. The standard symbol starts at 24px; use the supplied micro symbol at 16–23px. The micro form removes delicate eye detail. Do not invent a wordmark-only minimum that has not been established. SVG masters are outlined and scalable; small PNG mark exports cover 16–1024px.

### Crow and wallpaper family

The mascot is a natural right-facing crow with a folded wing, slate perch, precise engraved feather structure and one cyan wing feather. Preserve its head and beak, overall silhouette and orientation. Use the complete silhouette for hero placements. Switch to the vector symbol below roughly 180px of mascot height, where the engraving stops reading clearly.

The transparent mascot master is 1024 × 1536. It is supplied as a genuine transparent PNG and has been reviewed on Void, Abyss, Frost and white. Do not create a geometric background-removal mask or add a rectangular matte.

Signal Feather and Nightwatch are native 1672 × 941 raster artworks; the mobile feather artwork is 941 × 1672. Keep their stated native resolution. Quiet Signal and CrowbarOS Signature have scalable SVG masters with 3840 × 2160 PNG exports; Quiet Signal Ultrawide is 3440 × 1440. The feather detail sits away from important text. A vector export at large dimensions is distinct from claiming an upscaled raster is native 4K.

**The Artwork Rule.** The detailed crow is brand artwork, never an assistant, notification messenger or functional UI icon. Use the simplified vector mark at small sizes.

### Review controls

Scene buttons use the label role, control radius, Dark Line border and Secondary Text at rest. Hover adds Abyss and Frost; the pressed state uses Ice with Void text. They are real buttons with `aria-pressed`. Their five scenes replace the entire desktop composition and update the live caption. The desktop app chrome inside those scenes is illustrative.

Theme choices use 13px type, 10px × 20px padding and the theme radius. The selected choice has an Ice fill with bold Void text; hover supplies a one-pixel outline. Dark, Light and Warm Paper update this local settings concept. Studio is an independent switch, represented with `role="switch"` and `aria-checked`, and overrides the appearance surface with neutral grays while active. Viewer focus remains visible. Switching scenes reconstructs the scene and resets these preview-only choices; no preference is persisted to the OS.

The switch uses a 44px × 25px track with 3px padding and a 19px circular thumb. Off uses the line color; on uses the current accent and moves the thumb to the end. In Studio, the accent resolves to neutral text gray. The source supplies a global disabled button treatment at 45% opacity, but the presented controls are enabled.

### Native desktop concept chrome

Windows use the surface and text of the active appearance, rounded corners, one-pixel internal separators and the window shadow. The active browser receives an Ice outline. Window-width chips, close marks, app navigation, dock roles, wallpaper thumbnails and the terminal content are illustrative; they are not live controls. The status tag is a compact rounded count badge, with the neutral Studio override applied when appropriate.

The password treatment is a static visual field with a Steel border, control radius and 15px × 17px padding. It is not an input and does not collect a password. No error, validation or authentication behavior is established by this study. The boot bar is an authored visual progress state, not a real boot event. File names, time, battery values and account data are example content.

The HTML viewer uses local assets, HTML, CSS and plain JavaScript. Keep relative asset paths intact when distributing or opening it. SVG UI paths require no icon package. The rendering samples in the sidecar are self-contained visual extracts; their hover and keyboard-focus styling works, while scene switching and toggle behavior belong to the actual viewer JavaScript.

## Do's and Don'ts

### Do:

- Do preserve the five exact Nullkeel base colors and use the supplied logo masters.
- Do keep clear space around logos and leave the crow’s head, beak and silhouette readable.
- Do keep Light, Warm Paper and neutral Studio previews available alongside Dark.
- Do use labels, outlines and pressed or checked state to accompany selection color.
- Do identify the HTML as a local concept viewer and disclose illustrative app chrome and system values.
- Do preserve the five default app roles. New default-workspace studies should show familiar floating windows; retain horizontal layouts as explicitly optional tiling/scrolling studies.
- Do keep raster artworks at their documented native sizes; use vector wallpaper masters for genuine large-format exports.

### Don't:

- Don’t redraw the identity from a font, change the slash angle, distort proportions or add gradients, outlines or drop shadows to the logo.
- Don’t mirror the crow, recolor individual feathers, add clothing or give it a glowing technological eye.
- Don’t use Steel as normal body text on Abyss or spread Ice over large reading surfaces.
- Don’t turn this desktop review viewer into a supposed responsive mobile OS or imply its illustrative controls are native system functionality.
- Don’t treat role glyphs as upstream application trademarks or claim that the kit establishes artwork licensing, trademark clearance or a registered domain.
- Don’t add bundled AI affordances, release claims or invented metrics to this identity.

## Name update — 2026-11-14

The user renamed the project CrowbarOS. The palette, crow, slash geometry and fonts remain approved. Rebuilt outlined lettering uses an 850 × 180 horizontal canvas, 680 × 420 stacked canvas and 640 × 160 wordmark canvas, with preserved glyph proportions. Historical generation prompts are kept verbatim.
