# Local Fedora build and test lab

This is the reproducible development environment for CrowbarOS. Omarchy remains
the workstation. Fedora builds and OS integration run in separate KVM guests.
Physical hardware testing is deferred. These guests are development infrastructure,
not a shipped CrowbarOS desktop or installer.

**Work visibly:** keep the VM being worked on open in Virt Viewer, preferably on
the same desktop workspace as Codex. Open the builder's viewer during builds and
the test viewer during deployment and GUI checks. SSH remains the automation path;
the visible viewer lets the user follow the guest's state and interact with it.

The [November 14 validation record](vm-validation-2026-11-14.md) lists the tested
versions, passed checks, and saved checkpoints. The later
[hardware Vulkan validation](gpu-validation-2026-11-14.md) records the working
Intel acceleration profile and its remaining limits.

## Layout

| Guest | Resources | Role | SSH |
| --- | --- | --- | --- |
| `crowbaros-build` | 8 vCPUs, 12 GiB RAM, 120 GiB virtual disk | Rust/Cargo, C/C++ tools, Mock, RPM signing, repository creation | `127.0.0.1:22220` |
| `crowbaros-test` | 4 vCPUs, 6 GiB RAM, 64 GiB virtual disk | Clean package deployment, Hyprland/Wayland integration, graphics diagnostics | `127.0.0.1:22221` |

Both use **Fedora 45 Cloud Base, conventional RPM/DNF**, x86_64, UEFI, and KVM.
Cloud Base is a minimal Fedora installation with cloud-init provisioning; it is
not Fedora Atomic/CoreOS and does not require a cloud service.

The libvirt connection is **`qemu:///session`**, owned by the current workstation
user. VMs are not configured to autostart. `passt` supplies guest networking with
SSH forwarded only on localhost. SPICE uses private Unix sockets. There is no
LAN-facing SSH listener, system-wide network bridge, shared home directory,
clipboard integration, agent forwarding, or filesystem mount into the guests.

Each guest has a key-only `crowbar` administration account with passwordless sudo
**inside that guest**. The test guest also has an ordinary `desktop` account,
without sudo or SSH access, used by the graphical session. Its first-session
console autologin is a lab convenience, not a CrowbarOS authentication decision.

Repository files:

- `lab/fedora/config.json`: image identity, digest, signing fingerprint, VM resources,
  firmware paths, graphics configuration, and localhost ports.
- `lab/fedora/lab.py`: host-side lifecycle and transfer automation, Python stdlib only.
- `lab/fedora/guest/*.sh`: idempotent Fedora provisioning and packaging recipes.
- `lab/fedora/fixtures/`: the small Rust RPM pipeline probe and COPR public key.
- `lab/fedora/gpu/`: isolated Intel VirtIO Vulkan driver RPM specification.
- `.local/vms/`: ignored local disks, seeds, **private keys**, reports, logs,
  artifacts, and checkpoints. Do not commit or publish this directory.

Python and shell here are build/provisioning automation, like RPM specs and CI
recipes. They are not installed as CrowbarOS native applications or services;
the Rust/GPUI policy for owned OS code remains in force.

## Reproduce on this Omarchy host

Run commands from the repository root. The examples use `rtk` to match this
workspace's instructions; on another workstation without RTK, omit that prefix.

Install host tools through Omarchy's package manager (one-time administrator
authentication):

```sh
rtk omarchy pkg add qemu-desktop qemu-img libvirt virt-install virt-manager virt-viewer passt edk2-ovmf
```

The automation uses user-session libvirt. Do not run the lab commands with sudo
or switch them to `qemu:///system`; that is a different VM inventory. No system
libvirt daemon or libvirt group membership is required for this configuration.
The user must be able to read/write `/dev/kvm` and the selected render device.

```sh
rtk python3 lab/fedora/lab.py doctor
rtk python3 lab/fedora/lab.py download
rtk python3 lab/fedora/lab.py create all

rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py provision build
rtk python3 lab/fedora/lab.py stop build
rtk python3 lab/fedora/lab.py snapshot build provisioned

rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py provision test
rtk python3 lab/fedora/lab.py stop test
rtk python3 lab/fedora/lab.py snapshot test clean
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py view test
```

`wait` checks pinned SSH host keys and cloud-init completion. `provision` installs
packages and records their versions. After test provisioning, restart the VM to
start its greetd/Hyprland session. A terminal opens automatically. **Alt+Enter**
opens another terminal; **Alt+Q** closes the active window; **Alt+V** toggles floating.
- Hold **Alt** and drag with the left mouse button to move a window, or right to resize; window borders also resize.
These are temporary lab bindings chosen to avoid Omarchy's Super bindings.

