# CrowbarOS: vision

**Status:** accepted on November 24, 2026, by Jim Martin and Jason Alexander.
Proposed on November 23 by Jim Martin. It changes the vision they accepted on
November 18, 2026: what the Agent is made of, the milestones after the second,
and the open questions. That vision replaced the [substrate vision](archive/substrate-vision/README.md),
which replaced the [Fedora blueprint](archive/fedora-blueprint/README.md). The
decisions are [ADR 0030](adr/0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md)
and [ADR 0036](adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md).
Terms are in the [glossary](../GLOSSARY.md).

## What CrowbarOS is

CrowbarOS is a small operating system on Void Linux with an agent built into it.

A person installs it and gets a bare desktop: our own compositor, a shell, a
handful of default apps, and the Agent. The Agent is not an app on the
desktop. It is part of the OS, the way the shell and the compositor are.

The Agent is a partner at the person's side. The person can reach it from
anywhere, at any time, with one gesture, and ask it for help with anything the
OS can do. Until they do, it stays out of the way.

CrowbarOS does not build the program that runs a model. The Agent works
through the harnesses a person already has, such as OpenCode, Codex, Claude
Code, and Pi. CrowbarOS gives every one of them the OS to work with: its
windows, its settings, its software, a pointer and a keyboard of the Agent's
own, and the person's other machines. The OS is the Agent's home, whichever
harness is at work.

From there each person grows CrowbarOS into what they need, with the Agent's
help or by hand.

## What a person gets at install

| Part | What it is |
| --- | --- |
| Base | Void Linux, with runit and xbps, kept small |
| Compositor | Ours: a Wayland compositor named [Fulcrum](../compositor/README.md), written from the ground up with Smithay as its reference ([ADR 0015](adr/0015-our-own-compositor-on-smithay.md), [ADR 0028](adr/0028-the-compositor-is-written-from-the-ground-up.md), [ADR 0031](adr/0031-the-compositor-is-built-in-this-repository-again.md)) |
| Shell | The bar, launcher, dock, and wallpaper, from Jason Alexander's [design system](../DESIGN.md) and [shell](../apps/shell/README.md) |
| Default apps | A handful. [Studio Settings](../apps/studio-settings/README.md) is the first, and [Files](../apps/files/README.md), our own file manager, the second. [Editor](../apps/editor/README.md) opens text, Markdown, and configuration files. [Monitor](../apps/monitor/README.md) shows resource use, processes, services, and storage |
| The Agent | Running from boot, one gesture away: Lever, the service that drives the harnesses, and one harness, OpenCode, so the Agent works before the person installs another |

Nothing else is installed.

## The Agent

1. **Always reachable.** One gesture, Super+Space, brings the Agent up over
   whatever is on screen, in any app, on any workspace, and the same gesture
   puts it away. The compositor owns that gesture, so no app can take it or
   block it.
2. **Part of the OS.** Lever runs as a system service from boot, not in an
   app's window. The shell draws the Agent's surface, and the compositor draws
   every ask, above everything else.
   The person can also talk with the Agent from any terminal, at a console or
   over SSH: the terminal is a client of the same service, with the same
   threads.
   The Agent can do what the OS can do: open and arrange windows, change
   settings, install and remove software, work with the person's files, and
   run commands.
3. **One Agent, many threads.** A thread is one piece of work. Each has a
   harness and a Model, which the person picks as they pick anything else
   about it, and many threads work at once. A thread needs no project: it
   starts as talk, and becomes a project when the work needs a place, with a
   folder or a git repository the Agent sets up.
   The Agent's surface is a drawer. It lists the threads and shows the one in
   front, and where a thread has subagents the person can look into each.
   Pinned, the drawer stays, and the windows are laid out beside it.
   A person starts a thread from the drawer, from a terminal, from a
   schedule, or from a thing on the desktop: a window, a file, or a
   selection.
4. **The harnesses a person has.** Lever drives each harness through that
   harness's own protocol, and offers the person what it can do and no more.
   The harnesses bring their own tools, modes, subagents, and sign-ins, and
   CrowbarOS builds none of these again. A session the person starts by hand,
   by running a harness in a terminal, shows up as a thread too.
   Lever keeps its own record of every thread, so the person can read one
   whose harness is not running, and a thread can change harness partway.
5. **Not intrusive.** The Agent's surface never comes up on its own. The
   Agent may work unseen on what the person set up: a long thread, or a
   schedule. A mark shows whenever it is working, looking, or waiting for the
   person, and that is how the person finds out.
   It does not look at the person's windows unless the person asks. A message
   sent from the drawer carries the desktop context: the windows' titles and
   apps and where they are, never what they hold. The person asks the Agent
   to look with a share, of the clipboard or a capture, for once or as an
   allowance for one thread.
