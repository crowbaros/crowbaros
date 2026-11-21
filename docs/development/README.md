# Interface development on Void

Use the [Void development guide](local-vms.md) for current build/run commands
and the [desktop check](../../lab/void/README.md#the-desktop-check).
The [Fedora workflow](../archive/fedora-workflow/README.md) is archived.

| Page | Purpose |
| --- | --- |
| [local-vms.md](local-vms.md) | Current Void environments, direct Cargo builds, and remaining integration tasks |
| [gpu-vulkan.md](gpu-vulkan.md) | Graphics evidence and boundaries for Void qualification |
| [Void desktop acceptance](void-desktop-smoke-2026-11-15.md) | 37 passing checks for the desktop and Settings, as they were on that date |
| [Fedora retirement acceptance](fedora-retirement-completion-2026-11-15.md) | Post-retirement desktop/Base checks and preservation audit |
| [Fedora retirement preparation](fedora-retirement-2026-11-15.md) | Earlier archive preservation and build checks, recorded before desktop acceptance |
| [Radeon 610M record](../research/fulcrum-radeon-610m-2026-11-20.md) | The same machine with its integrated Radeon, on Mesa: the desktop flow, the GPU, the cursor plane, the monitor, and suspend |
| [RTX 5060 record](../research/fulcrum-rtx-5060-2026-11-20.md) | Base's image and Fulcrum on real hardware with NVIDIA's driver: the desktop flow, the GPU, the cursor plane, the monitor, and suspend |
| [NVIDIA suspend record](../research/nvidia-suspend-2026-11-21.md) | Base's image keeping NVIDIA's video memory across suspend, on the RTX 5060 machine |
| [NVIDIA/DRM record](../research/compositor-nvidia-2026-11-15.md) | GPUI and compositor on Void hardware, resize, and suspend/resume |
| [shell validation](shell-validation-2026-11-14.md) | Historical shell behavior checks and captures |
| [Settings validation](studio-settings-validation-2026-11-14.md) | Historical persistence, input, and visual checks |
| [GPUI validation](gpui-validation-2026-11-14.md) | Historical toolkit rendering/input evidence |
| [GPU validation](gpu-validation-2026-11-14.md) | Historical Intel VirtIO results |
| [VM validation](vm-validation-2026-11-14.md) | Historical Fedora VM results |

Dated validation pages and their assets stay unchanged. Historical commands in
them are evidence, not supported setup instructions. The archive manifest maps
retired script/recipe paths to their copies. Current Base work uses the
[Void lab](../../lab/void/README.md).
