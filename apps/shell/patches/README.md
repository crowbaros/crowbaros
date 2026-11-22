# GPUI patches

## The layer-shell backport

The published GPUI 0.2.2 crate does not expose Wayland layer-shell windows.
This shell applies the upstream implementation from commit
[`b92664c52d7c49bb49acd06d2d40eb23b4e8c601`](https://github.com/zed-industries/zed/commit/b92664c52d7c49bb49acd06d2d40eb23b4e8c601)
(PR #35610) to the verified 0.2.2 crate. Apache-2.0 notices are retained.

Adaptations: normalized crates.io Cargo dependencies, the missing 0.2.2 initial
title assignment, and a minimum 1×1 initial Vulkan allocation while a stretched
layer surface awaits its compositor-provided size. A fourth adaptation preserves
zero-sized (stretched) axes when acknowledging configure/resize: feeding the
configured pixel width back as a fixed size cropped the bar after an output mode
change in our VM check. Protocol size requests still
use zero when asking the compositor to stretch a surface.

`packaging/prepare-gpui.py` checks the original archive SHA-256 and applies the
committed patches in order with zero fuzz. Cargo.lock records the additional protocol crate.
The generated GPUI source is an ignored build input made by that script. It
leaves a source it made before from the same archive and patches as it is, so
Cargo does not rebuild GPUI, and makes a source that differs, as one edited by
hand, again.
The former RPM source recipe is archived. This is a private dependency of the
shell and Studio Settings; system packages are unchanged.

This backport is not claimed as a newly accepted upstream patch. The first shell
validates one output and explicitly defers multi-monitor output selection/hotplug.
Reassess the backport when updating GPUI rather than accumulating unrelated fixes.

## A still window commits nothing

`gpui-0.2.2-still-frames.patch`, applied after the backport, is ours. GPUI
0.2.2's Wayland backend asked for the next frame callback and committed on
every one, even when nothing had changed, so each of the shell's layers and
Studio Settings committed about 60 times a second on a still desktop (#269).
With the patch, a frame that shows nothing new, and after which no frame is
wanted, commits nothing, and the window's frames stop. When GPUI's effects
leave a window dirty, waiting to present, or holding a next-frame callback,
the window asks for a frame callback and commits once, and its frames start
again. Other platforms keep GPUI's behaviour: the patch adds two
`PlatformWindow` methods whose defaults do what GPUI did.

## A destroyed window draws nothing

`gpui-0.2.2-closed-windows.patch`, applied third, is ours. When a Wayland
window is dropped, GPUI 0.2.2 destroys its Vulkan surface at once but takes
the window out of the client's list only in a task that runs later. Events
already queued for the surface still reach it in between. When the last
display goes away, Fulcrum closes the shell's layers and then tells each its
new preferred scale, and GPUI resized the swapchain of a surface it had
destroyed. On NVIDIA's driver that failed with `ERROR_EXTENSION_NOT_PRESENT`,
and blade's `unwrap` panicked (#271). With the patch, a renderer that has been
destroyed ignores a resize, a change of transparency, and a draw. The fault is
GPUI's, so the fix is in GPUI: the shell cannot keep an event from reaching a
window GPUI has dropped.

## GPUI builds without warnings

`gpui-0.2.2-builds-without-warnings.patch`, applied last, is ours. Each build
of GPUI 0.2.2 for Wayland printed three warnings and a fourth about a crate it
depends on. Two of them name code a later Rust will refuse to build. The patch
changes nothing GPUI does:

- `src/taffy.rs` writes two float literals as `f32`. Rust chose `f32` for
  them by a fallback it is phasing out
  ([rust-lang/rust#154024](https://github.com/rust-lang/rust/issues/154024)).
- `Cargo.toml` asks for `stacksafe` 1.0.2 or later, where GPUI asked for 0.1.
  The macro of 0.1.4, 1.0.0, and 1.0.1 is built on `proc-macro-error2`
  2.0.1, the newest there is, which re-exports a private crate
  ([rust-lang/rust#127909](https://github.com/rust-lang/rust/issues/127909)).
  From 1.0.2 the macro does without it. `StackSafe` and `#[stacksafe]`, all
  GPUI uses, are the same in both. Each app's `Cargo.lock` records 1.0.3.
- `PlatformInputHandler::selected_text_range` in `src/platform.rs` is marked
  as unused on a build without X11. Only the X11, macOS, and Windows backends
  call it.

The shell's `tests/gpui_source.rs` fails when the prepared source or either
lockfile loses one of the three.

The gpui-probe builds the published crate with no patches, so its build still
takes `proc-macro-error2` and prints that warning.

All four patches serve both apps: Studio Settings takes the shell's prepared
source through its `[patch.crates-io]`.
