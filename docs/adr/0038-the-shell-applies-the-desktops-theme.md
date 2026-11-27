# The shell applies the desktop's theme

**Status:** proposed on November 27, 2026, for issue #368.

## The decision

Theme Studio runs as the person and manages theme bundles through
`crowbar-theme`. The shell offers a quick theme switcher. Base's launcher
service reads the theme catalog and applies selections as the person; its
requests accept validated theme IDs and background filenames, never paths.

Fulcrum accepts `set_theme` only on its shell control channel. The message
sets three opaque RGB colors: `focused`, `unfocused`, and `background`.
All three must be valid before any changes. Frame geometry and the locked
background stay unchanged. The ordinary app socket and unlock authority do
not gain this operation.

The shell receives semantic colors and theme names from the launcher. Theme
backgrounds use the existing bounded `wallpaper.rgba` transfer, decoded and
normalized as the person. The shell never opens a theme's image path. This
preserves the accounts and socket authority in ADR 0032.

## Alternatives and consequences

Letting each app change compositor frames would let it impersonate desktop
state, so the shell retains that authority. Giving the shell access to the
person's home would widen its filesystem access unnecessarily. Keeping
image decoding on the person's side reuses the Viewer wallpaper boundary.

Theme changes update native apps, shell colors, compositor frames, and
configuration files for foot and GTK. Programs that read configuration only
at startup pick up those generated files when they next start.
