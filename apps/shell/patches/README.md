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

`gpui-0.2.2-builds-without-warnings.patch`, applied fourth, is ours. Each build
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

## Image copies offer image bytes

`gpui-0.2.2-image-clipboard.patch`, applied fifth, is ours. GPUI 0.2.2's
Wayland backend advertised only text MIME types when copying a clipboard
item and sent only its text entries. Viewer could report an image copied,
but another application could neither request nor receive its PNG (#362).

The clipboard now advertises each image's actual MIME type and sends its
encoded bytes when that type is requested. It still offers the three text
aliases for text entries, concatenates text entries as before, and preserves
the process-local MIME marker used to retain clipboard metadata. Mixed items
offer both kinds, without duplicate MIME types. Primary selection keeps its
existing text behavior. No re-encoding or image decoding runs in the clipboard
callback.

The shell's `tests/gpui_clipboard.rs` compiles the patched MIME/payload selector
with GPUI's real clipboard types. It checks exact binary payloads, all image
formats, text aliases and metadata, and mixed entries. `tests/gpui_source.rs`
also checks that the actual Wayland offer and send paths call this selector.
Viewer's desktop check copies a rotated PNG and decodes the received bytes in
another process; unit tests alone do not prove Wayland delivery.

## Pointer axes work without an optional source

`gpui-0.2.2-optional-axis-source.patch`, applied sixth, is ours. Wayland
permits pointer `axis` events without `axis_source` or a discrete wheel event.
GPUI 0.2.2 initialized its source as Wheel and discarded continuous axes for
that source, so Fulcrum's valid Axis + Frame sequence did not scroll a window.
Viewer's distant-thumbnail check exposed the failure (#362).

Every axis now contributes to that frame's continuous delta. At Frame, a
discrete or value120 delta takes precedence when present; otherwise GPUI uses
the continuous pixels. Both are consumed, and only one scroll event is sent.
This preserves wheel line units and fractional high-resolution steps without
doubling their accompanying continuous motion. Touchpad pixel scaling remains
unchanged. Source state is no longer needed and cannot leak between frames.

`tests/gpui_axis.rs` compiles the patched decision with actual GPUI point and
scroll types. It checks bare and touchpad axes, discrete precedence, fractional
steps, and successive frames. `tests/gpui_source.rs` also checks that the
Wayland event handler accumulates bare axes and routes each frame through that
decision. Viewer's desktop check scrolls to a thumbnail outside the initial
page cache to verify delivery in a real window.

All seven patches serve the GPUI apps that take the shell's prepared source
through their `[patch.crates-io]`.


## Emoji families

`gpui-0.2.2-emoji-families.patch` keeps Noto Color Emoji when explicitly
requested. GPUI's Linux font loader rejects faces without a Latin `m` and
removes them from its database. An emoji-only family is valid without that
glyph; removing it also breaks subsequent ordinary text fallback. The patch
uses GPUI's existing known-emoji predicate and keeps the guard for other
fonts. The desktop check opens the Thinking face result and asserts colored
glyph pixels, then copies emoji and displays them again in history. The
pixel assertion fails with zero colored pixels without this patch.
