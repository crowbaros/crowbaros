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
  `watch`. On first summon, when Lever reports `models_chosen` false and there
  are no existing threads, the drawer asks where Models come from — offering
  **Use free hosted Models** (`set-free-models`) and **Sign in** (launching
  Base's `com.crowbaros.AgentSignIn.desktop` through the launcher service, which
  runs `crowbar-agent login` in `foot`) — and sends nothing until a choice is
  made. When sign-in finishes or the setting changes, Lever's `models` and
  `free-models` events update the drawer live with no restart of Lever or the
  shell. It sends messages and `stop` for the thread, streams the answer,
  each act, notes, desktop context, and stopped/failed/interrupted states,
  refuses a message with an explanation when no Model is available, and
  shows when no harness is configured. Putting the drawer away stops nothing.
  A transient history-read failure preserves the selected thread. If the next
  send finds that turn still working, the client reads its history again and
  rejoins its events; it never sends the message a second time. A delayed
  recovery reply cannot replace a later selection. A failed socket clone also
  keeps the first connection's thread discovery available for the retry.
- A request from the harness shows in its turn: the exact command, or the file
  and the change, with removed (`-`) and added (`+`) lines told apart in the
  mono face, and one control per answer it offers, named as the harness names
  them. A click sends `answer`, and so does Alt with a digit, for the answer
  at that place of the request that has waited longest; the card then says
  what was answered, or that
  the request was withdrawn. A request waits with no time limit, as long as
  Lever says it does, after its turn ended too: the drawer redraws it when
  summoned again and after the shell restarts, from `resume`. Stop shows
  while the thread works or a request of its waits, and withdraws it. A long
  change shows as Lever cut it, what it takes out and what it puts in.
- The person drives a thread from the drawer's message box, with slash
  commands. A `/` that begins the message lists them above the box, narrowed
  as the person
  types: a name's start before its middle, and then what a command does. Up
  and Down move through the list, Enter or Tab takes a row, a click does too,
  and Escape closes the list and keeps what was typed. The drawer's own
  commands come first: `/model`, `/effort`, `/mode`, `/default-mode`,
  `/free-models`, `/sign-in`, `/new`, `/threads`, and, where they apply,
  `/stop`, `/archive`, and `/delete`. One
  the thread cannot take now, as `/mode` while a turn works, is shown with
  the reason, and taking it changes nothing.
- A command that needs a choice lists the choices in the same place: `/mode`
  the modes Lever offers, `/default-mode` the modes new threads may start in,
  `/free-models` whether free hosted Models are on or off, `/effort` the effort
  levels of the thread's Model, and `/model` the Models.
  The list starts on the choice the thread is at. A command typed whole, as
  `/mode plan`, needs no list. What a list needs is asked of Lever when the
  list opens, with `models` and `efforts`, since that starts the harness.
- `/model` is the Model picker: the providers the harness has Models from on
  a rail, the Models of the one picked, and, once the person types, a search
  through every provider with no rail, with **Sign in** and free hosted Models
  controls in its footer. Left and Right move between providers.
  Picking a Model sends `set-model`, and where the Model has effort levels
  the drawer lists them next. A thread keeps its Model and effort, and the
  drawer shows them as soon as Lever says them. For the drawer's new thread
  a pick goes with its first message, and a new thread starts with the Model
  and effort last picked. The shell keeps those beside the active thread in
  its runtime directory, so they outlast the shell and not a restart of the
  machine. Which harnesses, providers, and Models are offered is the
  harness's to say until Studio Settings has a screen for it.
- The commands the thread's harness offers follow the drawer's own in the
  list, each marked with the harness's name and described in the harness's
  words (`commands`). Taking one puts `/` and its name in the message box, for
  the person to add to and send as the thread's next message. Anything typed
  after a `/` that is no command of the drawer's goes to the harness as it
  is.