`create` leaves existing registered guests unchanged. It refuses to overwrite an
unregistered disk. `provision` can be rerun; it deliberately reapplies the managed
files in `/etc/crowbaros-lab/` and `/etc/greetd/config.toml`. Checkpoints require
stopped guests and refuse duplicate names.

On this 32 GiB workstation, stop the test guest during larger builds. VM memory
is a maximum allocation, but heavy simultaneous workloads can still create memory
pressure. The disk sizes are capacities, not an immediate 184 GiB allocation.

### Default window manager and login session

**Hyprland is the test VM's default Wayland compositor and window manager.**
The provisioning recipe sets `graphical.target` as the boot target, enables
`greetd` as `display-manager.service`, and configures both greetd's initial
autologin and its normal login command to start Hyprland with
`/etc/crowbaros-lab/hyprland.lua`. This applies to newly provisioned test VMs as
well as the current lab; the build VM remains a command-line build environment.

A freshly provisioned baseline opens a **foot window inside Hyprland**. After
`shell-deploy` and `shell-run`, the packaged CrowbarOS bar, dock, launcher and
wallpaper start with the test session instead. Alt+Return still opens a terminal.
The local console autologin remains a disposable-lab convenience.

Verify the configured default and live session without restarting it:

```sh
rtk python3 lab/fedora/lab.py ssh test systemctl get-default
rtk python3 lab/fedora/lab.py ssh test systemctl is-enabled greetd
rtk python3 lab/fedora/lab.py ssh test systemctl is-active greetd
rtk python3 lab/fedora/lab.py ssh test readlink -f /etc/systemd/system/display-manager.service
rtk python3 lab/fedora/lab.py ssh test sudo cat /etc/greetd/config.toml
rtk python3 lab/fedora/lab.py ssh test sudo -u desktop env XDG_RUNTIME_DIR=/run/user/1001 hyprctl -i 0 version
rtk python3 lab/fedora/lab.py ssh test sudo -u desktop env XDG_RUNTIME_DIR=/run/user/1001 hyprctl -i 0 configerrors
```

Expected results: `graphical.target`, `enabled`, `active`, the greetd service
path, Hyprland startup commands for both login paths, a Hyprland version, and no
configuration errors. These checks passed on November 14, 2026 with Hyprland
`0.57.2-1.fc45`; no session-default change was needed.

## Everyday controls

```sh
rtk python3 lab/fedora/lab.py status
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py ssh build
rtk python3 lab/fedora/lab.py ssh build rustc --version
rtk python3 lab/fedora/lab.py stop build

rtk python3 lab/fedora/lab.py view test
rtk virt-manager --connect qemu:///session
```

The viewer requires the workstation's graphical session. If Virtual Machine
Manager opens with an empty system connection, select/add **QEMU/KVM user session**
or use the explicit command above. Closing the viewer does not shut down its VM.
The SSH helper also works when the compositor or login screen is broken.

For a serial console, use `lab.py console test`; **Ctrl+]** exits the console.
Guest passwords are locked, so use SSH for administration rather than expecting
a serial password login. All SSH settings are also available in the project-local
`.local/vms/ssh_config`:

```sh
rtk ssh -F .local/vms/ssh_config crowbaros-build
rtk scp -F .local/vms/ssh_config some-file crowbaros-build:src/
rtk rsync -a --exclude=.local --exclude=.git --exclude=target \
  -e 'ssh -F .local/vms/ssh_config' ./path/to/source/ crowbaros-build:src/my-project/
```

Copy source into the build guest and develop with incremental Cargo builds there.
Package integration checkpoints as RPMs. Keep compiler caches in the builder;
do not install compilers into the test guest to make a missing dependency disappear.
The GPUI probe below has its own dependency pins and RPM specification. The
Hyprland fork and remaining production shell features still need their own
build and integration work; the first shell recipe is documented below.

## Verified packaging path

The smoke fixture is a tiny Rust executable with no Cargo dependencies. It tests
the infrastructure independently of the desktop shell:

```sh
rtk python3 lab/fedora/lab.py stop test
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py smoke build
rtk python3 lab/fedora/lab.py stop build

rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py deploy-smoke test
```

