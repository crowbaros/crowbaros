# Theme Studio

Theme Studio creates a desktop palette from an image, or applies an installed
theme. The shell's palette button opens the Quick Theme Switcher for everyday
changes and background cycling.

Enter an image path and choose **Extract colors**, then pick a candidate accent
and Dark or Light. The preview shows the generated palette. Enter another
image path and choose **Add background** to attach more wallpapers (up to 16).
Name the theme and choose **Save & apply**. The installed list applies themes;
custom themes offer an inline, two-step Delete action. Tab and Shift+Tab move
through controls; Enter or Space activates buttons; Ctrl+Q closes the app.
Closing an unsaved draft asks Keep editing or Discard & close; Escape keeps
the draft. This also applies to native window-close requests.

PNG, JPEG, WebP, GIF (first frame), BMP, and TIFF are supported. Decoding rejects
special files, files over 64 MiB, and images over 40 megapixels; wallpapers are
normalized to at most 4,194,304 pixels. Transparent pixels do not vote for
colors. Extraction uses deterministic, alpha-weighted dominant colors. Text,
muted text, and accent/status foregrounds reach 4.5:1 on the generated
surfaces, and focus/lines reach 3:1. Existing named palettes keep their colors.

Bundles live in `/usr/share/crowbaros/themes/` or
`$XDG_CONFIG_HOME/crowbaros/themes/<slug>/` (default `~/.config`). Applying
writes `current-theme.json`, compatible `appearance.json`, foot and GTK
colors, and bounded wallpaper pixels. Existing foot/GTK entry configurations
are preserved; new ones include the generated theme files. Apps with existing
custom configurations can include `foot/theme.ini` or import `colors.css`.

## Build and run on Void

```sh
python3 apps/shell/packaging/prepare-gpui.py
cargo test --locked --manifest-path apps/theme-studio/Cargo.toml
base/void_base.py build --lab-account
lab/void/desktop_check.py
lab/void/compositor_check.py
```

Base builds the app in its Void build root and installs
`com.crowbaros.ThemeStudio.desktop`. Launch **Theme Studio** from the desktop
launcher or **Create theme…** in the shell's theme switcher. The graphical
checks record native screenshots and logs. See [the lab guide](../../lab/void/README.md).

## Headless commands

```sh
crowbaros-theme-studio --list-themes
crowbaros-theme-studio --apply-theme nord
crowbaros-theme-studio --apply-theme nord nightwatch.png
crowbaros-theme-studio --cycle-background
crowbaros-theme-studio --extract /path/to/image.png 'Ocean' dark /path/to/second.png
crowbaros-theme-studio --apply-theme ocean
```

`--extract` saves without applying. Every command returns nonzero on failure.
A build with `--no-default-features` supports the same commands without GPUI.
Image decoding and persistence run as the person. The shell receives bounded
pixels and palette metadata through Base's launcher service and never opens
a personal background path ([ADR 0038](../../docs/adr/0038-the-shell-applies-the-desktops-theme.md)).
