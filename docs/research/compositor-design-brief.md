# Compositor design brief

**What this is.** The input to the design session for the real compositor. It
lists what the spike settled, what is still unknown, and the decisions the
session has to make, in the order they depend on each other. It decides
nothing. Decisions go into ADRs.

**Written:** November 15, 2026, after the
[spike](compositor-spike-2026-11-15.md) and three rounds of review by three
models. Each claim below is from that record or its "Council review" section
unless it names another source.

**Read first:** [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md) (the
rules), [ADR 0015](../adr/0015-our-own-compositor-on-smithay.md) (the
decision), and the [compositor research](compositor-smithay.md) (what Smithay
gives and does not give).

## Where ADR 0015 stands

It stands provisionally. These are its four conditions for reopening:

| Condition | Status |
| --- | --- |
| The shell's layer surfaces or GPUI's rendering cannot be made to work | Not met nested, on NVIDIA. **Not tested on DRM with a GPU.** [Issue 5](https://github.com/crowbaros/crowbaros/issues/5) |
| A client can still receive the confirm key, or see a protocol it should not | Not met in what was tested. Two leaks of the key were found and closed |
| The compositor cannot run on a Void guest without systemd | Not met. It runs |
| The cost is beyond two people alongside Milestones 2 to 4 | **Not answered.** The spike measured an agent's hours, which says nothing about this |

The session should not plan past the first row before issue 5 has run. It is
the only run that can still reopen the decision on hardware.

## What the spike settled

Build on these. Each has a test or a log behind it.

| Finding | What it means for the design |
| --- | --- |
| Smithay's key filter leaks the confirm key: a release with no press, and a new keyboard object being told the key is down | The compositor takes the key before Smithay's keyboard sees it. The filter that ADR 0015 names is not the mechanism |
| One default-deny table decides every global for every client, applied through blanket dispatch impls | Keep this shape. A global with no entry is offered to nobody |
| A client's kind can be decided by how its connection was made: a socket pair for the shell, the public socket, a security-context listener | It works inside Wayland. It is only as strong as what keeps processes apart (decision 1) |
| Smithay's `Space` arranges layer surfaces and exclusive zones | The bar, dock, launcher, and wallpaper stay layer surfaces, as `DESIGN.md` assumes |
| GPUI 0.2.2 works over a socket handed to it, and did not freeze on NVIDIA when nested | The shell needs no GPUI change to be started by the compositor |
| seatd under runit, with no systemd, gives the compositor its seat, devices, and hotplug | Base needs seatd and the packages the record lists. Void's libseat also links `libelogind` |
| The account that runs clients cannot open `/dev/uinput` or the input devices in the Void guest | True of Void's defaults today. Base does not yet set or check it |

**What carries over as working parts:** the in-process test harness and probe
client (`compositor/tests/`, `src/probe.rs`), the graphical Void guest
(`lab/void/compositor_guest.py`), and the three checks that need a display.
They test behaviour, so they can be pointed at a new compositor.

**What does not:** the compositor's code. It is `smallvil` plus about 1,400
lines, written to answer questions. The issue calls it throwaway, and two of
the three first-round reviewers said outright to keep it as a reference and
not build on it.

## What is still unknown

- **A GPU drawing through this compositor.** No such frame has reached a
  display. Issue 5 covers NVIDIA through DRM, Void's `nvidia` package, explicit
  sync, the GPUI apps on that session, and suspend and resume.
- **Cost.** There is no estimate of the real compositor.
- **A program started from the shell.** The shell's sockets cannot be
  inherited, but no program was launched to see it arrive as an ordinary
  client. On Base it would not start at all: the shell launches through
  `systemd-run`.
- **The real shell under input.** No click or key was sent to the real shell.
  The launcher and the two focus requests are covered by tests on both sides
  only.
- **Real hardware.** Everything ran nested or in a VM: no laptop, no second
  GPU, no unplugged display, no VT switch.

## Decisions for the session

In dependency order. The first two shape the rest and each wants its own ADR.

### 1. The process model

**The question.** What keeps one principal's process from reaching another's,
or the shell's, outside Wayland?

**What the spike showed.** The compositor identifies the shell by its
connection, and every client by how it connected. In the Void guest the
compositor, the shell, and every client ran as one user. A process of that
user that may trace the shell can copy its sockets and become the shell. The
shell's own `control.sock` takes `quit` from any process of that user. The
only thing in the way of tracing was the kernel's default `ptrace_scope` of 1.

**To decide:**

- Does each principal run as its own user, in a sandbox, or both?
- Who makes a principal's Wayland connection: the broker, the compositor, or
  the principal itself? Today anything that reaches the public socket is an
  ordinary client, and confinement is something a launcher opts into.
- Is there a public socket at all? If so, should it give the smallest set?
- Who may register a security context? Today every ordinary client may.
- Who holds the `_seatd` group? Today everything the compositor starts
  inherits it.
- What does Base set and check: `ptrace_scope`, who may open `/dev/uinput`,
  the input device modes?

**Depends on.** Milestone 2 builds task principals in Linux sandboxes and the
broker. This decision should be made with that work, not after it.

### 2. The control interface and the confirm

**The question.** How do the shell and the compositor talk, and how does one
press of the confirm gesture answer exactly one ask?

**What the spike showed.** A JSON-lines channel over a socket the compositor
hands to the shell. It has no request ids and no way to reconnect, and the
reviews found three defects in it. The compositor takes the chord
whether or not an ask is showing. The event it sends names no ask, and the
shell's code never receives it.

**To decide:**

- The transport. A private Wayland protocol would need no second socket, but
  GPUI 0.2.2 gives the shell no way to bind a global of its own.
- Request ids, events read as they arrive, and what happens when the shell
  restarts.
- How a confirm is tied to one ask. ADR 0014 rule 2 wants a delay after the
  ask appears, so the compositor has to know when the ask became visible and
  topmost. Should the compositor own that check and the rate limits, with the
  shell only drawing?
- What the gesture is, and what happens to it when no ask is showing.
- One keyboard was assumed. With two, a release can reach a client with no
  press before it.

### 3. Trusted UI: stacking and failure

**The question.** What does the compositor guarantee about the trusted UI when
a client, or the shell itself, misbehaves?

**What the spike showed.** Layer-shell goes to the shell alone and an
exclusive Top or Overlay layer takes the keyboard. The compositor draws no
marking of its own, does not watch or restart the shell, and draws no cursor
or drag surfaces from clients.

**To decide:**

- ADR 0015 says the compositor draws the marking of a preview and of an ask
  itself. What is that marking, and what stops a client imitating it?
- Cursor and drag-icon surfaces are a client's pixels and usually sit above
  every layer. Where do they go?
- When the shell dies or stalls during an ask: the compositor should refuse
  the confirm gesture. What does the person see, and who restarts the shell?
- Session lock, and what happens to apps when the compositor itself dies.

### 4. Focus, the clipboard, and capture

**The question.** Who may read what the person types, copies, and sees?

**What the spike showed.** A new window takes keyboard focus as soon as it
opens. Focus is what offers the clipboard's contents, so a window that opens
itself can read the clipboard with no paste by the person, and gets the keys
typed next. The clipboard-manager and capture protocols are denied to all.

**To decide:**

- A focus policy that does not hand a new window the keyboard unasked.
- How a paste needs the person's say. This does not fit the core clipboard
  protocol and may need changes inside Smithay's selection code. Is the
  powerbox the answer?
- How the shell itself captures the screen for previews and for point and say.

### 5. Features that collide with ADR 0014

Each of these needs a route that rule 1 closes. For each, the session picks:
build it as a trusted part, or leave it out on purpose and say so.

| Feature | The route it needs |
| --- | --- |
| Text input for languages that need an input method | The input-method protocol, which the table denies |
| Screen readers and other assistive software | The accessibility bus. The vision's open question on driving native apps is the same problem |
| Screen sharing in a call | A portal and capture |
| X11 apps | An X server. ADR 0015 allows one bridge per app, or none |

### 6. The desktop's scope

**The question.** What is the smallest desktop the first real compositor
supports? This bounds the cost more than anything else.

**What the spike showed.** Nine workspaces, floating windows, maximize, two
outputs at different scales, and one hotplug. Nothing else.

**To decide:** window layout; output configuration and where it is stored;
fractional scaling; touch and tablets; animations and blur, which `DESIGN.md`
may ask for; and where settings such as reduced motion live now that Studio
Settings cannot call `hyprctl`.

### 7. Hardware

**To decide, after issue 5:** which GPUs are supported at first; explicit sync
(`linux-drm-syncobj-v1`), which the research says NVIDIA needs and the spike
does not offer; laptops with two GPUs; suspend and resume; VT switching; and
the cost in power of redrawing while idle.

### 8. Living with Smithay

**What the spike showed.** The default-deny table rests on dispatch traits
that were on Smithay's `master` only days before the spike. The spike pins one
commit.

**To decide:** how often the pin moves, and who fixes what a bump breaks.
Measure one bump before estimating.

### 9. Cost and order

**To decide, last:**

- A list of what the first compositor must have and what it leaves out on
  purpose, from decisions 3 to 7.
- An estimate against that list. niri's and cosmic-comp's histories are the
  nearest comparison.
- Where the work sits among Milestones 2 to 4. The vision puts the shell and
  the confirm gesture in Milestone 4, and the process model in Milestone 2.
- The first slices. The reviewers' advice was security first, each with tests
  that attack it: client identity, the global table, the confirm, then
  stacking, focus, and the clipboard.

## What the session should produce

1. An ADR for the process model (decision 1).
2. An ADR for the control interface and the confirm (decision 2).
3. A scope for the first compositor: what it has, and what it leaves out on
   purpose (decisions 3 to 7).
4. An estimate, and the compositor's place among the milestones (decision 9).
5. Issues for the first slices.

---

Work done by Claude Fable 5.2 in the Claude Code harness (through T3 Code).
