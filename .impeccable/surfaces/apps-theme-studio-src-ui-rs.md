# Theme Studio and Quick Theme Switcher — issue #368

Mode: Operate. Code-led extension of the established CrowbarOS native world.
The issue fixes the purpose: create accessible palettes from images, manage
installed themes and multiple backgrounds, and switch the desktop live.

Theme Studio keeps the installed-theme list beside an image-to-palette
editor. An image preview anchors the work; extracted candidate accents and
Dark/Light controls change a readable sample. Background attachments and a
name finish the draft, with Save & apply always reachable in the footer.
The shell's palette button opens a compact theme list with swatches,
background choices, Next background, and Create theme… .

Inherit UiPalette, shared buttons, Manrope labels, IBM Plex Mono for hex
values, and the desktop's opaque surfaces. No new brand or decorative motion.
Image loading and persistence run outside render. Loading, errors, empty
state, keyboard focus, destructive confirmation, and minimum window size
are part of the surface. The image-to-palette-to-desktop change is the
signature interaction, shown by native captures in Dark and Light.

The current vision and ADRs govern behavior. DESIGN.md and PRODUCT.md retain
older substrate wording; it is pre-existing drift, outside this task.
