# Native captures

Current source: `32c1124`. Normal output 1280×960 at 100%; compact output
1280×960 at 200% (640×480 logical). All text is synthetic fixture content.

- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/clipboard-dark-text.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/clipboard-light-text.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/characters-dark-all.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/characters-light-all.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/characters-dark-emoji.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/characters-light-emoji.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/clipboard-light-clear-focus.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/clipboard-light-empty.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/characters-light-empty.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/characters-light-next-page.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/picker-compact-history.png`
- `.lab/clipboard-iterations/20261127T225523-ea5pimjj/picker-compact-characters.png`

Focused native log: `.lab/clipboard-iteration-4.log`. Final results: `.lab/clipboard-iterations/20261127T225523-ea5pimjj/results-not-evidence-3.json`.

## Known issue from the second visual pass

Explicit `Noto Color Emoji` on emoji cells regressed glyph coverage: Thinking
face is now tofu, and the first faces remain monochrome. The prior default
font fallback rendered Thinking face in color. This needs a fix; include it
in the review findings. The main history text captures use longer synthetic
notes to check clipping. A copied rocket in an older history capture is also
tofu, so check the normal text fallback as well as the glyph cell.

The host/native functional assertions pass, but they check copied Unicode
bytes, not rendered glyph coverage. No claim of a visual pass is made.
