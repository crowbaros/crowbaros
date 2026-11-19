# Fulcrum serves the shell and the person's apps, and has no compartments

**Status:** proposed on November 18, 2026, by Jim Martin.

Fulcrum was built to the substrate vision's records: a Warden that registers a listener for each compartment, a broker that ends a lock and records transfers, and a table of what each compartment is offered. [ADR 0030](0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md) archived those records and let them bind the compositor "until a new record says what the compositor offers the shell and the Agent". This is that record, for the shell and the apps. It is written from the [vision](../VISION.md), which needs three things of the compositor: no app can pass for the shell, the compositor owns the summon gesture, and a yes comes from the person's own hand. What the compositor offers the Agent is Milestone 3's to decide.

## The decision

- **No archived record binds the compositor.** ADRs 0016, 0018, 0020, 0022, 0023, and 0029 in the [archive](../archive/substrate-vision/adr/) are history, like the rest. What Fulcrum does as a desktop, its frames, workspaces, chords, and placement, is what its [README](../../compositor/README.md) says, and changes by pull request.
- **Two kinds of client: the shell and apps.**
  - The shell connects at its own socket and its control channel, each checked against the shell's user ID. Only the shell is offered layer shell, so only it draws the bar, the dock, the launcher, the wallpaper, and the lock screen.
  - Every app connects at one ordinary Wayland socket, named by `WAYLAND_DISPLAY`, that only the person's account may use. An app is offered what a Wayland app expects, the GPU's buffers included.
- **The Warden's socket and compartments go.** So do the budgets for each compartment. The limits on each client stay: one client cannot take the compositor's descriptors or make it hold pixels without end.
- **Three accounts.** The compositor and the shell each run as a service user of their own, as they do now. Apps run as the person's account, which has a home. A launcher service that runs as the person's account starts the apps the shell asks for. Nothing in the desktop runs as root to start an app.
- **Copy, paste, and drag and drop work between all the person's apps.** The person's act moves the data, as on any desktop.
- **The desktop starts locked, and the person's password unlocks it.** A helper in Base checks the password and tells the compositor to unlock; the shell's own word is not enough. The lock screen takes the password without the confirm chord first: while the desktop is locked, the compositor shows the lock screen and nothing else, so no app can draw a false one. Whether the chord becomes the person's yes to the Agent is Milestone 3's to decide.
- **Fulcrum's proposals are accepted** as they are built: the shell's messages on the control channel (`launcher`, `launching`, `attention`, `lock`, `unlock`, `locked`, `unlocked`), the lock screen's namespace, the unlock helper's socket, and the layout of displays (`outputs` and `configure_output`). `unlock_entry` goes with the chord's part in unlocking.

## Considered Options

- **Keep compartments, with one for all the person's apps.** A Base service would play the Warden. Rejected: it keeps the substrate vision's parts and words in the running system to guard against nothing the vision names.
- **Leave the Warden and compartment code in place, unused.** Rejected: code bound to an archived plan misleads whoever reads it next. It is at the `substrate-final` tag and in this repository's history.
- **Everything runs as the person's account.** Rejected: the user ID check is what keeps an app off the shell's socket and the control channel.
- **Keep the confirm chord before the password.** Rejected for unlocking: it is a step every time, and the compositor already keeps apps off the screen while it is locked.

## Consequences

- Any app from Void's repository runs on the desktop as it is, with no launcher stand-in or registration.
- An app can read what another app copied, and every app can use the GPU. The [lessons](../LESSONS.md) say what to reach for if the Agent's reach makes that a real risk.
- Fulcrum's plan and README are rewritten against the vision. Its milestones that waited on the broker, asks, transfers between compartments, and the record of each unlock, are dropped, not waiting.
- The compositor check stops checking what a compartment is offered, and checks instead that an app at the ordinary socket is refused layer shell and the control channel.
