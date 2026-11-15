# Interface development: the GPUI lab and its records

These pages cover building, running, and validating the GPUI apps in
[`apps/`](../../apps/README.md). All of it runs in the
[Fedora lab](../../lab/fedora/README.md), which is kept for interface work only
and will be removed when the Void lab has a graphical guest. CrowbarOS itself is
no longer built on Fedora: see the [vision](../VISION.md).

| Page | Contents | Carries over to the Void lab |
| --- | --- | --- |
| [`local-vms.md`](local-vms.md) | Setting up and using the Fedora builder and test VMs | No |
| [`gpu-vulkan.md`](gpu-vulkan.md) | Hardware Vulkan in a VM through Intel VirtIO native contexts | Yes. It depends on the host and the Mesa build, not on Fedora |
| [`shell-validation-2026-11-14.md`](shell-validation-2026-11-14.md) | What was tested for the desktop shell | The behavior checks, yes. The packaging, no |
| [`studio-settings-validation-2026-11-14.md`](studio-settings-validation-2026-11-14.md) | What was tested for Studio Settings | The behavior checks, yes. The packaging, no |
| [`gpui-validation-2026-11-14.md`](gpui-validation-2026-11-14.md) | GPUI rendering and input in the test VM | The rendering and input results, yes |
| [`gpu-validation-2026-11-14.md`](gpu-validation-2026-11-14.md) | Hardware Vulkan results | Yes |
| [`vm-validation-2026-11-14.md`](vm-validation-2026-11-14.md) | The Fedora VMs themselves | No |

The dated pages are records of what was tested on that day. They are not updated.

For Base work, use the [Void lab](../../lab/void/README.md).
