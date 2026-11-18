# Archive: the Fedora blueprint

**Superseded. Not maintained. Do not build on it.**

This was the first plan for CrowbarOS: a curated desktop on Fedora (RPM/DNF)
with a Hyprland fork, five default apps, and no AI. In November 2026 Jim Martin
and Jason Alexander replaced it with the [vision](../substrate-vision/VISION.md). Where the two
disagree, the vision is right.

| File | What it is |
| --- | --- |
| [`community_foss_distro_plan.md`](community_foss_distro_plan.md) | The plan |
| [`crowbaros.html`](crowbaros.html) | The plan as a self-contained page. Open it in a browser |
| [`crowbaros-page/`](crowbaros-page/) | The source, design record, and checks for that page |

## What is still current, and where it lives now

| Subject | Current source |
| --- | --- |
| Name, crow, palette, typography | The [brand kit](../../branding/) and [`DESIGN.md`](../../../DESIGN.md) |
| Native interface design | [`DESIGN.md`](../../../DESIGN.md), [`PRODUCT.md`](../../../PRODUCT.md), and [`apps/`](../../../apps/README.md) |
| Stack, security, software model, roadmap | The [vision](../substrate-vision/VISION.md) |

Two parts of the plan have no current replacement yet: the **governance charter**
(section 5) and the **desktop UX specification** (section 3). Treat them as
proposals until they are restated against the vision.

## Notes

- Links in the plan were updated for this location. The copy embedded in
  `crowbaros.html` is the original text.
- The page cannot be rebuilt from here. `build.mjs` and `verify.cjs` expect the
  old paths under `docs/`.
- The page's review evidence is still in `.impeccable/review/crowbaros/`.
