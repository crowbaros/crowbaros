# Vulkan development evidence

The active target is Void Linux. The previous
[Fedora/Intel VirtIO setup instructions](../archive/fedora-workflow/docs/development/gpu-vulkan.md)
are archived with their package recipe; they are not current deployment steps.

The earlier [GPU validation](gpu-validation-2026-11-14.md) remains unchanged.
Its Intel native-context findings depend on the host GPU and Mesa build, not on
Fedora. Porting and qualifying that accelerated VM path for Void remains work;
the current compositor guest uses software rendering.

The [NVIDIA/DRM record](../research/compositor-nvidia-2026-11-15.md) demonstrates
the compositor and GPUI apps on one bare-metal Void/NVIDIA configuration,
including resize and suspend/resume. It records the exact driver, packages,
kernel/module options, and limitations. It does not qualify every GPU, driver,
explicit-sync client, or VM renderer.

Use the [Void development guide](local-vms.md) for current build/run paths.
Release GPU/display coverage belongs to
[#102](https://github.com/crowbaros/crowbaros/issues/102), with supported scope
selected first.
