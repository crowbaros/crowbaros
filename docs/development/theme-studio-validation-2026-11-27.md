# Theme Studio validation — November 27, 2026

Theme Studio ([issue #368](https://github.com/crowbaros/crowbaros/issues/368))
built in Base's Void build root. The full desktop check finished with
**335 passed, 0 failed; exit status 0**. The full compositor check finished with
**47 passed, 0 failed; exit status 0**.

The desktop run tested runtime source
`93377585be5fa36d1b0bc29b74dae26fb6653488`, based on `main` at
`7a2423fd69234bf263bcc2fa57232975a643fc21`. `origin/main` was unchanged when
validation finished.
The run recorded a dirty checkout because untracked visual-review evidence
was present; runtime sources were committed.

The compositor run recorded `410724c12656067bc4cdccb51fc6ac5acf4a0223`
with native desktop-test updates still uncommitted. Subsequent implementation
commits changed app draft protection, switcher focus, image-error copy, and
`desktop_check.py`; the compositor implementation, Base integration, and
compositor check remained unchanged. Its full result covers the same
compositor code delivered here. Later evidence commits contain no runtime
changes.

## Checks

| Check | Result |
| --- | --- |
| Compositor locked tests, default and nested features | Passed |
| Compositor fmt and clippy, default and nested, all targets, warnings denied | Passed |
| Base helpers fmt, locked tests, and clippy with warnings denied | Passed; 55 tests |
| Lever fmt, locked tests, and clippy with warnings denied | Passed |
| Theme library locked tests with `images` | 17 passed |
| Theme Studio locked tests, native and headless | 2 passed, including draft revision/close protection |
| Shell locked tests | Passed |
| Locked release builds and tests in Base's Void root | Passed, including Theme Studio and the rest of the desktop |
| Python syntax compilation and Base tests | Passed; 106 tests |
| Lab Python tests | 57 passed |
| CI routing tests | 24 passed |
| Shell packaging tests | 7 passed |
| `python3 lab/void/compositor_check.py` | Full build, 47 passed, 0 failed, exit 0 |
| `python3 lab/void/desktop_check.py` | 335 passed, 0 failed; exit status 0 |

The initial full desktop run at `3b08b64` stopped with 246 passes and one
failure: its fullscreen probe mistook Light foot's background for the Agent
drawer because both correctly used the same theme canvas. The assertion
was retained; the probe terminal now receives a distinct temporary background.
A regression test checks the emitted terminal sequence, its QMP typing, and
separation from every supported drawer canvas. A focused native run confirmed
fullscreen, drawer overlay, and dismissal before the full run was repeated.
The failed run remains in `.lab/void-desktop-checks/20261127T085733-lo8rcp4u/`.

The second run at `25e6a7c` passed the fullscreen check and stopped at 257
passes with one timeout: the empty second display also shared the drawer's
canvas. Its dismissal was visible in the screenshot and logged by the shell.
The multi-display probe now compares a header region with the pre-summon
baseline, proving both that content appears and that it disappears. A
regression test covers equal backgrounds with differing header content.
That run remains in `.lab/void-desktop-checks/20261127T091250-uqzg2d7i/`.

A focused native Agent-thread run passed all 76 recorded checks after the
probe corrections. It was iteration evidence; the completed full desktop
result is the acceptance result.

Both graphical commands built Base and booted disposable UEFI guests. These
were full runs, not `--no-build` iterations. Successful checks stopped their
guests and discarded their disks; the separate native iteration guest was
also stopped.

## Native behavior

In Dark and Light, the desktop check enumerated all seven built-in themes,
applied Nord and both Nullkeel modes, and validated the generated foot
configuration with `foot --check-config`. It extracted an image into a custom
theme with two backgrounds, applied it, then cycled the background while
checking that the palette stayed the same.

The native flow opened the Quick Theme Switcher and used Shift+Tab and Enter
to launch Theme Studio. It typed image paths, extracted colors, added a second
background, selected the requested palette mode, and saved/applied a named
theme. Ctrl+Q on an unsaved draft retained the window; Escape kept editing,
and saving completed with both backgrounds. A clean saved window closed.
The check restores the guest's original theme/configuration after each case.

The compositor test refuses theme changes from an ordinary app, and the
desktop check refuses them from the unlock authority. Compositor unit tests
check focused/unfocused frame and background pixels, strict color fields,
and atomic refusal of invalid requests. Theme tests exercise deterministic
alpha-weighted extraction, generated contrast, bounded decoding, theme and
background membership, traversal/symlink refusal, configuration generation,
and background cycling. Image decoding runs as the person; the shell handles
only palette metadata and the existing bounded wallpaper format.

Separate native review covered errors, compact scrolling, focused selected
and unselected choices, Keep editing, and explicit discard. The native
window-close callback was verified in source; no runtime Fulcrum close-gesture
test is claimed.

## Visual review and evidence

The [initial review](../../.impeccable/review/theme-studio/finish-review.md)
requested explicit concept-scope provenance, draft close protection, distinct
switcher keyboard focus, and actionable missing-image copy. The
[verdict pass](../../.impeccable/review/theme-studio/finish-verdict.md) scored
all four resolved with disposition `ship`, scoped to those corrections.
The [documentation handoff](../../.impeccable/review/theme-studio/documentation.md)
compares the finished native extension with the incumbent system and preserves
its files. Those records correctly describe the full desktop result as pending
at their time; the completed result is recorded here.

The [baseline desktop](../../.impeccable/review/theme-studio/before-desktop.png)
is main before the feature. Captures from the initial full run show the
desktop before each creation sequence, its switcher after custom-theme cycling, and Theme Studio after
native save/apply. All theme-specific checks passed in that run; the later
corrections only change the Agent visibility probes:

| Mode | Before creation | Quick Theme Switcher | Theme Studio saved |
| --- | --- | --- | --- |
| Dark | [Desktop](../../.impeccable/review/theme-studio/themes-dark-before.png) | [Switcher](../../.impeccable/review/theme-studio/themes-dark-switcher.png) | [Studio](../../.impeccable/review/theme-studio/themes-dark-saved.png) |
| Light | [Desktop](../../.impeccable/review/theme-studio/themes-light-before.png) | [Switcher](../../.impeccable/review/theme-studio/themes-light-switcher.png) | [Studio](../../.impeccable/review/theme-studio/themes-light-saved.png) |

The review directory also retains the empty editor, draft Light preview,
compact window, scrolling, recovery, focus, and close-decision captures.
Delivered captures were opened and checked for their named state. Wallpapers
are existing CrowbarOS assets; no new raster artwork or animation was authored.

Local full-run evidence, outside git:

```text
.lab/void-desktop-checks/20261127T092831-kracf5_n/desktop.log
.lab/void-desktop-checks/20261127T092831-kracf5_n/results.json
.lab/void-desktop-checks/20261127T092831-kracf5_n/build.log
.lab/void-compositor-checks/20261127T085135-qzlw62lx/compositor-20261127T155135Z.log
.lab/void-compositor-checks/20261127T085135-qzlw62lx/build.log
```

No screen-reader, IME composition, physical-hardware, or every-built-in-theme
visual validation is claimed. The branch remains local; remote PR CI has not
run.

Implementation and validation: GPT-6 Astra through the Codex harness in M3 Code.
