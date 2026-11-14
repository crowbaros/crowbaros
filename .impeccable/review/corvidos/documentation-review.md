# Documentation disposition: complete

This is an established-brand surface extension, documented after the current [ship review](finish-review.md). No new identity authorization, replacement brand system, UI edits or additional browser testing were needed. Only `docs/corvidos-page/DESIGN.md`, its local `.impeccable/design.json` and this evidence record were added. The original brand-kit PRODUCT.md, DESIGN.md, sidecar and artwork remain untouched.

## Incumbent comparison

| Established authority | Finished page evidence | Scoped documentation |
| --- | --- | --- |
| Nullkeel's five exact colors; Manrope and IBM Plex Mono | `page.css` root palette and font faces; 1280px and chapter captures | Inherited without change; page-specific line/section colors and larger reading hierarchy remain local. |
| Supplied outlined logo and complete natural right-facing crow | `user-1280.png`, `mobile-overview.png` | Asset geometry and orientation preserved; transparent WebP is a compression derivative. |
| Quiet opaque surfaces and restrained cyan selection | `chapter-desktop.png`, `chapter-software.png` | Flat editorial layout, real controls, ruled disclosures and limited overlay shadows recorded. |
| Complete native desktop studies with external captions | `chapter-desktop.png`; all-eight-scenes check | Original 1600 × 900 studies retained, including Light, Warm Paper and neutral Studio. No mobile OS reinterpretation. |
| Read-mode direction contract and compact one-file correction | Source, build policy, provenance, `checks.json` | Nine chapters, exact source, eight scenes, six wallpapers, inline resources; no embedded ZIP/PDF. |

The documenter opened the four screenshots named above and inspected the CSS, interaction code, direction contract, brand authorities, README, source-coverage record, raster-provenance record and reviewer evidence. Broader screenshot and browser coverage is supplied by the finish review and checks, not independently repeated here.

## Recorded validation

- Artifact confirmed at 3,514,870 bytes, SHA-256 `0a19bdea5bc69c1eeb6a98c58d21a4e8079582c3214a67940f630f29075047a2`, matching the ship review.
- Static sRGB contrast calculated from the shipped CSS: Frost/Void 15.18:1; Muted/Void 9.76:1; Muted/Abyss 8.16:1; Muted/Desktop Ground 9.14:1; Void/Ice 11.30:1. This is a pairing check, not a full accessibility audit.
- Existing checks report isolated offline operation, no unexpected requests, missing images or runtime errors, no page overflow at 360/390/1280px, functioning focus/search/navigation/gallery/download behavior, exact source and exact SVG/text downloads.
- Sidecar primitives are self-contained extracts with fallback colors and native semantics. Component behavior requiring page JavaScript is explicitly identified; no new component system is invented.

Documentation does not claim independently tested screen-reader, cross-browser or printed output. No unresolved documentation issues remain within this scoped extension.
