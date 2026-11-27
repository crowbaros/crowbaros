# crowbar-theme

Pure Rust theming library for CrowbarOS with zero GPUI or Wayland dependencies.

## What it provides

- **Built-in palettes** (`src/builtin.rs`): Nullkeel Dark, Nullkeel Light, Warm Paper, neutral Studio Mode, plus built-in dark palettes (`catppuccin-mocha`, `tokyo-night`, `nord`, `gruvbox-dark`) with exact `DESIGN.md` tokens.
- **WCAG 2.1 contrast helpers** (`src/color.rs`): RGB/HSL conversion, hex formatting/parsing, `relative_luminance`, `contrast_ratio`, and hue-preserving `ensure_contrast`.
- **Theme store** (`src/store.rs`): Built-in, system (`/usr/share/crowbaros/themes`), and user (`$XDG_CONFIG_HOME/crowbaros/themes`) discovery; atomic activation (`current-theme.json` + schema-1 `appearance.json` compatibility); and fast `(mtime, size)` change fingerprinting.
- **Custom wallpaper** (`src/wallpaper.rs`): A versioned, bounded RGBA transfer format shared by Viewer, Base's launcher, and the shell. The separate `wallpaper.rgba` file overrides the built-in wallpaper without changing schema-1 appearance preferences. Removing it restores the built-in selection.
- **Per-app config renderers** (`src/generators.rs`): `render_foot_ini` (`foot/theme.ini`) and `render_gtk_css` (`gtk-3.0/colors.css`, `gtk-4.0/colors.css`).

## Test

```sh
cargo test --locked
```

The optional `images` feature adds bounded raster decoding and theme wallpaper
activation on the person side. `extract` itself accepts RGBA pixels with no
image dependency and derives deterministic, contrast-tested palettes. Run
`cargo test --locked --features images` for both paths.
