# CrowbarOS desktop shell validation — November 14, 2026

**Result:** the first Rust/GPUI shell is packaged, installed, and running in the
Fedora 45 test VM. It starts automatically with Hyprland after a cold boot. The
viewer remains open beside Codex on host workspace 4.

## Package and build evidence

| Item | Validated value |
|---|---|
| Binary RPM | `crowbaros-shell-0.1.1-1.fc45.x86_64.rpm` |
| RPM size | 9,574,815 bytes (9.13 MiB) |
| Binary SHA-256 | `3d75af2f65fc05adf4cc673ba91dc2def38ac97b2cede7e861d64c315d3fd11e` |
| Source RPM SHA-256 | `811c0e09e57feb90cb67c52ca4319b5fdecfab23dc154092720ee1a13d670ce0` |
| Toolchain | Fedora Rust/Cargo 1.99.1 |
| GPUI | 0.2.2, Wayland only, private documented layer-shell backport |
| Compositor | Hyprland 0.57.2-1.fc45, Lua configuration |
| Renderer | Intel Graphics RPL-S through the isolated Intel VirtIO Vulkan ICD |
| Test user | Ordinary `desktop` account, UID 1001; no Rust/Cargo installed |
| Security state | Guest SELinux Enforcing; QEMU sandbox unchanged |

Clean Fedora Mock builds compile with frozen, vendored dependencies and run three
catalog tests. The binary RPM and repository metadata carry the existing local
development signature (`B84C3D26F0D574EB5299BAE532465B0A19A1B171`, **NOT FOR RELEASE**).
DNF verifies both before installation; `rpm -V crowbaros-shell` is clean. The
recorded manifest matches all 21 corresponding repository source/asset files.
Fedora hardening flags remain enabled; the development package uses optimization
level 2, 16 codegen units and no debug information. This is not a performance benchmark.

Retained artifacts:

- `.local/shell/artifacts/`: signed binary RPM, corresponding source RPM, build log and manifest.
- `.local/vms/shell-repo.tar.gz`: signed local repository for deployment.
- `.local/shell/validation/`: actual screenshots, layer/client/monitor JSON, interaction checks, cold-boot record and journal.
- `.local/vms/test/reports/`: installed package inventory and system baseline.
- `before-shell` checkpoint: prior working lab; `shell-0_1_1`: validated package state before the successful cold boot.

## Checks completed

- Real Wayland layers: background wallpaper, 44 px top bar, and centered dock with
  96 px reserved space. The shell creates no ordinary application top-level windows.
- Applications button, dock button and Alt+Space open the launcher. Search, arrows,
  Tab, Enter, Escape and outside-click dismissal work. The keyboard grab is limited
  to the open launcher.
- Installed desktop entries are discovered; native Settings launches from search.
  Dock controls launch or focus Terminal/Settings. Workspace buttons switch the
  actual compositor workspace; the bar follows the active window.
- Windows float by default; Settings opens at 980×640 in the 1280×800 test desktop.
  Alt+left-drag moves windows; Alt+right-drag and window borders provide resize controls.
- Applications launch through GIO in separate systemd user scopes. A terminal keeps
  its PID and remains open when the shell service restarts.
- Settings Save changes the shell to Light. Dark, Warm Paper and Studio neutral
  chrome follow the same saved preference schema. The approved wallpaper is unchanged.
  The validation restores Dark, Studio off, reduced motion on.
- Long-query horizontal scrolling, replacement with a short query, accented Unicode
  clipboard input, selection, empty results, and recoverable executable-launch errors
  were exercised. The query remains available after a failed launch.
- Live changes between 1280×800 and 1024×768 keep the bar at the full output width
  and the dock centered. The compact check qualifies shell surfaces, not every
  application's minimum size within the reserved work area.
- Cold boot starts greetd → Hyprland → the shell user service automatically. All
  three persistent layers return, screen reservations match, saved reduced motion
  is applied, and `hyprctl configerrors` is empty.

The initial layer-shell implementation fed configured dimensions back as fixed
sizes, cropping the bar after an output change. The final package preserves
compositor-controlled axes. This and the initial nonzero GPU allocation are
recorded in the [backport notes](../../apps/shell/patches/README.md), alongside the
upstream commit and the remaining small compatibility adaptations.

## Scope and observed limitations

This is a single-output first slice with a persistent dock. It does not implement
auto-hide, a full running-app task switcher, notifications, lock/power controls,
multi-monitor handling or the planned five-app product preload. Screen-reader,
IME, fractional-scale, international/font coverage and idle-power qualification
remain later work. Accented text renders; the emoji probe shows a missing glyph
in the minimal guest font set. Do not read this as complete Unicode/font coverage.

The experimental accelerated SPICE profile can pause Vulkan initialization when
the viewer is hidden on another host workspace. Bringing the viewer forward
resumed initialization in this run. Keep the viewer visible during startup,
shutdown and graphics checks; security settings were not relaxed to work around it.

## Review and reproduction

A fresh Impeccable finish reviewer returned **ship** for this bounded first slice,
with no material fixes. The documenter compared all final captures and native
sources, confirmed the palette/Manrope/approved image provenance, and preserved
`DESIGN.md` and `.impeccable/design.json` without changing global tokens. Native
HTML/CSS detectors do not apply; this is not an accessibility certification.

Use the [shell workflow](../../apps/shell/README.md) for build, deployment, recovery
and `infra/vm/validate_shell.py`. Keep the VM viewer open and pause manual input
while the automated native checks run. The broader
[local VM guide](local-vms.md) covers image creation and signing prerequisites.
The self-contained HTML blueprint was rebuilt and passed its existing offline,
content and navigation checks at 3.42 MiB.
