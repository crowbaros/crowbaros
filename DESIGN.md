---
name: CrowbarOS
description: Quiet surface. Sharp mind. — the approved identity extended into native Studio Settings.
colors:
  void: "#080D12"
  abyss: "#17212B"
  ice: "#49D7E8"
  steel: "#6D8191"
  frost: "#D8E5EC"
  secondary-text: "#A8BAC7"
  dark-line: "#364957"
  light-canvas: "#F4F7F9"
  light-surface: "#EAF0F3"
  light-secondary-text: "#3E515D"
  light-line: "#B2C2CC"
  light-focus: "#086A78"
  paper-canvas: "#F7F2E8"
  paper-surface: "#EAE3D7"
  paper-text: "#211F1C"
  paper-secondary-text: "#5C564B"
  paper-line: "#BCB3A3"
  paper-focus: "#526C6C"
  studio-background: "#111111"
  studio-surface: "#212121"
  studio-text: "#E5E5E5"
  studio-secondary-text: "#B8B8B8"
  studio-line: "#555555"
typography:
  title:
    fontFamily: "Manrope"
    fontSize: "28px"
    fontWeight: 700
  section:
    fontFamily: "Manrope"
    fontSize: "14px"
    fontWeight: 600
  body:
    fontFamily: "Manrope"
    fontSize: "13px"
    fontWeight: 400
  label:
    fontFamily: "Manrope"
    fontSize: "13px"
    fontWeight: 600
  supporting:
    fontFamily: "Manrope"
    fontSize: "12px"
    fontWeight: 400
  technical:
    fontFamily: "IBM Plex Mono"
    fontSize: "11px"
    fontWeight: 400
  selection-label:
    fontFamily: "Manrope"
    fontSize: "10px"
    fontWeight: 600
rounded:
  control: "10px"
  theme-card: "12px"
  theme-preview: "6px"
  circle: "50%"
spacing:
  space-4: "4px"
  space-8: "8px"
  space-12: "12px"
  space-16: "16px"
  space-20: "20px"
  space-24: "24px"
components:
  button-primary:
    backgroundColor: "{colors.ice}"
    textColor: "{colors.void}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "40px"
  button-secondary:
    backgroundColor: "{colors.abyss}"
    textColor: "{colors.frost}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "40px"
  navigation-selected:
    backgroundColor: "{colors.ice}"
    textColor: "{colors.void}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "0 16px"
    height: "40px"
  theme-card:
    backgroundColor: "{colors.abyss}"
    textColor: "{colors.frost}"
    typography: "{typography.label}"
    rounded: "{rounded.theme-card}"
    padding: "12px"
  toggle-row:
    backgroundColor: "{colors.void}"
    textColor: "{colors.frost}"
    typography: "{typography.section}"
    rounded: "{rounded.control}"
    padding: "16px 12px"
  switch-off:
    backgroundColor: "{colors.dark-line}"
    padding: "0 3px"
    width: "44px"
    height: "26px"
  switch-on:
    backgroundColor: "{colors.ice}"
    padding: "0 3px"
    width: "44px"
    height: "26px"
  save-footer:
    backgroundColor: "{colors.void}"
    textColor: "{colors.secondary-text}"
    typography: "{typography.supporting}"
    padding: "16px 24px"
  inline-error:
    backgroundColor: "{colors.void}"
    textColor: "{colors.frost}"
    typography: "{typography.supporting}"
    padding: "12px 24px"
---

# Design System: CrowbarOS

## Overview

**Creative North Star: "Quiet surface. Sharp mind."**

CrowbarOS keeps the approved Nullkeel blue-black field, controlled Ice signal, Rookslash crow and proportional Manrope interface. Engraved artwork supplies the identity's organic detail; functional controls remain quiet, precise and readable. This is an extension of that established world, not a replacement identity.

This root record combines shared brand commitments with the implemented Studio Settings surface: a native Linux desktop application built in Rust/GPUI on Wayland. The frontmatter's type, spacing and component values describe that native surface. The [brand-kit design system](docs/branding/crowbaros-brand-kit/DESIGN.md) remains authoritative for logo geometry, artwork, desktop concept compositions and the HTML review viewer. The [blueprint design system](docs/crowbaros-page/DESIGN.md) continues to govern the separate reading page. Their larger type scales, shadows and layout rules remain scoped to those surfaces.

