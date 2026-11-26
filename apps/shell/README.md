# CrowbarOS desktop shell

The Rust/GPUI desktop slice: workspace bar, dock, installed-application search,
Signal Feather wallpaper, and the Agent's surface. The interface and its assets carry forward to
Void; Fedora packaging and session hooks are
[archived](../../docs/archive/fedora-workflow/README.md).

## Build and run

Use a Void development environment with the dependencies in the
[build/run guide](../../docs/development/local-vms.md). From this directory:

```sh
python3 packaging/prepare-gpui.py
cargo build --release --locked
cargo test --release --locked
```

The sources are not rustfmt-clean, and CI runs `cargo test` only. Do not run
`cargo fmt` here: it reformats files a change does not touch.

GPUI 0.2.2 uses the existing verified layer-shell backport, a patch with
which a still window commits nothing, and one with which a destroyed window
draws nothing ([patches](patches/README.md)).

With no display, the shell waits for one before it makes its layers. When the
last display goes away, the compositor closes the shell's layers and GPUI
stops, so the shell exits cleanly; its service starts it again, and it waits.
`compositor/tests/shell_outputs.rs` runs the real shell through that on a GPU
(#271).
The [desktop check](../../lab/void/README.md#the-desktop-check) builds Base's
image and runs the shell as Base runs it, on a copy of that image. On Base the
shell runs as its own service account, `crowbar-shell`, and connects to the
compositor's shell socket and control channel, which take the shell's user ID
alone ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)).
The [development guide](../../docs/development/local-vms.md) also describes a
nested, same-user rendering experiment, which runs no launcher service and has
no separate accounts.

The [Void NVIDIA record](../../docs/research/compositor-nvidia-2026-11-15.md)
shows rendering, resize, and suspend/resume for the tested setup. The earlier
[shell validation](../../docs/development/shell-validation-2026-11-14.md) remains
historical behavior evidence, not a current Fedora workflow.

## How it behaves on the desktop

- The bar and dock reserve 24 and 48 pixels through Wayland layer surfaces.
  Only the shell is offered layer shell, so no app can draw the bar, the dock,
  the launcher, the Quick Settings popover (`crowbaros-quick-settings`), the
  bar clock calendar (`crowbaros-calendar`), the transient volume/brightness OSD
  pill (`crowbaros-osd`), the wallpaper, the lock screen, or the Agent's surface.
- Clicking the top-right Quick Settings control opens a floating layer popover
  with Wi-Fi (`iwd`), Bluetooth (`BlueZ`), PipeWire volume and microphone mute,
  display brightness (when `/sys/class/backlight/*` exists), Lock/Suspend/Reboot/Power
  Off actions (with confirmation on Reboot and Power Off), and a shortcut to
  Studio Settings. Hardware volume, mute, and brightness keys or Quick Settings
  changes show a brief non-focus-stealing OSD pill clear of the dock that
  auto-dismisses.
- Clicking the center clock in the bar toggles a month calendar popover with
  `Left`/`Right` month navigation, `Escape`/click-outside dismissal, and a quick
  timer.
- Applications search supports keyboard navigation; closing the launcher returns focus.
- Workspace and window focus requests go over the compositor's control
  channel, and the compositor tells the shell of each change to the windows
  and workspaces as it happens, so the bar shows it at once.
- A still desktop draws nothing: the bar, dock, lock screen, and wallpaper
  commit only when what they show changes.
- Super tapped alone toggles the launcher, an app the shell launches takes the
  keyboard with its first window, and the bar and dock mark a window that
  wants attention.
- Super+Space, the summon gesture, brings up the Agent's surface
  ([ADR 0035](../../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md)):
  a drawer on the right of the display the pointer is on, that display's
  full height and a third of its width, above every window, a fullscreen one
  included, and taking no room from any. The compositor tells the shell of
  each gesture, with the display and whether the keyboard is in the drawer,
  and the shell holds whether the drawer is up. A gesture brings the drawer
  up with the keyboard; with the drawer up and the keyboard in a window, it
  brings the keyboard back; with the keyboard in the drawer, it puts the
  drawer away and the window that had the keyboard has it back. The drawer's
  close control puts it away too. Escape does not. Locking leaves the drawer
  as it is, and it is up or put away after unlock as it was.
- When `CROWBAR_LEVER_SOCKET` is set, the shell connects to Lever's surface
  socket (`/run/crowbar-lever/surface`) with backoff using the shared version-1
  framing, waits for Lever's initial `status` (and restores the active thread,
  or after a fresh start the one waiting for the person, else the working or
  newest one, via `list` and `resume`) before reporting connected, then sends
  `watch`. It sends messages and `stop` for the thread, streams the answer,
  each act, notes, desktop context, and stopped/failed/interrupted states, and
  shows when no harness is configured. Putting the drawer away stops nothing.
- A request from the harness shows in its turn: the exact command, or the file
  and the change, with removed (`-`) and added (`+`) lines told apart in the
  mono face, and one control per answer it offers, named as the harness names
  them. A click sends `answer`; the card then says what was answered, or that
  the request was withdrawn. A request waits with no time limit, as long as
  Lever says it does, after its turn ended too: the drawer redraws it when
  summoned again and after the shell restarts, from `resume`. Stop shows
  while the thread works or a request of its waits, and withdraws it. A long
  change shows as Lever cut it, what it takes out and what it puts in.
