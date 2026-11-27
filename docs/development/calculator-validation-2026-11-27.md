# Calculator validation — November 27, 2026

Calculator ([issue #366](https://github.com/crowbaros/crowbaros/issues/366))
built in Base's Void build root and passed the full desktop check:
**316 passed, 0 failed; exit status 0**. Calculator and launcher math ran on
Fulcrum in Dark and Light.

The tested runtime implementation was
`d9eeceae81a205442f39dee1d23aeac48894705d`, based on `main` at
`3883ccb0ca29017867fe79e76f311dcd05859e07`. `origin/main` was unchanged when
validation finished. The run recorded a dirty checkout because untracked
visual-review evidence was present; runtime sources were committed. The
subsequent commits contain review evidence and documentation only.

## Checks

| Check | Result |
| --- | --- |
| Calculator and evaluator `cargo fmt --check` | Passed; no formatter ran in `apps/shell` |
| Calculator `cargo clippy --locked --all-targets -- -D warnings` | Passed with the native UI enabled |
| Shared evaluator `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| Locked release builds and tests in Base's Void root | Passed, including Calculator, shell, shared evaluator, and the rest of the desktop |
| Shared evaluator tests | 6 passed: precedence, scientific functions, word operations/base conversions, errors/limits, launcher detection, unary/bitwise precedence |
| Calculator model tests | 2 passed: exact history mode/width restoration and bounded history/error exclusion |
| Python tests under `base/` | 106 passed |
| Python tests under `lab/void/` | 55 passed |
| Python tests under `.github/workflows/` | 23 passed |
| Shell packaging Python tests | 7 passed |
| `python3 lab/void/desktop_check.py` | Full build, 316 passed, 0 failed, exit 0 |

The full check built Base, booted a disposable guest under UEFI, compared
installed binary hashes against the build, checked dynamic libraries, and
exercised the desktop. It stopped the guest and discarded its disk on
success. This was not a `--no-build` run. Earlier focused VM iterations are
visual/behavioral development evidence, not the full-check result.

## Native behavior

In each theme, the check typed `1280 * 1.25`, `sqrt(81) + log(100)`, and
`0xff XOR 0x0f` into Standard, Scientific, and Programmer respectively. Enter
recorded 1600, 11, and 240; Ctrl+C copied each result, read back by a separate
ordinary Wayland client.

Pointer-only sequences computed an 8-bit `NOT 0` (255), `sqrt(9)` (3), and
`2+3` (5). Clicking history restored the expression and mode. Division by
zero remained an editable error, followed by a successful `(2+3)*4` (20).
The launcher evaluated and copied both `1280 * 1.25` and `0xff + 16`.
The existing launcher search checks still opened Studio Settings.

Separate native visual inspection covered empty history, long expressions,
keyboard focus, the maximum 64-bit word, a 780×620 window and scrolling its
keys into view, and a theme change while the same window remained open.
Default-mode captures use a 900×760 window on a 1280×960 guest display.
Scientific functions use radians and binary64 arithmetic; Programmer uses
exact unsigned words with the overflow/shift rules in the app's README.

## Visual review and evidence

The [independent review](../../.impeccable/review/calc/finish-review.md)
identified one material correction: the launcher footer described opening
when Enter would copy math. The
[verdict pass](../../.impeccable/review/calc/finish-verdict.md) scored that
correction resolved in both themes, with application-search and error-state
captures. Its `ship` disposition covers the scored correction. The
[documentation handoff](../../.impeccable/review/calc/documentation.md)
compares the implementation with the incumbent system and preserves its
files. Those handoff records correctly describe integration as pending at
their time; the completed result is recorded here.

Calculator is new, so the before captures show the desktop without its
window. The after captures show a completed calculation and reusable history.

| Appearance | Before | After | Typing recording |
| --- | --- | --- | --- |
| Dark | [Desktop](../../.impeccable/review/calc/validation-dark-before.png) | [Calculator](../../.impeccable/review/calc/validation-dark-after.png) | [MP4](../../.impeccable/review/calc/validation-dark-typing.mp4) |
| Light | [Desktop](../../.impeccable/review/calc/validation-light-before.png) | [Calculator](../../.impeccable/review/calc/validation-light-after.png) | [MP4](../../.impeccable/review/calc/validation-light-typing.mp4) |

Each recording is three seconds at 1280×960, with nine distinct decoded
frames showing the live answer changing during typing. The review directory
also retains each mode, errors, launcher results, focus, and compact views.
All delivered captures were opened and checked for their named state.

Local full-run evidence, outside git:

```text
.lab/void-desktop-checks/20261127T071444-k1agffie/desktop.log
.lab/void-desktop-checks/20261127T071444-k1agffie/results.json
.lab/void-desktop-checks/20261127T071444-k1agffie/build.log
```

No screen-reader, IME composition, physical-hardware, or additional theme
visual validation is claimed. The native app reuses the existing input and
theme infrastructure. The branch remains local; remote PR CI has not run.

Implementation and validation: GPT-6 Astra through the Codex harness in M3 Code.
