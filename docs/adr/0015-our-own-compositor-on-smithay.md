# CrowbarOS has its own compositor, built on Smithay

The Wayland compositor is a CrowbarOS program, written in Rust on the [Smithay](https://github.com/Smithay/smithay) library. It is part of Base and part of what we trust. We do not ship Hyprland, a fork of it, or another existing compositor.

[ADR 0028](0028-the-compositor-is-written-from-the-ground-up.md) changes what it is built on: it is written from the ground up, with Smithay as its reference and not a dependency.

The compositor is where three rules of [ADR 0014](0014-a-yes-comes-from-the-person.md) are enforced, so it cannot be a borrowed part that we configure:

1. **Only the shell delivers input.** The compositor offers no virtual keyboard, virtual pointer, input-method, or input-emulation interface to a Tool or an app. With Smithay a protocol exists only if our compositor creates it, and each one can be limited to named clients.
2. **The confirm gesture never reaches a Tool.** The compositor takes the key press itself and forwards it to no client.
3. **System chrome cannot be imitated or covered.** The compositor decides what is drawn above what, knows which principal owns each surface, and draws the marking of a preview and of an ask itself.

It also gates screen capture and the clipboard per principal, so no pixels and no pasted value leave a surface without the person's say.

The evidence is in the [compositor research](../research/compositor-smithay.md). The decision rests on these findings from it:

- Smithay's protocols are opt-in and take a per-client filter, and its keyboard input can withhold a key from every client.
- Two shipping desktops, COSMIC and niri, are built on Smithay.
- Hyprland's control socket runs commands inside the compositor, it loads plugins into the compositor process, and Void does not package it.
- sway refuses proprietary GPU drivers unless started with an override.

## Considered Options

- **A maintained Hyprland fork.** Rejected: Hyprland is built to be customized from outside, through a control socket, plugins, and protocols open to every client. A fork's standing job would be keeping those removed across a fast-moving upstream, in a large C++ codebase inside the trusted base. We would also package Hyprland and its own libraries for Void ourselves.
- **Upstream Hyprland, locked down by configuration.** Rejected: configuration cannot guarantee that a route stays closed, and ADR 0014 says a route that is not closed is a hole.
- **A compositor on wlroots, or sway or labwc as the base.** Rejected: the same rules can be built there, but in C, and the existing compositors offer the sensitive protocols to every client by default. Rust on Smithay matches the rest of the stack and starts from nothing offered.
- **niri or cosmic-comp, as shipped or forked.** Rejected. As shipped, each offers virtual input, clipboard access, and screen capture to every ordinary client, and neither has the confirm gesture or marked chrome, so meeting ADR 0014 means a fork. A fork would give a working desktop sooner: multi-monitor, scaling, screen sharing, and the X11 bridge already exist. We give that up for three reasons. Both are GPL-3.0 and CrowbarOS is MIT, so our trusted UI code would take that licence. Our changes cut across input, protocols, and window layout, so every upstream release means rework against a project with other goals. And a trusted part is easier to defend when it holds only what we put there. We read them to learn how Smithay is used. We do not copy from them.
- **Decide at Milestone 4.** Rejected: the shell would be designed against Hyprland and then reworked, and the Void lab needs a compositor for its graphical guest before then.

## Consequences

- We own a compositor. Smithay gives no window management, output management, or wlr-screencopy, and its only hardware renderer is GLES. Multi-monitor, scaling, session lock, screen sharing, text input for languages that need an input method, and accessibility are each ours to build or to leave out on purpose.
- Smithay has no stable release cadence. We pin a git commit, bump it on a schedule, and fix what each bump breaks.
- The filter that decides which client sees which protocol, and the way the compositor identifies the shell, are new security code. They get adversarial tests like the gate does.
- The apps stop talking to Hyprland. The `session.rs` adapters in `apps/shell` and `apps/studio-settings` move to an interface the compositor defines, which only the shell can reach. The Hyprland session files and the Fedora lab's Hyprland setup do not carry over.
- The look in [`DESIGN.md`](../../DESIGN.md) carries over: the apps draw their own content, and the bar, dock, launcher, and wallpaper stay layer surfaces. Effects the compositor would draw, such as blur or window animation, are ours to write if the design later asks for them.
- X11 apps run through a separate bridge process per app, or not at all. No X server is shared.
- NVIDIA is a cost, not a barrier. The proprietary driver needs a glibc Base and Void's nonfree repository. Known problems on Smithay compositors with NVIDIA are an open flicker bug and a freeze seen with the GPUI version we pin; the second may force a change of GPUI version.
- The compositor lives in [`compositor/`](../../compositor/README.md), as its own Cargo project outside the root workspace. [ADR 0027](0027-the-compositor-is-built-in-its-own-repository.md) moves it to a repository of its own, Fulcrum, and Base pins one commit.
- The Void lab's graphical guest runs this compositor. The Fedora lab is removed when it does.
- The compositor is no longer an open question in the vision.

## What would change this

The [spike](https://github.com/crowbaros/crowbaros/issues/3) tests this decision. We reopen it if:

- the shell's layer surfaces or GPUI's rendering cannot be made to work under a Smithay compositor;
- a client can still receive the confirm key, or still see a protocol it should not;
- the compositor cannot run on a Void guest without systemd.

[ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) records that none of the three is met after the spike and the NVIDIA run, so this decision stands. It also says what the first compositor has and leaves out.

Cost does not reopen it. This list once had a fourth condition: that the measured cost of the spike showed the full compositor to be beyond two people alongside Milestones 2 to 4. The spike could not measure that, and on November 15, 2026 we dropped the condition: we build this compositor whatever it costs. What can still change is what the first compositor has, and where its work sits among the milestones.
