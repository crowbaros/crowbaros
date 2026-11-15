# Archive: Fedora development workflow

**Historical source, not an active build or deployment workflow.** Use the
[Void development guide](../../development/local-vms.md) for current work.

This archive preserves the Fedora VM automation, RPM recipes, systemd/Hyprland
session packaging, and earlier app/development instructions. Paths below mirror
their former paths at the repository root. Scripts expect the old checkout
layout and must not be run from this archive. No CI job executes them.

| Location | Historical material |
| --- | --- |
| [lab/fedora/](lab/fedora/) | VM manager, fixtures, validation scripts, guest scripts, and Mesa RPM |
| [apps/](apps/) | Previous app READMEs, RPM specs, and session hooks |
| [docs/development/](docs/development/) | Fedora VM and Intel VirtIO Vulkan setup guides |
| [manifest.json](manifest.json) | Original paths, source commit, and SHA-256 of every preserved file |

Files in the manifest are byte-for-byte copies or moves from the recorded commit.
Their relative links and commands describe that old layout; use the original
commit to reconstruct it. Dated validation records, images, application source,
fonts, licenses, and branding remain at their original locations.

This archive contains no VM disks, checkpoints, credentials, or new signing keys.
The fixture public key and development-key instructions belong to the old lab
and are not a release trust root. No existing local VM data is removed by this
change.

Retirement is tracked in [#65](https://github.com/crowbaros/crowbaros/issues/65).
Its prerequisite is the [integrated Void acceptance record](../../development/void-desktop-smoke-2026-11-15.md)
for [#54](https://github.com/crowbaros/crowbaros/issues/54), with 37 passing checks.
The [preparation record](../../development/fedora-retirement-2026-11-15.md) remains
as written before that prerequisite was complete.
The [retirement acceptance record](../../development/fedora-retirement-completion-2026-11-15.md)
verifies the desktop and Base after this archive was created.
