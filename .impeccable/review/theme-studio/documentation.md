# Theme Studio documentation check

Source checked: `3b08b64`, based on main `7a2423f`. This is the documenter pass for issue #368, an ordinary native extension of the established CrowbarOS system. No new visual world or durable system change is established. `DESIGN.md`, `PRODUCT.md`, and `.impeccable/design.json` remain unchanged.

## Evidence checked

- Read the documenter role and `reference/document.md`, the surface contract, `PRODUCT.md`, `DESIGN.md`, the sidecar structure and component inventory, and the current vision's opening product definition.
- Inspected `apps/theme-studio/src/ui.rs`, `apps/shell/src/theme_switcher.rs`, shared font and dimension constants in `libs/crowbar-ui/src/lib.rs`, palette mapping in `theme.rs`, and shared buttons and titlebar in `components.rs`.
- Inspected `libs/crowbar-theme/src/builtin.rs`, `extract.rs`, and generator mappings. Confirmed that the seven built-in themes and shared typography/control constants already existed at `7a2423f`.
- Read `review-packet.md`, the original `finish-review.md`, and `fix-packet.md`. Independently opened `before-desktop.png`, `studio-final-extracted.png`, `studio-light-preview-final.png`, `fix-close-compact.png`, `fix-image-error-compact.png`, and `fix-switcher-selected-focus.png`.

## Five-line system summary

1. Palette: semantic `UiPalette` roles preserve Nullkeel Dark, Nullkeel Light, Warm Paper and the existing theme catalog; a generated draft is previewed independently of active desktop chrome.
2. Type: Manrope remains the interface face and IBM Plex Mono the hexadecimal face; shared roles remain 28/14/13/12/11/10px, while this surface locally uses 24px and 18px semibold headings, 13px body/control text, 11px library metadata, and a 20px switcher heading.
3. Signal and Choice rules: active choices retain purposeful accent or surface emphasis, with readable semantic text; image-derived colors are editable theme content, not replacement brand tokens.
4. Stable Actions rule: the shared 48px titlebar and a fixed save/recovery footer bound independently scrolling library/editor content, including the 820 × 660 compact capture.
5. Human Type and State Separation rules: shared 40px, 10px-radius buttons preserve native geometry; dashed keyboard focus is distinct from persistent selection, including the switcher's compact 6px-radius rows.

## Incumbent-system comparison

The finished source preserves the native system's opaque canvas and surface planes, outlined controls, proportional interface text, technical hex labels, and immediate state feedback. The image is the actual input to the palette workflow, using existing bundled wallpaper in the supplied captures. It is not a decorative identity replacement. The sample uses the draft colors while the surrounding app follows the active palette; the Light preview within a Dark desktop demonstrates that distinction.

The 250px library, 24px editor padding, 144px source-image preview, 380px shell panel, local heading sizes, and local swatch geometry describe these surfaces. They do not establish new shared tokens or supersede Studio Settings' explicitly surface-scoped measurements. The palette generator's stronger lines derive from its contrast target; this pass neither promotes that result into a universal border treatment nor weakens the generator to match the built-ins.

The correction captures show an explicit Keep editing / Discard & close footer, complete missing-image retry instructions at compact size, and a dashed focused selected switcher row. These fit existing state and recovery patterns. The original review remains historical; this document does not replace the separate final review verdict.

## Drift retained, not canonized

`PRODUCT.md` and `DESIGN.md` still cite the archived substrate vision and describe the shell drawing Tool interfaces and trusted surfaces under that older model. The current `docs/VISION.md` defines the small Void Linux OS and its Agent. This pre-existing product prose is outside the extension's documentation scope and was not repaired.

The root design record and sidecar principally document the earlier Studio Settings slice. Their palette-source wording (`Palette::for_preferences`), three-choice scope, and limited component inventory do not fully describe the already-existing shared theme engine, seven built-ins, semantic status colors, inputs, and chip primitives. That documentation lag predates this extension; adding those concepts to the root system here would exceed the authorized ordinary-extension pass. Local heading sizes and generated palette values are likewise not canonized as shared rules. No craft-floor violation is promoted into a design-system rule.

## Limits

This pass compared source and six supplied native PNGs; it did not run the app, rerun tests, or independently exercise input paths. The correction packet reports its native-window-close callback as code-inspected and the full desktop check as in progress. No successful full-check result or final acceptance verdict is inferred here. HTML/CSS detectors and browser component samples cannot validate these GPUI surfaces and were not used.
