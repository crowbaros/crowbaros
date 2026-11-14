# Hardware Vulkan in the Fedora test VM

The Intel GPU in this workstation can render Vulkan for the test guest through
**VirtIO DRM native contexts**. The [validation record](gpu-validation-2026-11-14.md)
includes a rendered Vulkan cube, host GPU counters, the GPUI interaction checks,
and a cold boot. Keep the test viewer open while running these checks.

This is an experimental, host-specific lab profile. It uses Intel's `i915` driver;
it does not pass through or detach a PCI GPU. Hyprland still uses VirGL for
OpenGL. Vulkan applications opt into a separate Intel Mesa driver inside the VM.

## Why this path

The installed QEMU/VirGL Venus path launches a renderer helper with `fork`/`exec`.
That conflicts with libvirt's QEMU `spawn=deny` sandbox on this host. Explicitly
selecting Intel's host Vulkan ICD did not fix that initialization failure.
The native-context backend works with the existing sandbox enabled.

Fedora's installed Mesa 26.2.6 package does not include the experimental Intel
VirtIO backend. The lab builds the same upstream release with
`-Dintel-virtio-experimental=true`, packages it as
`crowbaros-mesa-intel-virtio`, and installs it under
`/opt/crowbaros-lab/mesa-intel-virtio`. No Mesa source patch is required.
Fedora's Mesa packages and global Vulkan driver discovery remain unchanged.

## Prerequisites

Start with the [provisioned VM lab](local-vms.md) and installed
[GPUI probe](../../apps/gpui-probe/README.md). The builder needs the development
signing key created by the smoke/GPUI build workflow.

The validated host uses an Intel Raptor Lake-S integrated GPU with `i915`, kernel
7.2.15, QEMU 11.1.2, and virglrenderer 1.3.0 built with `i915-experimental` DRM
renderer support. The guest uses kernel 7.2.11. A sufficiently recent version
number alone does not establish that the backend was enabled when packaged.
Do not apply this profile unchanged to NVIDIA, AMD, or Intel `xe` hosts.

