# Studio Settings

The first Rust/GPUI Settings surface: Dark, Light, and Warm Paper previews,
neutral Studio Mode, explicit Save/Undo, keyboard navigation, inline errors,
and unsaved-close protection. Its source and preference model carry forward;
the Fedora RPM and Hyprland startup hook are
[archived](../../docs/archive/fedora-workflow/README.md).

## Build and run on Void

Use the dependencies and compositor command in the
[Void development guide](../../docs/development/local-vms.md). From this directory:

```sh
cargo build --release --locked
cargo test --release --locked
target/release/crowbaros-studio-settings --version
```

Run the [full desktop smoke command](../../lab/void/README.md#full-desktop-smoke-check)
and launch Settings from its shell. The Alpha launcher supplies
`CROWBAR_SESSION=alpha-no-animations`, selecting persistence without Hyprland.
For a same-user rendering experiment, pass the binary to the compositor's
`--spawn` option with that environment value. It has also rendered on
[Void/NVIDIA](../../docs/research/compositor-nvidia-2026-11-15.md). The earlier
[Settings validation](../../docs/development/studio-settings-validation-2026-11-14.md)
is retained as dated evidence.

Save persists appearance on Void, the shell follows saved themes, and a clean
reboot restores them. The Alpha compositor has no animations: Settings retains
the reduced-motion preference for compatible sessions and describes that limit.
The [acceptance record](../../docs/development/void-desktop-smoke-2026-11-15.md)
covers Save/Undo and reboot. The retained Hyprland adapter and `--apply-session`
helper are not Void startup hooks; production adapter cleanup is tracked in
[#71](https://github.com/crowbaros/crowbaros/issues/71).

## Controls and preferences

| Input | Action |
| --- | --- |
| Tab / Shift+Tab | Move visible focus |
| Enter / Space | Activate the focused control |
| Ctrl+S | Save changes |
| Ctrl+Z | Undo preview changes |
| Ctrl+Q | Close with unsaved-change protection |
| Escape | Cancel the close confirmation |
| Drag title bar | Request a move; maximize/restore is tracked in #36 |

`src/settings.rs` owns model/storage, `src/session.rs` owns compositor integration,
`src/theme.rs` maps brand tokens, and `src/main.rs` presents the surface.
Preferences are schema 1 JSON at `$XDG_CONFIG_HOME/crowbaros/appearance.json`
(falling back to `~/.config/crowbaros/appearance.json`). New files are mode 0600.
Unknown/malformed data is rejected. Save locks, checks for external changes,
stages/syncs the file, applies the session adapter, then renames it. The Alpha
adapter reports that animations are absent; the legacy Hyprland adapter changes
motion and attempts to restore it after a failed rename. It is not a crash-safe transaction
across the file and compositor. `--print-settings` reads preferences headlessly.

Studio Mode is neutral styling, not display calibration. IME, screen-reader
semantics, third-party themes, and the production Settings API require separate
qualification. The Alpha shell follows saved preferences, including Studio Mode.

Owned source is MIT; GPUI/probe notices and Manrope/IBM Plex Mono OFL licenses
remain with the app. See [font provenance](fonts/README.md).