The `smoke` command transfers the fixture over SSH, builds its SRPM and binary RPM
in **Mock's Fedora 45 clean buildroots**, signs the RPM, builds repository metadata,
signs `repomd.xml`, and copies an archive to `.local/vms/smoke-repo.tar.gz`.
`deploy-smoke` transfers that repository into the test guest, verifies the package
signature, enables package **and repository-metadata signature checking**, installs
through DNF, and runs the executable as the non-admin desktop user.

The repository is a local `file://` repository inside the guest; no HTTP server
or additional host port is required. It remains disabled by default and is enabled
only for the explicit installation command. The test machine does not need the
builder running once the archive has been copied to the host.

The signing key is generated inside the build VM, stored in
`~/.local/share/crowbaros-dev-signing/`, and clearly identified as **NOT FOR RELEASE**.
It has no passphrase for disposable local automation. Production signing must use
separate protected keys and a release process; never promote this key.

Build logs, SRPMs, and RPMs remain under `~/artifacts/smoke/` in the builder.
The source fixture is not part of the product's five default applications.

## Native GPUI application probe

The [GPUI probe](../../apps/gpui-probe/README.md) exercises the native GUI path
through a separate signed RPM. It pins GPUI 0.2.2 and its Cargo dependency graph,
vendors dependencies for the offline Mock compilation, and includes a desktop
launcher plus bundled CrowbarOS fonts and license notices.
The [GPUI validation record](gpui-validation-2026-11-14.md) records the successful
signed RPM installation/upgrade and actual rendering/input checks on software Vulkan.
The [GPU follow-up](gpu-validation-2026-11-14.md) repeats those checks on Intel
hardware Vulkan. Set up that optional profile using the [GPU workflow](gpu-vulkan.md).

```sh
rtk python3 lab/fedora/lab.py stop test
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py view build &
rtk python3 lab/fedora/lab.py gpui-build build
rtk python3 lab/fedora/lab.py stop build
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py view test &
rtk python3 lab/fedora/lab.py gpui-deploy test
rtk python3 lab/fedora/lab.py gpui-run test
```

`gpui-run` launches the installed binary as the ordinary desktop user in its
existing Wayland session. The GUI never runs as root. See the probe README for
keyboard controls and logs. Keep the `before-gpui` checkpoint if you want to
return to the pre-installation test baseline.

An optional last argument selects `system`, `software`, or `intel-virtio` Vulkan.
Without it, `gpui-run` uses `gpui_vulkan_driver` from the lab configuration.
The validated workstation's local default is `intel-virtio`; a fresh lab defaults
to `system`. The Intel mode requires the separately installed driver RPM.
Quit an existing probe before switching drivers.

Run `rtk python3 lab/fedora/check_gpui.py` with the test viewer open for the input
smoke test. It restarts the probe, exercises the VM's keyboard and pointer,
resizes the window, checks event logs, captures screenshots, and quits the app.
Run `gpui-run test` again afterward to leave it available for exploration.

## Studio Settings: first application slice

The [Studio Settings application](../../apps/studio-settings/README.md) builds on
the GPUI probe with real appearance preferences and a Hyprland motion adapter.
It has its own signed RPM and leaves the probe available as a smaller diagnostic.

The [Studio Settings validation record](studio-settings-validation-2026-11-14.md)
records the installed package, actual guest screenshots, and tested behavior.

```sh
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py view build &
rtk python3 lab/fedora/lab.py studio-build build
rtk python3 lab/fedora/lab.py stop build
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py view test &
rtk python3 lab/fedora/lab.py studio-deploy test
rtk python3 lab/fedora/lab.py studio-run test intel-virtio
```

Save a checkpoint before deployment. Keep the active VM viewer visible; an idle
guest can be stopped to free resources during a large build.
`studio-run` accepts the same Vulkan modes as `gpui-run`. The desktop launcher
uses system discovery; the lab launcher explicitly selects the isolated driver.
The development repository archive is `.local/vms/studio-repo.tar.gz`.

`studio-deploy` loads the RPM's optional login hook from the **lab-owned** Hyprland
configuration. Saved reduced-motion preferences are restored when the compositor
starts; the GUI is not started automatically. Saved appearance also updates the
installed CrowbarOS shell. It does not theme GTK/Qt applications.

With a checkpoint saved and manual interaction paused, run
`rtk python3 lab/fedora/check_studio.py`. This check changes the lab user's settings,
exercises real input, and finishes with Dark appearance, Studio Mode off, and
reduced motion on. It captures evidence under `.local/studio/validation/`.
The app README documents the model, save/error behavior, keyboard controls,
package contents, and recovery.

