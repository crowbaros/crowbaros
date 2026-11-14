# GPUI layer-shell backport

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
committed patch with zero fuzz. Cargo.lock records the additional protocol crate.
The generated GPUI source is included in the vendored RPM source archive. This is
a private application dependency; Studio Settings and system packages are unchanged.

This backport is not claimed as a newly accepted upstream patch. The first shell
validates one output and explicitly defers multi-monitor output selection/hotplug.
Reassess the backport when updating GPUI rather than accumulating unrelated fixes.
