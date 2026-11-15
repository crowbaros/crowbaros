# CrowbarOS

> **New direction: agent-first CrowbarOS.** A lean OS on Void Linux where each
> person shapes their own OS with their agent. Their intents, checks, and pinned
> builds are the source, changes to their tools are checked in a fork before they go live, and
> security comes first and does not depend on the model being right. Start
> with the [vision](docs/agent-first/VISION.md), then the [glossary](GLOSSARY.md),
> the [decision records](docs/adr/), the [design notes](docs/agent-first/design/),
> the [Milestone 1 record](docs/agent-first/milestone-1.md), and the Rust
> [apply engine workspace](agent-first/). The desktop design, shell, and Studio
> Settings below carry over as the starting point for the agent-first shell. The
> Fedora packaging and VM recipes are kept for reference.

A Fedora-based desktop using Hyprland, Rust, and GPUI.

- [Operating system plan](docs/community_foss_distro_plan.md)
- [Self-contained visual blueprint](docs/crowbaros.html)
- [Local Fedora VM setup and development workflow](docs/development/local-vms.md)
- [Initial VM validation results](docs/development/vm-validation-2026-11-14.md)
- [Desktop shell source and workflow](apps/shell/README.md)
- [Desktop shell validation results](docs/development/shell-validation-2026-11-14.md)
- [Studio Settings source and workflow](apps/studio-settings/README.md)
- [Studio Settings validation results](docs/development/studio-settings-validation-2026-11-14.md)
- [Native GPUI app and build instructions](apps/gpui-probe/README.md)
- [GPUI RPM and desktop validation results](docs/development/gpui-validation-2026-11-14.md)
- [Hardware Vulkan setup and recovery](docs/development/gpu-vulkan.md)
- [Intel GPU acceleration validation results](docs/development/gpu-validation-2026-11-14.md)
- [VM configuration and provisioning recipes](infra/vm/)

The repository contains the plan, brand references, a working local build/test
lab, and native GPUI applications packaged as signed RPMs. Studio Settings now
implements appearance previews, saved preferences, and Hyprland reduced motion.
The test VM runs hardware Vulkan through an experimental Intel VirtIO profile.
The first GPUI shell now supplies a workspace bar, persistent dock, application
launcher and wallpaper as a signed RPM. It follows saved appearance preferences
and starts with the test Hyprland session. The compositor fork and full production
desktop remain later milestones.
