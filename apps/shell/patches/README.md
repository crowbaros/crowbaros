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
The generated GPUI source is an ignored build input recreated by that script.
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

Both apps build with these patches: Studio Settings takes the shell's
prepared source through its `[patch.crates-io]`.
