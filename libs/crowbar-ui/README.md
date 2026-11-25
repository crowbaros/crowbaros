# crowbar-ui

Shared GPUI 0.2.2 UI library for native CrowbarOS apps following `DESIGN.md`.

## What it provides

- **Bundled typography** (`fonts/` + `load_fonts`): Static `Manrope` (Regular, SemiBold, Bold) and `IBM Plex Mono` (Regular) with OFL notices, plus `DESIGN.md` type scale constants (`FONT_UI`, `FONT_MONO`, `TEXT_TITLE`, `TEXT_SECTION`, `TEXT_BODY`, `TEXT_LABEL`, `TEXT_SUPPORTING`, `TEXT_TECHNICAL`, `TEXT_SELECTION_LABEL`).
- **Live theming** (`src/theme.rs`): `UiPalette` (derived from `crowbar-theme` `ActiveTheme` with Studio Mode override, `on_danger`, `selection_bg`, and `signal_fg`) and `ThemeWatcher` / `spawn_theme_watcher` so open native apps update immediately when appearance changes without redrawing while idle.
- **`DESIGN.md` primitives** (`src/components.rs`): `titlebar`, `sidebar`, `button` (`Primary`, `Secondary`, `Ghost`, `Danger` with dashed focus rings), `action_chip`, `card`, `section_header`, `badge`, `swatch_strip`, `divider`, and `status_footer`.

## Test

```sh
python3 ../../apps/shell/packaging/prepare-gpui.py
cargo test --locked
```
