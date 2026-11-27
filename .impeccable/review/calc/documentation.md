# Calculator documentation handoff

Reviewed implementation at `d9eecea` for issue #366. This is an ordinary
extension of the incumbent CrowbarOS native visual system. No new system
decision is recorded: `DESIGN.md`, `.impeccable/design.json`, and `PRODUCT.md`
remain unchanged.

## Sources and evidence

Compared `apps/calc/src/main.rs`, the Launcher in `apps/shell/src/main.rs`,
`apps/shell/src/theme.rs`, and `libs/crowbar-ui/src/{lib,components,theme}.rs`
with the root design record, its sidecar, product record, and
`.impeccable/surfaces/apps-calc-src-main-rs.md`. `docs/VISION.md` governs
current product behavior.

Directly inspected [Dark Standard](dark-standard.png),
[Light keyboard focus](light-keyboard-focus.png), and
[Light launcher result](light-launcher-271.png). The
[finish review](finish-review.md) records the broader mode, compact, empty,
error, and theme evidence. Its one launcher-footer finding is resolved in the
[finish verdict](finish-verdict.md), including ordinary app search and error
recovery. The ship disposition applies to that scored correction. Full
desktop integration validation was still running at this handoff; this
document does not report it as passed.

## Implementation against the incumbent system

| Area | Observed implementation and scope |
| --- | --- |
| Palette | Calculator uses `UiPalette::from_active` and semantic canvas, surface, text, muted, line, accent, focus, and danger roles. Dark and Light captures preserve the incumbent Nullkeel grounds and purposeful Ice controls. Launcher math uses the shell's existing `Palette`, with surface fill and focus border; it does not introduce another palette. |
| Typography | Shared bundled Manrope supplies interface controls; IBM Plex Mono supplies Calculator expressions, results, base values, and history numbers. Calculator uses a 28px numerical result, 13px body/error, 12px supporting/base text, and 11px history context. Launcher uses a 24px mono result, 13px error, and 12px supporting instruction. The larger mono numerals are local numerical hierarchy, not a replacement for the root Manrope title role. |
| Primitives | Calculator calls shared `button` and `titlebar`: 40px buttons, 10px radius, 2px borders, 13px semibold labels, and a 48px titlebar. Existing enabled hover and disabled opacity remain. Local shorter controls serve width selection and Close/Clear; keypad padding adapts to six-column rows. No new shared primitive is introduced. |
| Focus | Shared controls keep a dashed focused border distinct from selected primary fill. The expression input maps ring and caret to the palette's focus role. The Light focus capture shows the dashed keypad boundary while Standard remains selected. Tab navigation and explicit shortcuts are wired in source; screenshots alone do not certify accessibility semantics. |
| Theme following | Calculator loads the active theme and uses `spawn_theme_watcher` to update its palette and GPUI input theme. Shared code supports Warm Paper and Studio neutrality, but this handoff's visual evidence covers Dark and Light only. Launcher follows its existing model palette. |
| Layout and depth | The 900 × 760 initial window and 780 × 620 minimum retain stable titlebar/mode/footer rows, a 250px history column, and separately scrolling calculation/history areas. Eight-pixel key gaps and 16px body padding extend incumbent spacing. Canvas/surface contrast and dividers carry structure. The existing launcher backdrop and input component treatment remain scoped to their own surfaces. No new animation or shadow rule is established. |

## System summary

1. Palette: retain Nullkeel canvas/surface contrast and semantic theme roles.
2. Type: Manrope controls, IBM Plex Mono numbers; local 28px/24px numerical emphasis.
3. Signal Rule: selected modes, selected width, and evaluation use purposeful primary fill.
4. Human Type Rule: human-facing labels and technical values keep their distinct families.
5. State Separation Rule: dashed keyboard focus remains distinct from selection.

## Drift not canonized or repaired

The root product/design prose still describes parts of the substrate vision
and links to its archived plan. Current `docs/VISION.md` takes precedence;
this Calculator extension does not renew that architecture. The root token
inventory and sidecar describe the earlier Studio Settings surface, while
the shared library now also exposes semantic danger/warning/success and
additional primitives. Calculator's danger-colored errors use that existing
shared role, not a newly approved brand color. The shell still has its own
palette mapping rather than the shared watcher path. These are pre-existing
documentation or implementation differences outside this handoff's boundary;
none is promoted into a new system rule or repaired here.