Keep a SPICE/GL viewer visible when shutting down the test VM. During development,
one shutdown stalled with `no gl-draw-done` messages while its viewer was obscured;
bringing the viewer forward allowed graceful shutdown to complete. If it happens
again, inspect the viewer and guest state before considering a forced power-off.

## Clean tests and recovery

Take a named checkpoint before a package or compositor experiment:

```sh
rtk python3 lab/fedora/lab.py stop test
rtk python3 lab/fedora/lab.py snapshot test before-experiment
rtk python3 lab/fedora/lab.py start test
```

Restore the baseline after testing:

```sh
rtk python3 lab/fedora/lab.py stop test
rtk python3 lab/fedora/lab.py restore test clean
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
```

These are **offline copies of the QCOW2 disk and UEFI variables**, with reflinks
when the host filesystem supports them. The shared Fedora base image remains
read-only. Restore retains the outgoing disk/NVRAM in a timestamped
`before-restore-*` checkpoint; it does not silently delete experimental work.
Checkpoint storage accumulates and should be reviewed periodically.
Restore does not reapply saved domain XML or change local configuration overrides.
When restoring across graphics experiments, select the corresponding profile and
run `configure test` while the guest is stopped; see [GPU recovery](gpu-vulkan.md#fall-back-or-recover).

This is VM-level recovery, not a claim that conventional Fedora provides atomic
OS rollback. The future product's update/recovery design needs separate testing.

If graceful shutdown fails, inspect the guest through SSH or its console. The
helper does not automatically hard-power-off a machine. An intentional
`virsh -c qemu:///session destroy crowbaros-test` is equivalent to pulling its power;
use it only when the guest is stuck and you accept losing unsaved guest work.

## First desktop shell

The [shell source and workflow](../../apps/shell/README.md) provide a Rust/GPUI
workspace bar, persistent dock, searchable launcher and wallpaper. It uses a
pinned, documented GPUI layer-shell backport; Studio Settings stays on stock GPUI.
The [validation record](shell-validation-2026-11-14.md) records package identity,
layer/focus checks, theme behavior and the single-output scope.

```bash
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py view build &
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py shell-build build
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py view test &
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py shell-deploy test
rtk python3 lab/fedora/lab.py shell-run test intel-virtio
```

The signed development repository is `.local/vms/shell-repo.tar.gz`. The build
recipe vendors pinned dependencies, builds offline in clean Mock, runs Rust
catalog tests, and packages licenses. Fedora hardening flags are retained while
the development profile uses optimization level 2, 16 codegen units and no debug
information. This is not a release performance qualification.

`shell-deploy` connects the packaged hook to `/etc/crowbaros-lab/hyprland.lua`,
replaces the baseline terminal autostart, and leaves the Studio Settings hook
intact. `shell-run` selects a service-scoped Vulkan ICD, imports the live Wayland
session, reloads the lab compositor config and starts the user service. The
service and fixed-command control socket run as `desktop`, with no privileges.
Applications launched by the shell get separate systemd user scopes.

Click Applications or press **Alt+Space**; type a name, choose with Up/Down or Tab,
and press Enter. Escape or clicking outside restores desktop focus. The dock
opens or focuses installed Terminal and Settings. Floating windows are the lab
default; Alt+V switches a window to/from tiling. The bar reserves 44 px, and the
persistent dock reserves 96 px. Alt+left-drag moves windows; Alt+right-drag
resizes them, and window edges also resize. Auto-hide and the full product app set remain
future work. `before-shell` is the offline pre-install checkpoint.

## Graphics and desktop boundary

The test VM uses **VirtIO with VirGL OpenGL acceleration**, backed by this host's
Intel render device. **Hardware Vulkan now works through Intel VirtIO DRM native
contexts**, using an isolated Mesa driver built and signed in the builder.
The cube, host GPU counters, GPUI interaction checks, and cold boot passed;
see the [GPU validation record](gpu-validation-2026-11-14.md).

The [acceleration workflow](gpu-vulkan.md) enables this experimental profile
through local configuration and explicit per-app ICD selection. Fedora's Mesa
packages and software Vulkan remain available. Venus stays disabled because its
renderer-helper launch conflicts with the installed QEMU sandbox. The working
native-context profile retains that sandbox and guest SELinux enforcement.
Successful rendering and input do not establish performance targets or hardware
coverage beyond this tested Intel host.

Hyprland 0.57.2 is a temporary upstream baseline from the `lionheartp/Hyprland`
COPR recommended by Hyprland's installation guide. It is not an official Fedora
package or the CrowbarOS fork. The repository is disabled by default, limited to
`hypr*`, `aquamarine*`, and `xdg-desktop-portal-hyprland*`, and uses package signature
checks with a pinned public-key digest. COPR does not provide signed repository
metadata here; that limitation is explicit in the recipe. Only the test guest uses
this repository. Replace it with our maintained, signed packages when available.

The lab intentionally uses a minimal upstream test session. CrowbarOS's floating
complete window behavior, remaining shell features, custom greeter, accessibility, and full app integration
remain implementation work. GPU performance, color accuracy, suspend/resume, and
physical input/display behavior remain deferred hardware tests.

## Reproducibility and portability

The Fedora 45 compose **45-1.4** and its image SHA-256 are pinned. The script checks
the checksum manifest's OpenPGP signature against Fedora 45's published fingerprint
before using the image. Image signatures and RPM signatures remain enabled.

Fedora and COPR package repositories are live. Reprovisioning later can install
newer packages, even though the base image is identical. Therefore this is a
repeatable environment recipe, **not a byte-for-byte reproducible OS release**.
Installed package manifests, repository lists, and inactive domain XML are saved
under `.local/vms/{build,test}/reports/`. Keep an offline checkpoint to reproduce
the exact local installed state. Future release builds need retained RPMs, pinned
dependency/source manifests, and controlled repository snapshots.

`config.json` records this Intel/NVIDIA Omarchy host's firmware and Intel graphics
paths. For another machine, create ignored `.local/vms/config.local.json` with
overrides such as `render_node`, `vulkan_icd`, `firmware_code`, and `firmware_vars`.
If overriding `guests`, supply the complete guest mapping. Run `doctor` first.
While a guest is stopped, `lab.py configure test` regenerates its definition from
the effective configuration. Keep resource/graphics changes in these files, rather
than only in VM Manager, so they can be recreated.

The QCOW2 overlays reference the base image by absolute path. Keep `.local/vms/`
at its current location, or deliberately rebase the disks and update definitions
when moving it. To back up working VMs, shut them down and preserve the complete
state directory, including the base image, credentials, NVRAM, and checkpoints.
Treat that backup as sensitive. A clean machine can instead rebuild from the
repository and will generate fresh credentials and signing keys.

## Troubleshooting

- **Sandbox cannot see KVM or libvirt:** execute lifecycle commands on the real
  host. The Codex filesystem sandbox hides devices and user-session sockets;
  sandbox failure is not evidence virtualization is disabled in firmware.
- **First-boot hostname warning:** the recipe omits `local-hostname` from NoCloud
  metadata, writes `/etc/hostname`, and uses `hostnamectl` in cloud-init's final
  stage. This avoids Fedora 45's early system-bus timing issue. Do not suppress
  unrelated cloud-init errors.
- **VM is missing in VM Manager:** use `qemu:///session` as the same host user.
- **SSH fails:** check `status`, localhost ports 22220/22221, and the serial
  console. Do not disable SSH host-key checking or reuse your personal keys.
- **Desktop fails:** use SSH to inspect `sudo journalctl -b -u greetd`,
  `sudo systemctl --failed`, and the `desktop` user's Hyprland logs.
- **Graphics investigation:** use the explicit `VK_DRIVER_FILES` commands in the
  [GPU workflow](gpu-vulkan.md#run-and-verify), then inspect
  `~/.cache/libvirt/qemu/log/crowbaros-test.log` on the host. A CPU Vulkan device is
  software rendering. Plain system discovery does not select the private driver.
- **Repository key changes:** stop and verify the new source/key before updating
  the pinned digest. Never add `--nogpgcheck` to get a build through.

## Primary references

- [Fedora 45 release announcement](https://fedoramagazine.org/announcing-fedora-linux-45/)
- [Fedora Cloud downloads](https://fedoraproject.org/cloud/download/)
- [Fedora signing certificates](https://fedoraproject.org/security/)
- [Mock documentation](https://rpm-software-management.github.io/mock/)
- [libvirt domain XML and passt networking](https://libvirt.org/formatdomain.html)
- [QEMU VirtIO graphics](https://www.qemu.org/docs/master/system/devices/virtio/virtio-gpu.html)
- [Hyprland installation and Fedora repository](https://wiki.hypr.land/getting-started/installation/)
