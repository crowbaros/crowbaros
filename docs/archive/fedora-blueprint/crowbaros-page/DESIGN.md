---
name: CrowbarOS project blueprint
description: A compact offline reference page extending the approved CrowbarOS identity.
colors:
  void: '#080D12'
  abyss: '#17212B'
  ice: '#49D7E8'
  steel: '#6D8191'
  frost: '#D8E5EC'
  muted: '#A8BAC7'
  line: '#30424F'
  recess: '#111B24'
  desktop-ground: '#0D161E'
typography:
  display:
    fontFamily: Manrope, sans-serif
    fontSize: 64px
    fontWeight: 650
    lineHeight: 1.2
    letterSpacing: -0.03em
  headline:
    fontFamily: Manrope, sans-serif
    fontSize: 42px
    fontWeight: 650
    lineHeight: 1.2
    letterSpacing: -0.03em
  title:
    fontFamily: Manrope, sans-serif
    fontSize: 23px
    fontWeight: 650
    lineHeight: 1.2
    letterSpacing: -0.03em
  body:
    fontFamily: Manrope, sans-serif
    fontSize: 16px
    fontWeight: 400
    lineHeight: 1.75
  label:
    fontFamily: Manrope, sans-serif
    fontSize: 14px
    fontWeight: 650
  code:
    fontFamily: IBM Plex Mono, monospace
    fontSize: 0.84em
    fontWeight: 400
rounded:
  compact: 5px
  selection: 8px
  field: 9px
  control: 10px
  panel: 12px
spacing:
  space-8: 8px
  space-12: 12px
  space-16: 16px
  space-24: 24px
  space-32: 32px
  space-40: 40px
  space-64: 64px
  space-96: 96px
components:
  button-primary:
    backgroundColor: '{colors.ice}'
    textColor: '{colors.void}'
    rounded: '{rounded.control}'
    padding: 15px 20px
  button-primary-hover:
    backgroundColor: '{colors.frost}'
    textColor: '{colors.void}'
  chapter-link:
    textColor: '{colors.muted}'
    rounded: '{rounded.selection}'
    padding: 11px 13px
  chapter-link-current:
    backgroundColor: '{colors.abyss}'
    textColor: '{colors.frost}'
    typography: '{typography.label}'
  choice:
    backgroundColor: transparent
    textColor: '{colors.muted}'
    rounded: '{rounded.selection}'
    padding: 10px 15px
  choice-selected:
    backgroundColor: '{colors.ice}'
    textColor: '{colors.void}'
  search-input:
    backgroundColor: transparent
    textColor: '{colors.frost}'
    padding: 12px 0
  disclosure:
    typography: '{typography.label}'
    padding: 20px 0
---

# Design System: CrowbarOS project blueprint

## Overview

**Creative North Star: "Quiet surface. Sharp mind."**

This is a scoped record of the completed [single-file plan page](../crowbaros.html). It inherits the approved identity from [the brand-kit design system](../../../branding/crowbaros-brand-kit/DESIGN.md) and [product commitments](../../../branding/crowbaros-brand-kit/PRODUCT.md). The canonical brand documents remain authoritative for logos, the crow, native desktop studies and appearance variants. This file documents their extension into a reading surface; it does not establish a replacement identity or native OS behavior.

The [surface brief](../../../../.impeccable/crowbaros-brief.txt) owns the atlas composition and Read mode. Summaries lead to inspectable specifications, with complete source preserved in the appendix. Frontmatter records actual reusable values from [page.css](page.css); responsive exceptions and behavior remain in the implementation. The sidecar carries breakpoints, effects and isolated component examples.

**Key Characteristics:**

- Quiet opaque planes, readable support text and a restrained cyan signal.
- Supplied engraved crow and outlined logos, with generous surrounding space.
- Persistent wayfinding, open editorial rows and native disclosures.
- Complete offline content in one compact, portable HTML file.

## Colors

The five Nullkeel colors are inherited unchanged. This surface's utility line and desktop-section ground are local extensions, not changes to the brand palette.

### Primary

**Ice** identifies actions, selected choices, focus, reading progress and search matches. Filled Ice controls use Void text.

### Neutral

**Void** supplies the page and navigation ground; **Abyss** distinguishes selected navigation and overlay surfaces. **Frost** carries headings and primary text; **Muted** carries functional support text. **Steel** is reserved for decorative structure, scrollbar thumbs and the governance graphic. **Line** divides chapters and rows; **Recess** grounds code and subtle hover states. **Desktop Ground** separates the large scene gallery from the reading canvas.

**The Signal Rule.** Preserve the inherited Ice/Void pairing and readable Muted text. Do not substitute Steel for normal secondary text.

Static sRGB checks of the actual pairings give Frost/Void 15.18:1; Muted/Void 9.76:1; Muted/Abyss 8.16:1; Muted/Desktop Ground 9.14:1; Void/Ice 11.30:1. These verify those color pairs, not a complete accessibility certification.

## Typography

Manrope remains the proportional reading and interface family; IBM Plex Mono marks code, paths and numerical technical data. The delivered HTML embeds the original font files and their downloadable notices. Wordmarks use supplied outlined SVGs, never substituted live text.

The main heading, chapter headings and subsection headings use the frontmatter hierarchy. Supporting body copy is commonly 14–15px; lead copy is 21px at 1.5 line-height. General paragraphs stop at 72ch; section introductions at 69ch. The full-reference appendix uses 14px text at 1.8 line-height, with horizontally scrollable code and tables. Governance values use tabular numerals.

