# Fedora lab (interface work only)

CrowbarOS is no longer built on Fedora. This lab stays for one reason: today it
is the only place the GPUI apps in [`apps/`](../../apps/README.md) are built, run,
and validated. It builds each app as a signed RPM in a Fedora builder VM and runs
it under Hyprland in a Fedora test VM.

- Use it to work on the look and behavior of the interfaces.
- Do not add Fedora, RPM, or Hyprland work that the [vision](../../docs/VISION.md)
  does not need.
- Remove it when the [Void lab](../void/README.md) has a graphical guest that can
  run the apps.

| Path | Contents |
| --- | --- |
| `lab.py` | The lab command: VM lifecycle, provisioning, and the build, deploy, and run steps for each app |
| `config.json` | Image identity, signing fingerprint, and VM resources |
| `guest/` | Provisioning and packaging scripts that run inside the guests |
| `check_*.py`, `validate_shell.py` | Interaction checks that drive the apps in the test VM |
| `gpu/`, `fixtures/` | The Intel VirtIO Vulkan driver spec and the RPM pipeline probe |

The workflow is in [`docs/development/local-vms.md`](../../docs/development/local-vms.md).
VM disks and build artifacts go to `.local/` in the repository root, outside git.
