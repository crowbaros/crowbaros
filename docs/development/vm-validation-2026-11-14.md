# Local VM validation — November 14, 2026

The Fedora build/test lab was created and exercised on the existing Omarchy
workstation. Physical hardware testing was not performed. See
[the reproducible workflow](local-vms.md) for lifecycle and provisioning commands.

**Follow-up:** the later [GPUI validation](gpui-validation-2026-11-14.md) confirms
actual native application rendering and input from a signed RPM on software
Vulkan. The subsequent [GPU validation](gpu-validation-2026-11-14.md) also confirms
hardware Vulkan through Intel VirtIO native contexts, including GPUI input/redraw
checks and a cold boot. The observations below preserve the earlier infrastructure
baseline and its then-unresolved Venus limitation.

## Recorded baseline

| Item | Observed version/configuration |
| --- | --- |
| Base image | Fedora Cloud Base Generic 45-1.4, x86_64 QCOW2 |
| Image SHA-256 | `50bdc5e70f6a24f540932d15411c1c6ae9fcfa40fb990d876d83c5cc823b2ad0` |
| Fedora image signer | `4F50A6114CD5C6976A7F1179655A4B02F577861E` |
| Guest kernel | `7.2.11-300.fc45.x86_64` |
| Rust / Cargo | `1.99.1-2.fc45` |
| Mock | `6.9-1.fc45` |
| Test compositor | Hyprland `0.57.2-1.fc45`, upstream commit `11cbc6c577b84ffb1edf4df3fc5b6128c3ae6947` |
| Hyprland package source | Restricted `lionheartp/Hyprland` COPR, disabled by default |
| COPR public-key fingerprint | `97E23476C89635135407C7D5E9BA41342C4B2995` |
| Guest Mesa | `26.2.6-1.fc45` |
| QEMU host | `11.1.2-1` |
| libvirt host | `12.8.0-1`, user session |
| Host graphics library | virglrenderer `1.3.0-2`, Mesa/Intel Vulkan `26.2.5-1` |
| Firmware package | `edk2-ovmf 202608-1` |

## Checks performed

- Verified Fedora's checksum signature against the published fingerprint and
  matched the downloaded image's SHA-256.
- Created independent copy-on-write disks, cloud-init seeds, UEFI variable stores,
  unique instance IDs, and pinned SSH host keys.
- Confirmed KVM execution and localhost-only SSH forwarding on ports 22220/22221.
- Confirmed first-boot completion and subsequent clean boots of both guests.
- Confirmed the guest root filesystems grew to 120 GiB and 64 GiB capacities.
- Provisioned Rust/Cargo and Mock in the builder; retained 763 installed-package
  records in `.local/vms/build/reports/packages.tsv`.
- Provisioned the test desktop; retained 740 installed-package records in
  `.local/vms/test/reports/packages.tsv`.
- Started greetd and Hyprland under `desktop`, which has no administrator group.
  Confirmed a terminal rendered at 1280×800 and inspected a guest screenshot.
- Confirmed Hyprland reported no configuration errors.
- Confirmed accelerated OpenGL: `virgl (Mesa Intel(R) Graphics (RPL-S))`, OpenGL 4.3,
  `Accelerated: yes` from `glxinfo -B`.
- Confirmed software Vulkan: `llvmpipe (LLVM 23.1.2, 256 bits)`, Vulkan 1.4.354.
  A 120-frame Wayland `vkcube` run completed successfully.
- Built a Rust SRPM and binary RPM in clean Fedora 45 Mock buildroots, then signed
  both the binary package and generated repository metadata with a local-only key.
- Installed through DNF with package and repository-metadata verification enabled;
  the probe ran successfully as `desktop`.
- Restored the clean offline checkpoint and confirmed the probe package and its
  development repository were absent afterward.
- Confirmed the restored test guest had active greetd, qemu-guest-agent, and
  firewalld services, SELinux enforcing, and zero failed system units.
- Verified the checkpoint command refuses a running guest. Verified the stopped
  build disk using `qemu-img check`, with no errors.
- Rebuilt the self-contained blueprint and passed its existing offline browser,
  source-coverage, navigation, asset, download, and responsive-layout checks.

## Resolved setup issues and remaining limits

Fedora's early cloud-init hostname handling attempted to contact the system bus
before it was ready. The final recipe omits NoCloud's `local-hostname`, writes
`/etc/hostname`, and sets the live name during the final stage. The corrected test
guest completed cloud-init without warnings; the builder's subsequent boot also
completed without warnings.

Fedora's greetd package uses the `greetd` service account. The recipe uses that
account and a separate, ordinary `desktop` account for the initial session.

Enabling VirtIO/Venus Vulkan failed host renderer initialization and caused guest
graphics/shutdown problems. Explicit Intel Vulkan selection did not resolve it.
The accepted configuration disables Venus and uses verified VirGL OpenGL plus
software Vulkan. This is adequate for the verified lab checks, but **accelerated
Vulkan and actual GPUI application rendering remain unvalidated**. No GPU
performance, color-management, hardware-compatibility, or accessibility claims
follow from these VM tests.

## Handoff state

- `crowbaros-build`: shut down; offline checkpoint `provisioned` saved.
- `crowbaros-test`: running from the restored `clean` checkpoint.
- The outgoing state containing the installed probe is retained in a timestamped
  `before-restore-*` checkpoint.
- Signed smoke artifacts and build logs remain in the builder; the transferable
  repository archive is `.local/vms/smoke-repo.tar.gz`.
- Local VM state, caches, and checkpoints occupy approximately 11 GiB as reported
  by `du`; reflink sharing can make that differ from unique physical extents.
- Private VM state is excluded by the repository's `.gitignore`.