- The message box holds many lines: Enter sends, Shift+Enter begins a new
  line, and pasted text keeps its lines. It grows with what it holds, to
  eight lines, and then scrolls. What the person typed in a thread and did
  not send is there when they come back to that thread, and when the drawer
  is put away and summoned again. One quiet line under the box says the
  thread's Model, effort, and mode. While a turn works, a message sent from
  the box goes into that turn where the thread's harness can take one, as
  Lever's `models` says (`steering`): it shows in the turn's answer where the
  harness took it, as the person's message does, set to the right, and stays
  there when the thread is read again. Where the harness cannot take one, or
  did not take this one, the box keeps the message and the drawer says why.
- An answer shows as the Markdown it is: paragraphs, headings, lists, quotes,
  strong and emphasised words, links (with destinations shown beside titled
  links; clicking copies the URL to the clipboard since the shell runs as its
  own account), inline code, and fenced code in the
  mono face, read again as each piece arrives. The person's message sits to
  the right. While a turn works its acts show as lines, with how long the
  turn has worked. When it ends they fold into one line, as `Worked for 12s ·
  3 acts`, which opens, and an act's line opens to what it gave. Each subagent
  shows in its turn as a line (`subagent:<id>`) that opens to its prompt, its
  acts, and its messages. A subagent's request shows in the thread and is
  answered there, and while a subagent still works after its turn ended the
  thread stays working and Stop remains available. A message can still be
  sent then, and the mode, Model, and effort changed, though a message waits
  while a subagent's request does: Lever says to answer it, or stop the
  thread, first. When the harness goes on by itself after a subagent
  reported, its turn shows a quiet line, `The Agent went on after a subagent
  reported`, in place of a message, and a message sent meanwhile takes that
  turn over.
- The mark covers every thread, from Lever's `activity`: `Waiting` while any
  request waits, else `Working` while any thread works, else `Done` once the
  last working thread ends with the drawer put away, until the drawer is
  summoned or put away. It is shown in the bar and, whenever a fullscreen
  window covers the bar while the drawer is put away, in a floating overlay
  surface (`crowbaros-mark`) at the top right. The drawer never comes up on
  its own, not even for a request.
- The shell's own `crowbar-shell`-only `control.sock` answers `agent` beside
  `snapshot`: whether the drawer is up and on which display, the mark, the
  thread with its turns, requests, Model, effort, and mode, the starting
  mode, what the message box holds (`draft`), the list open above it
  (`list`: what it is a list of, its rows in order, and the row the keys are
  on), and `controls`, the bounds of each control the drawer shows, in the
  display's logical pixels, for checks to click: `answer:<request>:<answer>`,
  `acts:<turn>`, `act:<id>`, `subagent:<id>`, `context:<turn>`, `newest`, `send`, `stop`, and
  `close`, and, of an open list, `command:<name>`, `choice:<value>`, and
  `provider:<id>`. Model-source controls add `choose-free-models`,
  `disable-free-models`, `toggle-free-models`, `sign-in`, and `refresh-models`.
  Thread navigation adds `new-thread`, `threads`,
  `thread:<id>`, `cancel-delete`, and `confirm-delete`. A control scrolled
  out of view is left out.
- The drawer has a thread list and a thread view. **Threads**, `/threads`, or
  `Ctrl+L` returns to the list, grouped by folder with newest activity first.
  **New thread**, `/new`, or `Ctrl+N` starts in the person's home; terminal
  threads keep their
  terminal's working directory. Summoning returns to the view left behind.
  Opening a record reads Lever's history without starting its harness. Tab and
  Shift+Tab move among controls; Enter activates the focused control.
- `/archive` hides a thread but keeps its record. `/delete` asks for
  confirmation; Escape cancels that confirmation. Stop active work before
  deleting it. Lever requests harness session deletion, and explains when the
  harness cannot remove its own copy. The Mark follows all threads, including
  terminal and archived work, while the drawer is hidden or displaying another
  thread. Waiting requests take precedence over working threads; working
  takes precedence when another thread finishes. Stop also withdraws requests
  that remain after a turn ends, before the thread can be deleted.
- Streamed answers follow the bottom only while the person is already near it.
  Scrolled up, the thread holds its place and shows **Newest**, which goes
  back to its newest line. What an act gave shows its first lines, and all
  of it when asked; desktop context after the first turn is collapsed until
  requested.
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