- The drawer's header shows the thread's mode. Its control opens a menu of the
  modes Lever offers, asked for with `models` only when the menu opens, since
  that starts the harness. Picking one sends `set-mode`, or, for the drawer's
  new thread, goes with its first message. The control rests while the thread
  works. The menu also sets the mode new threads start in
  (`set-default-mode`), Ask first or Full access, and shows which it is.
- The mark covers every thread, from Lever's `activity`: `Waiting` while any
  request waits, else `Working` while any thread works, else `Done` once the
  last working thread ends with the drawer put away, until the drawer is
  summoned or put away. It is shown in the bar and, whenever a fullscreen
  window covers the bar while the drawer is put away, in a floating overlay
  surface (`crowbaros-mark`) at the top right. The drawer never comes up on
  its own, not even for a request.
- The shell's own `crowbar-shell`-only `control.sock` answers `agent` beside
  `snapshot`: whether the drawer is up and on which display, the mark, the
  thread with its turns, requests, and mode, the starting mode, whether the
  mode menu is open, and `controls`, the bounds of each control the drawer
  shows, in the display's logical pixels, for checks to click: `mode`,
  `mode:<id>`, `default-mode:<id>`, `answer:<request>:<answer>`, `send`,
  `stop`, and `close`. Thread navigation adds `new-thread`, `threads`,
  `thread:<id>`, `archive`, `delete`, `cancel-delete`, and `confirm-delete`.
  A control scrolled out of view is left out.
- The drawer has a thread list and a thread view. **Threads** (or `Ctrl+L`)
  returns to the list, grouped by folder with newest activity first. **New
  thread** (`Ctrl+N`) starts in the person's home; terminal threads keep their
  terminal's working directory. Summoning returns to the view left behind.
  Opening a record reads Lever's history without starting its harness. Tab and
  Shift+Tab move among controls; Enter activates the focused control.
- **Archive** hides a thread but keeps its record. **Delete…** asks for
  confirmation; Escape cancels that confirmation. Stop active work before
  deleting it. Lever requests harness session deletion, and explains when the
  harness cannot remove its own copy. The Mark follows all threads, including
  terminal and archived work, while the drawer is hidden or displaying another
  thread. Waiting requests take precedence over working threads; working
  takes precedence when another thread finishes. Stop also withdraws requests
  that remain after a turn ends, before the thread can be deleted.
- Streamed answers follow the bottom only while the person is already near it.
  Long act details expand to the complete command/output; desktop context after
  the first turn is collapsed until requested.
- The desktop starts locked. While the compositor has it locked, the shell
  shows its lock screen, and the bar's lock button and Super+L lock it. The
  lock screen takes the person's password at once and sends it to Base's
  unlock helper (`CROWBAR_UNLOCK_SOCKET`), which checks it and tells the
  compositor to unlock; a wrong one shows "Wrong password" after a pause.
  Without a helper, as in the nested experiment, its Unlock button or Enter
  asks the compositor to unlock.
- The shell asks Base's launcher service to start an app, and the launcher
  starts it as the person's account. The shell reads the person's saved
  appearance through a read-only request to the launcher, and follows the
  theme they saved; it does not theme other apps.
- The shell lends Studio Settings display, Input, and Power settings through a bounded socket
  of its own (`CROWBAR_DISPLAY_SOCKET`), which takes only the person's and
  the shell's user IDs and passes on only `outputs`, `configure_output`,
  `input_config`, `configure_input`, `power_config`, and `configure_power`.
  Before serving requests, it reads saved Input and Power files through the
  launcher and restores them through the same validation. This runs at every
  shell start, including after a compositor restart or reboot; it never
  gives apps general shell or compositor control.
- Desktop-entry discovery honors XDG precedence, visibility, locale, and executable availability. Search text is never executed as a command.
- The compositor and the shell restart independently under runit. A shell
  restart keeps the person's windows open; a compositor restart comes back
  locked.

These messages, the lock screen's namespace, and the unlock helper's socket
are accepted in ADR 0032. The summon gesture's messages and the Agent's
surface's namespace, `crowbaros-agent`, follow ADR 0035, which is proposed. The compositor's README lists the messages and
their framing, under
[the control channel](../../compositor/README.md#the-control-channel). Desktop shortcuts and window-layout behavior come
from the compositor.

`src/session.rs` retains Hyprland, `systemd-run`, and `systemctl` fallback
paths from before Void. Base's services do not use them; do not use
`--start-session` as a Void startup command. Their old service and Lua hook
are archived and are not installed.

The [desktop acceptance record](../../docs/development/void-desktop-smoke-2026-11-15.md)
covers the desktop as it was on that date. Accessibility and input methods are
not established yet.

## Licensing

Native source is MIT. Manrope and IBM Plex Mono retain their OFL notices.
The adapted GPUI input example and layer-shell backport retain Apache-2.0 terms.
Brand origins remain in [assets/README.md](assets/README.md). No source, font,
license, asset, or Cargo lockfile was removed with the RPM recipe.
