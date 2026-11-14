# Bundled fonts

The original Manrope variable font and IBM Plex Mono Regular come from the
CrowbarOS brand kit. Their OFL notices are retained alongside them.

GPUI 0.2.2's Linux text stack treated this variable font as its default ExtraLight
face in the first probe. The app therefore embeds separate Manrope Regular (400),
SemiBold (600), and Bold (700) faces. These are static instances of that same
source, generated in the Fedora builder with FontTools 4.68.0:

```python
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

for weight, style in [(400, "Regular"), (600, "SemiBold"), (700, "Bold")]:
    font = TTFont("Manrope-Variable.ttf")
    static = instantiateVariableFont(
        font, {"wght": weight}, inplace=False, updateFontNames=True
    )
    static.save(f"Manrope-{style}.ttf")
```

Run from this directory in the builder after `sudo dnf install python3-fonttools`
if regeneration is needed. Normal RPM builds use the checked-in static faces and
do not need FontTools. `BUILD-MANIFEST.txt` records the font digests.