QEMU documents minimum Intel native-context versions of host Linux 6.13 and
virglrenderer 1.3.0, and guest Linux 6.14 and Mesa 26.1. This recipe validates the
specific newer combination above, not every combination satisfying those minima.
See [QEMU's native-context documentation](https://www.qemu.org/docs/master/system/devices/virtio/virtio-gpu.html).

## Build and install the driver

Run from the project root. Checkpoint names must be new; the existing `before-gpu`
checkpoint already protects this workstation's pre-acceleration state.

```sh
rtk python3 infra/vm/lab.py stop test
rtk python3 infra/vm/lab.py snapshot test before-gpu
rtk python3 infra/vm/lab.py start build
rtk python3 infra/vm/lab.py wait build
rtk python3 infra/vm/lab.py view build &
rtk python3 infra/vm/lab.py gpu-build build
rtk python3 infra/vm/lab.py stop build
```

`gpu-build` checks the pinned upstream archive SHA-256, builds an SRPM and binary
RPM in clean Fedora 45 Mock buildroots, signs the RPM and repository metadata,
and retrieves `.local/vms/gpu-repo.tar.gz`. Source, SRPM, binary, and build logs
remain under `~/artifacts/gpu/` in the builder. The RPM spec is
[`infra/vm/gpu/crowbaros-mesa-intel-virtio.spec`](../../infra/vm/gpu/crowbaros-mesa-intel-virtio.spec).
Build dependencies are fetched from live Fedora repositories; this is a
repeatable recipe, not a byte-identical release build.

While the test VM is stopped, merge the acceleration profile into the local
overrides. This preserves any other overrides already present:

```sh
rtk python3 - <<'PY'
import json
from pathlib import Path
p = Path('.local/vms/config.local.json')
c = json.loads(p.read_text()) if p.exists() else {}
c.update(venus_enabled=False, drm_native_context_enabled=True,
         gpui_vulkan_driver='intel-virtio')
p.write_text(json.dumps(c, indent=2) + '\n')
PY
rtk python3 infra/vm/lab.py configure test
rtk python3 infra/vm/lab.py start test
rtk python3 infra/vm/lab.py wait test
rtk python3 infra/vm/lab.py view test &
rtk python3 infra/vm/lab.py gpu-deploy test
```

The VM configuration enables blob resources, a 1 GiB host-visible address window,
and `drm_native_context`. That address window is not an additional 1 GiB of
dedicated video memory. Venus stays disabled. The tracked base profile remains
VirGL/software Vulkan so a fresh lab does not depend on this experimental driver.
The local overrides above are already active on the validated workstation.

`gpu-deploy` verifies signatures and uses DNF with both `gpgcheck=1` and
`repo_gpgcheck=1`. Its guest-local repository is disabled outside explicit deploy
commands. It does not install compilers into the test guest.

## Run and verify

Force the private ICD when testing. A plain `vulkaninfo` still follows Fedora's
system driver discovery and can report software Vulkan; that is intentional.

```sh
rtk python3 infra/vm/lab.py ssh test sudo -u desktop env \
  XDG_RUNTIME_DIR=/run/user/1001 WAYLAND_DISPLAY=wayland-1 \
  VK_DRIVER_FILES=/opt/crowbaros-lab/mesa-intel-virtio/share/vulkan/icd.d/intel_icd.x86_64.json \
  timeout 20 vulkaninfo --summary

rtk python3 infra/vm/lab.py ssh test sudo -u desktop env \
  XDG_RUNTIME_DIR=/run/user/1001 WAYLAND_DISPLAY=wayland-1 \
  VK_DRIVER_FILES=/opt/crowbaros-lab/mesa-intel-virtio/share/vulkan/icd.d/intel_icd.x86_64.json \
  timeout 45 vkcube --wsi wayland --c 1800

rtk python3 infra/vm/check_gpui.py --vulkan-driver intel-virtio \
  --output .local/gpu/gpui-validation
rtk python3 infra/vm/lab.py gpui-run test intel-virtio
```

These direct commands assume the lab's UID 1001 and `wayland-1` socket; `gpui-run`
discovers the current socket. The input checker expects the lab's single
1280×800 scale-1 display. Avoid manual input while it runs.

Acceptance requires an **integrated Intel GPU**, successful visible rendering,
and passing input/redraw checks. A CPU device or llvmpipe is a failed hardware
check. Selecting only the private ICD excludes software fallback. The GPUI
checker additionally asserts that the process loaded the private Intel library
and did not load lavapipe. Inspect its screenshots as well as its exit status.

`gpui-run test` uses `gpui_vulkan_driver` from the effective lab configuration.
It accepts `intel-virtio`, `software`, or `system` explicitly. A running probe
keeps its original driver: quit it with **Ctrl+Q** before switching, or use the
input checker, which restarts it. The package's ordinary desktop launcher uses
system discovery; the hardware override belongs to the lab launch command.

Repeat a graceful stop/start, reopen the viewer, then repeat `vulkaninfo` and
`gpui-run` to check cold-boot behavior. The validated `gpu-validated` offline
checkpoint contains the installed hardware driver and GPUI package.

## Fall back or recover

For a per-app fallback, quit the probe and run:

```sh
rtk python3 infra/vm/lab.py gpui-run test software
```

To return the VM to the original graphics profile, stop it, merge these three
values into `.local/vms/config.local.json`, then run `configure test` and start it:

```json
{
  "venus_enabled": false,
  "drm_native_context_enabled": false,
  "gpui_vulkan_driver": "system"
}
```

The private RPM may remain installed without affecting system drivers. Restore
`before-gpu` while stopped if the pre-driver disk state is also needed.
**Checkpoint restore copies disk/NVRAM, not the effective host configuration:**
set the desired profile and run `configure test` separately. Restoring an older
disk without the private RPM while retaining `intel-virtio` as the launch default
will correctly fail its missing-ICD check.

## Limits and references

This profile establishes hardware Vulkan for the cube and GPUI probe on one
Intel host. It does not establish performance targets, power use, mixed-DPI or
multi-monitor behavior, a passing Vulkan conformance suite, or physical hardware
support. Ray tracing is disabled in this lab build. Keep physical-device testing
as a separate milestone.

- [QEMU VirtIO GPU backends](https://www.qemu.org/docs/master/system/devices/virtio/virtio-gpu.html)
- [QEMU Venus/render-server sandbox issue](https://gitlab.com/qemu-project/qemu/-/issues/3156)
- [Mesa 26.2.6 release and source checksum](https://docs.mesa3d.org/relnotes/26.2.6.html)
- [Mesa 26.2.6 build options](https://gitlab.freedesktop.org/mesa/mesa/-/blob/mesa-26.2.6/meson.options)