6. **The person stays in charge.** The Agent acts with the person's
   permissions. Each thread works in a mode of its harness's own, as the
   person set it, from one that asks before every act to full access, which
   asks nothing for what the person's account can do. A harness's request is
   answered in the Agent's surface.
   What the OS guards is not the harness's to ask about. Before the Agent
   acts as root, in any mode, CrowbarOS makes it show what it will do and
   wait for the person's yes. A yes comes from the person's own hand, and the
   compositor can tell it apart from input an app or the Agent made. One
   switch for the whole machine, YOLO mode, lets acts as root through without
   an ask, and only a yes turns it on.
   A person can take a thread back to before any of its turns.
7. **Hands of its own.** The Agent can use any app, as a person does. A
   thread that opens a window gets a workspace of its own, and a pointer and
   a keyboard of its own to work there, so the Agent and the person never
   reach for the same one. The Agent sees its own workspace without a share.
   The person can go there at any time, watch, and work in it beside the
   Agent: to sign in where the Agent is stuck, or to show it the way.
   The Agent sees and uses the person's own windows only when the person
   asks, under a share.
8. **One home.** What the person has given the Agent is kept once, in the
   Agent's home: their instructions, its skills, and its tool servers, sorted
   by where they apply, which is everywhere, one machine, one harness, or one
   project. Lever hands each harness what applies to it, in the form that
   harness reads, with a guide to the machine it is on. Secrets are kept
   apart from the folder. The Agent's home is the same on every machine the
   person has paired, and it is a git repository too, for its history and for
   a machine that can pull but cannot pair.
9. **Every machine the person owns.** Each machine runs its own Lever and
   keeps its own threads. The person pairs two machines with a one-time code,
   and they reach each other directly, with no server of ours between them.
   From any one of them the person sees every thread on every machine,
   answers it, and starts a thread elsewhere. A thread can hand work to a
   thread on another machine, and the person can move a thread to another
   machine. An ask raised on a machine with no display can be answered at the
   machine the person is sitting at.
   The fleet is CrowbarOS machines first. Nothing in it may rule out another
   Linux desktop, then macOS, then Windows.

## What we build first

Each milestone ends in something a person can use.

| Milestone | Scope |
| --- | --- |
| **1. Base boots** | Plain Void Linux, built without root, boots under UEFI. Jason Alexander started this in [`base/`](../base/) |
| **2. A desktop** | Our compositor runs the shell and Studio Settings on Base, in the VM and on real hardware |
| **3. The Agent at hand** | The person summons the Agent from anywhere, starts a thread, and it answers and acts as the person. Lever, the drawer with its list of threads, and OpenCode, driven through the Agent Client Protocol. A session started in a terminal shows up as a thread |
| **4. The Agent acts on the OS** | Every harness gets CrowbarOS's tools: windows and workspaces, apps, settings, and acts as root behind an ask. Codex and Claude Code are driven through their own protocols. The Agent's home. A thread starts from a schedule, or from a window, a file, or a selection |
| **5. Installable** | An image a person installs on their own machine, with the default apps. The release process gets an ADR before it is published |
| **6. Hands of its own** | Captures, the Agent's pointer and keyboard, and a workspace for each thread that needs one |
| **7. The fleet** | Pairing, every machine's threads in one list, handing work to another machine, and moving a thread |

The substrate vision's security architecture, with its broker, gate, labels,
and compartments, is not part of this plan. It stays in the archive. If the
Agent's reach makes a real risk, a new record decides what to do about it.

## Open questions

- **Voice.** Whether the person can summon the Agent and speak to it by voice.
- **Models.** Whether CrowbarOS helps a person run a model on the machine
  itself, and on what hardware. The rest is decided: each harness reaches the
  Models its person signed it in to, and CrowbarOS holds no sign-in
  ([ADR 0036](adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md)).
- **Life without a model.** What does the desktop do when no model is
  reachable?
- **The default apps.** Files is our own. A terminal, a browser, a text
  editor? Which, and whose?
- **What the Agent remembers.** Whether one memory serves every harness,
  where it lives, and who may read it.
- **The accessibility interface.** The Agent first sees an app as a picture.
  When do Fulcrum and our apps offer the tree of an app's controls, which a
  screen reader needs as well?
- **Taking it back.** By what means does a person take a thread back, and
  what can never be taken back? Base's root is Btrfs, so snapshots are one
  answer, and git is another inside a project.
- **What starts a thread.** A schedule and a thing on the desktop are
  decided. Something that happens on the machine, or a call from outside it?
- **Away from the desk.** Whether the person can watch and answer threads
  from a phone or a browser.
- **What leaves the machine.** Where threads and the Agent's memory live,
  and what each harness sends to a model provider.
