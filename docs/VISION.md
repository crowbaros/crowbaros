# CrowbarOS: vision

**Status:** draft by Jim Martin, November 17, 2026, for Jason Alexander's review.
It replaces the [substrate vision](archive/substrate-vision/README.md), which
replaced the [Fedora blueprint](archive/fedora-blueprint/README.md). The decision
is [ADR 0030](adr/0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md).
What the earlier plans taught us is in [LESSONS.md](LESSONS.md). Terms are in
the [glossary](../GLOSSARY.md).

## What CrowbarOS is

CrowbarOS is a small operating system on Void Linux with an agent built into it.

A person installs it and gets a bare desktop: our own compositor, a shell, a
handful of default apps, and the Agent. The Agent is not an app on the
desktop. It is part of the OS, the way the shell and the compositor are.

The Agent is a partner at the person's side. The person can reach it from
anywhere, at any time, with one gesture, and ask it for help with anything the
OS can do. Until they do, it stays out of the way.

From there each person grows CrowbarOS into what they need, with the Agent's
help or by hand.

## What a person gets at install

| Part | What it is |
| --- | --- |
| Base | Void Linux, with runit and xbps, kept small |
| Compositor | Ours: a Wayland compositor named [Fulcrum](../compositor/README.md), written from the ground up with Smithay as its reference ([ADR 0015](adr/0015-our-own-compositor-on-smithay.md), [ADR 0028](adr/0028-the-compositor-is-written-from-the-ground-up.md), [ADR 0031](adr/0031-the-compositor-is-built-in-this-repository-again.md)) |
| Shell | The bar, launcher, dock, and wallpaper, from Jason Alexander's [design system](../DESIGN.md) and [shell](../apps/shell/README.md) |
| Default apps | A handful. [Studio Settings](../apps/studio-settings/README.md) is the first. Which others is open |
| The Agent | Running from boot, one gesture away |

Nothing else is installed.

## The Agent

1. **Always reachable.** One gesture brings the Agent up over whatever is on
   screen, in any app, on any workspace, and the same gesture puts it away. The
   compositor owns that gesture, so no app can take it or block it.
2. **Part of the OS.** The Agent runs as a system service from boot, not in an
   app's window. Its surface is drawn by the compositor, above everything else.
   It can do what the OS can do: open and arrange windows, change settings,
   install and remove software, work with the person's files, and run commands.
3. **Not intrusive.** The Agent speaks when it is spoken to. It does not pop
   up, interrupt, or look at the screen unless the person asks.
4. **The person stays in charge.** The Agent acts with the person's
   permissions and never more. Before it does something that cannot be taken
   back, it shows what it will do and waits for the person's yes. A yes comes
   from the person's own hand, and the compositor can tell it apart from input
   an app or the Agent made.

## What we build first

Each milestone ends in something a person can use.

| Milestone | Scope |
| --- | --- |
| **1. Base boots** | Plain Void Linux, built without root, boots under UEFI. Jason Alexander started this in [`base/`](../base/) |
| **2. A desktop** | Our compositor runs the shell and Studio Settings on Base, in the VM and on real hardware |
| **3. The Agent at hand** | The person summons the Agent from anywhere, asks, and it answers and acts on the desktop |
| **4. Installable** | An image a person installs on their own machine, with the default apps. The release process gets an ADR before it is published |

The substrate vision's security architecture, with its broker, gate, labels,
and compartments, is not part of this plan. It stays in the archive, and the
[lessons](LESSONS.md) say what to reach for when the Agent's reach makes a real
risk.

## Open questions

- **What the Agent sees.** When the person summons it, what can it see without
  asking: the focused window, the selection, the screen? How does the person
  share more, or less?
- **The gesture.** Which key, and whether voice comes later.
- **The harness.** Do we write our own, or run an existing harness as the OS's
  harness? Which models, local or in the cloud, and who chooses?
- **Life without a model.** What does the desktop do when no model is
  reachable?
- **The default apps.** A terminal, a browser, files, a text editor? Which,
  and whose?
- **Acting in other apps.** How does the Agent work in an app it did not write?
  Through an accessibility interface, the compositor, or not at all?
- **Taking it back.** How does a person undo what the Agent did? Base's root is
  Btrfs, so snapshots are one answer.
- **What leaves the machine.** Where conversations and the Agent's memory live,
  and what is sent to a model provider.
