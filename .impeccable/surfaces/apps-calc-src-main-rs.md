# Calculator — issue #366

Mode: Operate. Code-led extension of CrowbarOS's existing native world.
The issue specifies the purpose, modes, fonts, shared components, theme
following, history, keyboard operation, and launcher math. These are the
surface's direction; no new identity or durable design-system change.

The first viewport puts an editable expression and live result above familiar
arithmetic keys. A separate, scrollable history tape keeps completed work
visible and reusable. Mode controls stay in the same place; Scientific adds
functions, Programmer adds width controls and four exact representations.
The signature interaction is one expression flowing through typing, live
answer, Enter to history, and click to restore its mode and width. Launcher
math is a single answer row in the existing palette, copied on Enter.

Use shared UiPalette, live ThemeWatcher, and button/titlebar primitives.
Manrope controls; IBM Plex Mono expression, answer, base values, and history.
Errors sit where the answer would be, with an editable expression preserved.
No decorative animation, invented icon system, or new palette.

Verify on Fulcrum in Dark and Light, including keyboard input and copying,
all modes, pointer controls/history, empty/error states, and the compact
window. Native screenshots are the visual evidence; the HTML/CSS detector
has no applicable targets. The vision and current ADRs govern behavior;
PRODUCT.md and DESIGN.md's older substrate wording is pre-existing drift.
