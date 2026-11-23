# CrowbarOS: vision

**Status:** accepted on November 18, 2026, by Jim Martin and Jason Alexander.
It replaces the [substrate vision](archive/substrate-vision/README.md), which
replaced the [Fedora blueprint](archive/fedora-blueprint/README.md). The decision
is [ADR 0030](adr/0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md).
Terms are in the [glossary](../GLOSSARY.md).

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
| Default apps | A handful. [Studio Settings](../apps/studio-settings/README.md) is the first, and [Files](../apps/files/README.md), our own file manager, the second. Which others is open |
| The Agent | Running from boot, one gesture away |

Nothing else is installed.

## The Agent

1. **Always reachable.** One gesture, Super+Space, brings the Agent up over
   whatever is on screen, in any app, on any workspace, and the same gesture
   puts it away. The compositor owns that gesture, so no app can take it or
   block it.
2. **Part of the OS.** The Agent runs as a system service from boot, not in an
   app's window. Its surface is drawn by the shell, and every ask by the
   compositor, above everything else.
   The person can also talk with it from any terminal, at a console or over
   SSH: the terminal is a client of the same service, with the same
   conversations.
   It can do what the OS can do: open and arrange windows, change settings,
   install and remove software, work with the person's files, and run commands.
3. **Not intrusive.** The Agent speaks when it is spoken to. It does not pop
   up, interrupt, or look at the screen unless the person asks. With each
   message it is given the desktop context: the windows' titles and apps and
   where they are, never what they hold. The person asks it to look with a
   share, of the clipboard or a capture, for once or as an allowance for one
   conversation. A mark shows whenever the Agent is working, looking, or
   waiting for the person.
4. **The person stays in charge.** The Agent acts with the person's
   permissions. Before it does something as root, CrowbarOS makes it show what
   it will do and wait for the person's yes. Before anything else it judges
   cannot be taken back, the Agent does the same. The person chooses a mode:
   Ask, Auto, or YOLO. A yes comes from the person's own hand, and the
   compositor can tell it apart from input an app or the Agent made.

## What we build first

Each milestone ends in something a person can use.

| Milestone | Scope |
| --- | --- |
| **1. Base boots** | Plain Void Linux, built without root, boots under UEFI. Jason Alexander started this in [`base/`](../base/) |
| **2. A desktop** | Our compositor runs the shell and Studio Settings on Base, in the VM and on real hardware |
| **3. The Agent at hand** | The person summons the Agent from anywhere, asks, and it answers and acts on the desktop |
| **4. Installable** | An image a person installs on their own machine, with the default apps. The release process gets an ADR before it is published |

The substrate vision's security architecture, with its broker, gate, labels,
and compartments, is not part of this plan. It stays in the archive. If the
Agent's reach makes a real risk, a new record decides what to do about it.

## Open questions

- **Voice.** Whether the person can summon the Agent and speak to it by voice.
- **Models.** Whether a model can run on the machine itself, and on what
  hardware. The rest is decided. The harness is Pi Durable, behind a protocol
  CrowbarOS owns
  ([ADR 0034](adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)).
  The person chooses the model: they sign in to a provider in the cloud,
  with an API key or OAuth, and pick one of its models
  ([the Harness](../agent/README.md#reaching-a-model)).
- **Life without a model.** What does the desktop do when no model is
  reachable?
- **The default apps.** Files is our own. A terminal, a browser, a text
  editor? Which, and whose?
- **Acting in other apps.** How does the Agent work in an app it did not write?
  Through an accessibility interface, the compositor, or not at all?
- **Taking it back.** How does a person undo what the Agent did? Base's root is
  Btrfs, so snapshots are one answer.
- **What leaves the machine.** Where conversations and the Agent's memory live,
  and what is sent to a model provider.
