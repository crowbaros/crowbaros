# CrowbarOS

> **New direction: agent-first CrowbarOS.** We are pivoting to an agent-first OS
> on Void Linux, where each person shapes a tiny base into their own OS by talking
> to an agent, and every change is a bootable, reversible generation. Start with
> the [vision](docs/agent-first/VISION.md). Then read the
> [design notes](docs/agent-first/design/), the
> [Milestone 1 record](docs/agent-first/milestone-1.md), and the Rust
> [apply engine workspace](agent-first/). The Fedora plan below is kept for reference.

A Fedora-based desktop using Hyprland, Rust, and GPUI.

- [Operating system plan](docs/community_foss_distro_plan.md)
- [Self-contained visual blueprint](docs/crowbaros.html)
- [Local Fedora VM setup and development workflow](docs/development/local-vms.md)
- [Initial VM validation results](docs/development/vm-validation-2026-11-14.md)
- [Native GPUI app and build instructions](apps/gpui-probe/README.md)
- [GPUI RPM and desktop validation results](docs/development/gpui-validation-2026-11-14.md)
- [Hardware Vulkan setup and recovery](docs/development/gpu-vulkan.md)
- [Intel GPU acceleration validation results](docs/development/gpu-validation-2026-11-14.md)
- [VM configuration and provisioning recipes](infra/vm/)

The repository currently contains the plan, brand references, and a working local
build/test lab, and a validated native GPUI application packaged as a signed RPM.
The test VM also runs hardware Vulkan through an experimental Intel VirtIO profile.
The CrowbarOS compositor fork and full GPUI desktop remain to be built.
