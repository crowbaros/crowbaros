# CrowbarOS

<!-- impeccable:product-schema 1 -->

## Platform

Native Linux desktop, Rust/GPUI on Wayland. The current Impeccable platform enum
does not represent Linux; this is neither a web app nor an iOS/Android app.

## Users and purpose

CrowbarOS is a community-built desktop for curious, technically inclined users,
including people moving from Windows or macOS who want familiar floating windows
and mouse controls. Studio Settings is the first-party place to configure it.

## Stack and operating context

User-confirmed foundations: conventional Fedora RPM/DNF, a maintained Hyprland
fork, Rust for owned code (upstream code and patches may retain their languages),
and GPUI for owned native interfaces. Build inside the Fedora builder, distribute
signed RPMs, and validate in the separate Fedora test VM. Keep the active VM
visible alongside Codex. Physical hardware testing is deferred.

## Brand commitments

Preserve the approved CrowbarOS identity, Nullkeel palette, Rookslash crow,
Manrope interface typography, IBM Plex Mono for technical text, and the existing
Studio Settings visual studies. The canonical plan and brand kit own those
decisions. Do not reopen the identity for an implementation slice.

## Capabilities and constraints

The user confirmed Appearance plus reduced motion for the first Studio Settings
slice. Implement real preferences and truthful apply/error states. The full shell,
third-party theme integration, accessibility, and physical-device coverage remain
separate acceptance work. A neutral Studio interface is not display calibration.

## Evidence on hand

- `docs/community_foss_distro_plan.md`: product decisions and release gates.
- `docs/branding/crowbaros-brand-kit/`: approved assets, tokens and visual studies.
- `apps/gpui-probe/`: pinned GPUI 0.2.2 API and validated rendering/input baseline.
- `docs/development/`: VM, RPM and Intel hardware Vulkan evidence.

## Product principles

- Make settings discoverable with familiar controls and clear scope.
- Preview safely, save deliberately, and preserve user choices.
- Keep native interfaces unprivileged and system integration separate.
- Report tested behavior and remaining gaps accurately.