The display heading becomes 55px at the 1280px breakpoint, then 43px at 760px and 39px at 380px. At 760px, chapter and subsection headings become 34px and 22px. This hierarchy is page-specific; it does not replace the smaller native desktop-study type scale.

## Layout

The desktop shell uses a fixed 248px chapter rail and 76px toolbar. Main content starts beside and below them, with a maximum width of 1800px. Chapters have 96px vertical and 64px horizontal padding. The opening pairs left-aligned copy with the complete right-facing crow; its foundation row anchors the bottom. The nine chapters vary between two-column explanations, open rows, diagrams, a scene stage and the full reference.

At 1280px and below, the rail narrows to 220px and chapter padding becomes 78px by 40px. At 1050px, the rail becomes a native Contents disclosure in a 70px toolbar; main content uses the full width and chapter padding becomes 76px by 48px. At 760px, chapter padding becomes 64px by 24px, two-column text stacks, controls wrap and the crow enters normal flow. At 380px, horizontal padding becomes 20px and the palette has two columns. The 1750px rule enlarges the desktop hero and crow.

Scene previews preserve their 16:9 desktop canvas with captions outside the image. A phone sees a complete desktop study; it does not imply a mobile OS. Package tables scroll inside their container, retaining readable columns. Print rules remove fixed navigation, use light paper and expand reference disclosures through the print lifecycle. Print layout was authored but not visually validated in the current review.

## Elevation & Depth

The reading surface is flat, using tonal changes and one-pixel rules. Shadows belong to transient overlays: search, the mobile Contents menu, copy feedback and the image dialog. Their exact values are in the sidecar. No glass layer or decorative hover lift is introduced.

Focus uses a two-pixel Ice outline with five-pixel offset; search destinations use a twelve-pixel offset. Chevron rotation takes 0.2s; copy feedback uses 0.16s opacity/translation. Smooth navigation and transitions respect reduced motion. Content is visible without entrance animation.

## Shapes

Soft rectangular controls inherit the brand's restrained geometry. Selection controls and navigation use the selection radius, action buttons use the control radius, and overlays, preview frames and the full-reference container use the panel radius. Open lists and tables remain ruled rows rather than decorative card grids. Small open-stroke SVG icons use round caps and joins; they do not replace supplied identity artwork.

## Components

**Wayfinding and search.** Real chapter anchors preserve URL fragments. The current location combines a filled background, heavier text, a cyan dot and `aria-current`. Search offers keyboard shortcuts, arrow-key selection, Enter to jump, an empty state and Escape dismissal. Jumps open enclosing disclosures and focus their destination. The skip link moves keyboard focus into main content. Mobile Contents preserves the same chapter links.

**Buttons and choices.** Primary links use Ice with Void text, changing to Frost on hover. Scene, appearance and logo choices are buttons with `aria-pressed`; hover adds Abyss and Frost. The architecture stages use the same pressed-state semantics with an underline signal. The page's four appearance choices display existing captures, including neutral Studio; they do not change system preferences.

**Disclosures and data.** Native `details`/`summary` pairs expose technical notes, apps, milestones and the complete reference. Their arrow rotates with the open state. Package filtering retains a status message when no rows match. The wallpaper picker remains a native `select`; the full-size image uses native `dialog`, with Escape support and focus return to its trigger. Clipboard copy has a fallback and readable recovery message.

**Compact offline resources.** The build embeds CSS, JavaScript, fonts, outlined logo SVGs, source Markdown, small text downloads and compressed WebP previews. It excludes the full brand-kit ZIP and PDF by explicit user direction. The reviewed HTML is 3,514,870 bytes (3.35 MiB); verification enforces a size below 5 MiB. Interactive galleries and local downloads require JavaScript, with no server or adjacent files required.

The [visual provenance](visual-provenance.json) maps all 15 raster previews to existing approved masters: one transparent crow, eight desktop captures and six wallpapers. Compression retains original dimensions and alpha; desktop captures use WebP quality 92 and other previews quality 88. Original assets are untouched. [Source coverage](content-coverage.json) records the embedded plan hash and derived rows. The HTML preserves the complete original plan byte-for-byte and renders its full reference; preview downloads are WebP, while SVG/text downloads retain their original bytes. Build and verification instructions belong in [README.md](README.md).

## Do's and Don'ts

### Do:

- Do inherit the approved palette, supplied logo geometry and complete right-facing crow.
- Do keep page layout tokens scoped here and preserve the brand-kit authority.
- Do preserve native controls, keyboard focus, selected-state semantics and readable external image captions.
- Do rebuild from the source files and retain provenance for every shipping preview.
- Do preserve all eight scene variants, six wallpapers and the complete source when reducing file size.

### Don't:

- Don't add the full brand-kit ZIP or PDF back into this compact HTML without a new request.
- Don't introduce network or adjacent-file dependencies into the delivered one-file page.
- Don't present illustrative desktop chrome, proposed architecture or roadmap content as shipped OS functionality.
- Don't edit the generated HTML, redraw the supplied identity or change original artwork to maintain this page.

## Name update — 2026-11-14

The user renamed the project CrowbarOS. This surface keeps its approved palette, type, composition and interactions. The outlined horizontal logo now uses an 850 × 180 master to accommodate the longer wordmark; the stacked master is 680 × 420. Preview assets and technical namespace now use `crowbaros`. Exact historical generation prompts retain their original name for provenance.