Sources are [the canonical brand tokens](docs/branding/crowbaros-brand-kit/tokens/crowbaros.tokens.json), [native palette mapping](apps/studio-settings/src/theme.rs), [native controls](apps/studio-settings/src/main.rs), [product commitments](PRODUCT.md) and the [surface brief](.impeccable/surfaces/apps-studio-settings-src-main-rs.md). The first-build captures in [.impeccable/review/studio-settings](.impeccable/review/studio-settings/) establish the four appearance variants, compact layout, About, close protection and error placement. Current source includes the subsequent focus, native-title and error-copy corrections; those captures predate them. This document records implementation, not a final build or validation verdict. Follow the [finish review](.impeccable/review/studio-settings/finish-review.md) for that evidence.

**Key Characteristics:**

- Quiet opaque surfaces with a controlled signal for selection and action.
- Manrope for human-facing controls; IBM Plex Mono for technical data.
- Dark, Light and Warm Paper choices with a neutral Studio override.
- Familiar native window structure, explicit settings state and visible recovery.

## Colors

The five Nullkeel brand colors retain their established names and values. Supporting colors are functional palette roles, not new brand swatches. Frontmatter owns their exact values; the table maps the native palette fields to those tokens.

### Primary

**Ice** marks selected navigation, enabled primary actions and active switches. It pairs with **Void** on filled controls. **Light Focus** and **Paper Focus** deepen the selection/focus border against pale surfaces without recoloring the brand accent. Studio resolves both accent and focus to **Studio Text**, and uses **Studio Background** for text on the accent.

### Neutral

**Void** and **Abyss** separate the Dark content plane from chrome and controls. **Frost** and **Secondary Text** supply readable text. **Steel** remains an approved decorative brand color; native Studio Settings does not use it for text. The Light and Paper palettes retain distinct cool and warm grounds. Studio uses only the neutral gray family throughout the native window, including schematic previews.

| Native field | Dark | Light | Warm Paper | Studio override |
| --- | --- | --- | --- | --- |
| canvas | void | light-canvas | paper-canvas | studio-background |
| surface | abyss | light-surface | paper-surface | studio-surface |
| text | frost | void | paper-text | studio-text |
| muted | secondary-text | light-secondary-text | paper-secondary-text | studio-secondary-text |
| line | dark-line | light-line | paper-line | studio-line |
| accent | ice | ice | ice | studio-text |
| on_accent | void | void | void | studio-background |
| focus | ice | light-focus | paper-focus | studio-text |

**The Signal Rule.** Use Ice for purposeful selection and action. Keep Steel decorative and use the palette's readable muted role for supporting text.

**The Choice Rule.** Preserve Dark, Light and Warm Paper as separate choices. Studio neutralizes the current window while retaining the underlying appearance choice.

The sidecar's tonal ramps are generated swatch illustrations, not additional application tokens or approved contrast pairings. Native colors come directly from `Palette::for_preferences`.

## Typography

**Interface:** Manrope Regular, Semibold and Bold, bundled as local static fonts. **Technical text:** bundled IBM Plex Mono Regular. The native application loads these fonts directly through GPUI. Browser-side component samples name the same families with generic fallbacks; they illustrate native styling, not a web implementation.

The native hierarchy uses the title role for page headings, section for setting names, label for buttons/navigation/theme names, body for introductory and About copy, and supporting for explanations and status. The small technical role is confined to About's stack line. The selection-label role is confined to the redundant “Selected” word on theme cards; it is not a body-copy size. About also uses a regular-weight section-sized introduction. The source does not author a tracking or line-height scale, so neither is invented here.

**The Human Type Rule.** Use Manrope for the interface and IBM Plex Mono for technical information. Use the supplied outlined logo master when a wordmark is required; typed UI text is not a substitute for that asset.

## Layout

Studio Settings opens at 980 × 720 logical pixels and has an 800 × 640 minimum. A fixed-height titlebar (48px) sits above a sidebar (176px) and flexible content column. The sidebar uses 16px padding; the content uses 24px padding. The three theme choices share available width equally with 12px gaps. Section spacing uses 20px, while related labels and supporting copy use the smaller steps in the frontmatter. These are observed native measurements, not a universal desktop-window template.

The content scrolls vertically as space tightens. The titlebar, sidebar and footer remain outside that scrolling plane; the error band sits immediately above the footer when needed. The app retains its desktop composition at minimum size rather than shrinking the type or adopting a mobile navigation model. No web breakpoint or iOS/Android layout is implied.

**The Stable Actions Rule.** Keep the status and save/recovery actions reachable while the settings content scrolls.

## Elevation & Depth

The native surface authors no shadows, blur, translucent layers or entrance animation. Canvas/surface contrast and quiet one-pixel dividers establish depth. Two-pixel control borders communicate interactive state. Native window framing belongs to the compositor; the rounded, shadowed windows in the brand studies do not establish an app-authored frame here.

