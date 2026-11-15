# Studio Settings

> **Status.** This app was built for the earlier Fedora plan. Its interface work
> carries over to the [vision](../../docs/VISION.md); its RPM packaging and Hyprland
> wiring do not. It is built and run only in the [Fedora lab](../../lab/fedora/README.md),
> which is kept for interface work. See [what carries over](../README.md).

The first native CrowbarOS settings slice, implemented in Rust and GPUI 0.2.2:

- Dark, Light, and Warm Paper appearance modes with an immediate app preview.
- A neutral Studio Mode override that retains the underlying appearance choice.
- Reduced motion applied to Hyprland when changes are saved.
- Explicit save/undo, keyboard navigation, inline errors, and unsaved-close protection.
- Versioned user preferences and a small, separate compositor adapter.

Appearance currently styles **Studio Settings itself**. Desktop-wide theme tokens,
GTK/Qt integration, wallpaper, fonts, display settings, and the full shell are
future work. Studio Mode does not calibrate a display. Text fields/IME and
screen-reader semantics are not validated by this slice.

See the [validation record](../../docs/development/studio-settings-validation-2026-11-14.md)
for the tested RPM, screenshots, behavior checks, and remaining integration gates.

## Build, package and run

Use the [Fedora VM lab](../../docs/development/local-vms.md). Keep its viewer open,
preferably on the same workspace as Codex. All compilation happens in the builder;
the test guest only receives the signed package.

```sh
rtk python3 lab/fedora/lab.py stop test
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

The last argument selects the [isolated hardware Vulkan profile](../../docs/development/gpu-vulkan.md).
Use `software` for a deliberate lavapipe run or `system` for Fedora's loader
discovery. With no argument, the launcher inherits the lab's `gpui_vulkan_driver`
setting. Quit a running instance before switching drivers. The ordinary desktop
entry follows system driver discovery.

The build vendors the committed Cargo graph and compiles with `--frozen` in a
clean Fedora 45 Mock buildroot. `%check` runs persistence/rollback tests, checks
the CLI version, and validates the desktop entry. RPMs and repository metadata
are signed with the lab's **NOT FOR RELEASE** development key. DNF verifies both
signatures. Live Fedora build dependencies mean this is a repeatable workflow,
not a byte-identical release build.

Host artifact: `.local/vms/studio-repo.tar.gz`. Builder artifacts and logs:
`~/artifacts/studio/`. The test repository is disabled except during explicit
installation/upgrade. It does not require a running builder or an HTTP server.

## Use

Theme and Studio controls preview in this window. Reduced motion takes effect
only after **Save changes**. **Undo** returns to the last loaded/saved preferences.
**Reload saved**, shown on an error, reads the file again and discards the current
preview. Closing with unsaved edits offers Keep editing or Discard & close.

| Input | Action |
| --- | --- |
| Tab / Shift+Tab | Move visible keyboard focus |
| Enter / Space | Activate the focused control once |
| Ctrl+S | Save and apply desktop motion |
| Ctrl+Z | Undo the preview |
| Ctrl+Q | Close, checking unsaved changes |
| Escape | Cancel the close confirmation |
| Drag titlebar | Move the floating window |

The GUI is launched as the ordinary `desktop` user. It never uses sudo or
changes host Omarchy settings. Application logs are available through:

```sh
rtk python3 lab/fedora/lab.py ssh test sudo journalctl -b \
  _SYSTEMD_USER_UNIT=crowbaros-studio-settings.service --no-pager
```

## Preferences and integration

`src/settings.rs` owns the model and storage, independently of GPUI.
`src/session.rs` owns the version-specific Hyprland adapter. `src/theme.rs` maps
preferences to the approved brand tokens; `src/main.rs` owns presentation.

The file is `$XDG_CONFIG_HOME/crowbaros/appearance.json`, or
`~/.config/crowbaros/appearance.json` when XDG_CONFIG_HOME is not an absolute path.
New files are mode 0600. Schema 1 stores `theme`, `studio`, and `reduced_motion`.
Unknown schemas, malformed files, and unexpected fields are rejected without
overwriting the file. Save uses an advisory lock, checks for external changes,
stages and syncs the file, applies the motion setting, then atomically renames it.
If the rename fails, the adapter attempts to restore the prior desktop motion
state and reports any rollback failure. This is best-effort coordination of two
systems, not a crash-proof distributed transaction.

The adapter selects the compositor matching the current Wayland socket and uses
fixed `hyprctl` commands with a bounded timeout. It reads back the motion value
to verify the change. This release targets the lab's Hyprland 0.57 Lua API.

The RPM ships `/usr/share/crowbaros-studio-settings/hyprland.lua`. Lab deployment
loads it from the lab-owned compositor config; it runs `--apply-session` when
Hyprland starts. The helper is a no-op if preferences have never been saved.
It restores motion only and does not launch the GUI. The RPM does not edit user
Hyprland configuration. Other sessions must explicitly load the supplied hook.
Startup restoration retries transient compositor-readiness failures for up to
10 seconds, plus any in-flight bounded command. Interactive saves do not retry.
The hook sends diagnostics to the journal identifier `crowbaros-settings-session`.

Headless helpers:

```sh
crowbaros-studio-settings --version
crowbaros-studio-settings --print-settings
crowbaros-studio-settings --apply-session
```

`--print-settings` reads preferences; `--apply-session` requires the target user's
active Wayland/Hyprland environment. Neither command needs a Vulkan device.

## Recovery and licensing

The test VM has a `before-studio-settings` checkpoint. Restoring it recovers the
pre-installation disk; see the lab workflow for disk and graphics-profile recovery.
To stop using the app while retaining preferences, close it and remove its RPM
through DNF. Removing the RPM does not reset a motion change already applied to
the running compositor; a fresh lab login without the hook uses the lab defaults.

Owned application code is MIT. The earlier GPUI probe's MIT notice,
GPUI/dependency notices, and bundled font OFL licenses are retained. Fonts are
the same static Manrope weights and IBM Plex Mono used by the validated probe;
see [font provenance](fonts/README.md).