Hover feedback is immediate: enabled buttons use 85% opacity, theme choices 86%, and setting rows switch to the surface color. Disabled buttons use 50% opacity. No duration or easing token is implemented in this app. Reduced motion is a real preference for compositor window/workspace animation, applied on Save; it is not an authored animation inside the settings page. The separate viewer's existing shadows and reduced-motion-aware entrance remain governed by its own design record.

## Shapes

Native controls use the established gently rounded control shape. Theme cards use the slightly larger theme-card radius, with an inset preview at the theme-preview radius. Setting rows share the control radius, with their unfocused border matching the canvas. Switch tracks are fully rounded pills (44 × 26px) with a circular thumb (20px) and horizontal inset (3px). Small schematic rectangles inside theme previews are drawings of UI, not a separate card or icon system.

**The State Separation Rule.** Distinguish focus from selection. Native buttons and theme cards use a dashed focused border; primary buttons use on-accent contrast for that border. A selected theme retains its solid border and “Selected” label when unfocused. Toggle rows use a solid focus border independent of their On/Off state.

## Components

### Buttons and navigation

Buttons are compact and direct: 40px high, with 16px horizontal padding, the control radius, a two-pixel border and semibold label type. Primary uses accent/on-accent; secondary uses surface/text with the line border. Focus uses the State Separation Rule. Enabled hover and disabled opacity are recorded under Elevation & Depth. The titlebar Close control is a shorter 30px variant.

Appearance and About reuse these controls in the sidebar; the active destination uses the primary fill. Native tab focus follows visible controls. The titlebar displays “Studio Settings,” supports dragging, and the source explicitly sets the Wayland window title. Native focus/accessibility semantics are not inferred from the HTML examples in the sidecar.

### Theme choices

Each theme choice is a bordered card with 12px internal padding and spacing. Its schematic preview is 72px high and resolves from that theme's native palette. Selection adds the palette's focus-colored border and the redundant “Selected” text. Keyboard focus changes the border to dashed, preserving the distinction from selection. Under Studio, all three previews become neutral while the selected underlying theme remains labeled. Choosing a card updates the app preview immediately.

### Setting rows and switches

Each setting row combines a semibold setting name, supporting explanation, explicit On/Off text and a pill switch. The entire row is the interactive control, with 16px vertical and 12px horizontal padding and 20px gaps. Off uses line/text for track/thumb; On uses accent/on-accent. On moves the thumb to the end without authored interpolation. Studio Mode neutralizes the window immediately; Reduced motion takes effect on Save. Studio Mode does not calibrate a display.

### Status, errors and close protection

The footer uses supporting text and a top divider, with 24px horizontal and 16px vertical padding. It distinguishes preview, saving and saved states beside Undo and Save changes. Saving disables the actions that would conflict with the in-flight operation. Undo restores the last loaded/saved choices.

Errors appear in a separate band above the footer, using the current text and line colors rather than an invented error palette. The source's write-permission message names the settings directory, asks the user to restore write permission, and directs them to Save changes again. The text column wraps within the available width while Reload saved retains its width; the error band does not shrink into the footer. The draft remains available. Reload saved re-reads storage and discards the current preview; it does not repair permissions.

Unsaved close replaces the footer actions with Keep editing and Discard & close, preserving the surrounding context. The prompt focuses Keep editing; Escape cancels the close confirmation. No modal overlay is introduced. The native slice has no text input, chip/tag system or general-purpose content card; none is synthesized from the concept viewer.

### Identity and artwork

The Rookslash vector masters, engraved crow, palette and font commitments remain unchanged. The native settings content uses schematic previews and text rather than adding decorative artwork. Any later placement of the logo or crow follows the [brand-kit geometry, clear-space and artwork rules](docs/branding/crowbaros-brand-kit/DESIGN.md). The detailed crow remains brand artwork, never a functional assistant or notification icon.

## Do's and Don'ts

### Do:

- Do preserve the approved Nullkeel colors, supplied Rookslash masters and established font families.
- Do pair filled accents with on-accent text and use each palette's dedicated focus color on pale surfaces.
- Do accompany selected and enabled states with visible labels, border treatment and switch position.
- Do keep the native footer reachable as content scrolls at the minimum window size.
- Do describe appearance as an app preview and desktop motion as an apply-on-Save preference.
- Do preserve the existing brand-viewer and blueprint design records within their own surfaces.

### Don't:

- Don't replace the approved identity or redraw the wordmark from ordinary text.
- Don't use Steel for normal body copy or turn the accent into a large reading surface.
- Don't let focus disappear into a selected fill or border.
- Don't claim that neutral Studio chrome calibrates a display or themes third-party applications.
- Don't inherit concept-only controls, artwork treatments or web breakpoints as implemented native behavior.
- Don't treat component-preview HTML as evidence of native accessibility, final runtime validation or a web platform.
