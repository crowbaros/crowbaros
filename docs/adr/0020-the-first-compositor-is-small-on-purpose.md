# The first production compositor is a small floating desktop, and what it leaves out is on purpose

**Status:** accepted on November 15, 2026, by Jim Martin, at the end of the design session for [issue 66](https://github.com/crowbaros/crowbaros/issues/66).

[ADR 0015](0015-our-own-compositor-on-smithay.md) says CrowbarOS owns its compositor. [ADR 0016](0016-the-warden-starts-every-compartment.md) says who its clients are, and [ADR 0018](0018-one-gesture-answers-one-ask.md) says how it carries a yes. The [design brief](../research/compositor-design-brief.md) left open what desktop the first real compositor is: decisions 3 to 9. This record answers them. "First" means the compositor that [issue 21](https://github.com/crowbaros/crowbaros/issues/21) builds for the Beta.

Each thing left out is left out on purpose and listed below, so that nobody reads a gap as an oversight and fills it with a route [ADR 0014](0014-a-yes-comes-from-the-person.md) closes.

## ADR 0015 stands

None of its three conditions for reopening is met.

| Condition | Result |
| --- | --- |
| The shell's layer surfaces or GPUI's rendering cannot be made to work | Not met. They worked nested, in the Void guest, and on an NVIDIA GPU through DRM, with suspend and resume ([NVIDIA record](../research/compositor-nvidia-2026-11-15.md)) |
| A client can still receive the confirm key, or see a protocol it should not | Not met in what was tested. Two leaks of the key were found and closed ([spike record](../research/compositor-spike-2026-11-15.md)) |
| The compositor cannot run on a Void guest without systemd | Not met. It runs |

The NVIDIA record covers one GPU, one driver, and GPUI clients. It does not show explicit sync, other clients, other drivers, or hotplug. Those are work for the hardware rows below, and none of them is a reason to reopen the decision.

## Windows and workspaces

- **Windows float.** The person moves and resizes a window with the pointer or the keyboard, maximizes and restores it, makes it fullscreen, and snaps it to a half of the display.
- **There are nine workspaces, and a workspace spans every display.** Switching workspace switches all displays together, and the bar shows the same nine on each.
- **Tiling and scrolling layouts are left out.** The brand kit names them as optional layouts. They can be added later without changing anything here.

## Frames

A **frame** is what the compositor draws around a window or an ask, which no client can draw or cover.

- **Every window has a frame.** It is a thin line around the window, and its colour shows which window has the keyboard.
- **A Preview and the window of a wide compartment carry a label tab on the frame.** The tabs are a fixed set of images built into the compositor. The compositor has no font renderer and lays out no text.
- **A fullscreen window keeps its frame and its tab.** They are drawn inside the edge of the display. So a fullscreen window that draws a picture of the desktop, of an ask, or of the lock screen still carries a frame that the real ones do not.
- **Apps draw their own title bars.** The compositor draws no title bar and no window buttons. The shell's bar names the principal that owns the focused window.
- **Nothing in the Environment can change a frame** (ADR 0014, rule 5). Its colours and shapes are fixed in Base. This record fixes what is drawn and where. The design work under [issue 22](https://github.com/crowbaros/crowbaros/issues/22) decides how a frame, a tab, and the frame of an ask look.
- **The compositor learns that a window is a Preview, or belongs to a wide compartment, through the shell's interface** (ADR 0016). [Issue 144](https://github.com/crowbaros/crowbaros/issues/144) still decides how a wide compartment's ask is shown, and the attacks its tests must refuse.

## What is drawn above what

From the bottom to the top:

1. The wallpaper.
2. Windows, each with its frame.
3. The bar and the dock. A fullscreen window covers these two, and nothing else the shell draws.
4. The launcher and the shell's other overlays.
5. Cursor images and drag icons. They are a client's pixels, and the compositor clips them so that they cover no frame.
6. While an ask is showing: the dim, the ask, and its frame (ADR 0018).
7. While the session is locked: the lock screen, and nothing else.

A client reaches only the second level and the fifth. A popup or a subsurface is clipped to the display, and it cannot cover another window's frame.

## Focus

- **A new window takes the keyboard in two cases only.** It is the first window of a compartment the person has just started from the shell, or its parent window has the keyboard.
- **Any other new window opens without the keyboard.** The bar shows that it wants attention.
- **Focus moves on a click or on one of the compositor's keys.** It never moves because the pointer passed over a window.
- **A client cannot ask for focus.** The activation protocol is offered to nobody.

## The compositor's keys

- **Every chord with the Super key belongs to the compositor.** It is never sent to a client. Window moves, workspace switches, the launcher, and lock all use Super.
- **The set is fixed for the Beta.** There is no rebinding, and no protocol that lets a client hold back the compositor's keys.
- The confirm chord is separate, and ADR 0018 governs it.

## The clipboard

- **Copy and paste inside one compartment work as apps expect.**
- **Between compartments, the compositor makes the transfer itself.** It holds what was offered, and moves the bytes from the source to the destination only when the person presses a paste key in the destination's window. The read must follow the key press within a short, fixed time.
- **The paste keys are Ctrl+V, Ctrl+Shift+V, and Shift+Insert.** The compositor still delivers them to the app.
- **A read with no paste key gets nothing.** That includes a paste from a right-click menu, which the compositor cannot tell from an app reading the clipboard unasked. The bar then shows a hint that names the source and says to press the paste key.
- **The compositor tells the broker the source and the destination of each paste**, so the pasted value carries its source's label and mark ([ADR 0019](0019-the-origin-table-fails-closed.md)), and the destination compartment is then at that label, as with any value it reads.
- **Left out:** clipboard managers, the primary selection (middle-click paste), and drag and drop between compartments. Drag and drop inside one compartment works.

## Capture

- **Only the shell reads pixels.** It asks through its interface to the compositor, for a Preview, for point and say, and for the person's own screenshots.
- **No capture protocol is offered to any client.**
- **Screen sharing in a call is left out.** A call in the Beta can send the camera and the microphone, and not the screen. [Issue 112](https://github.com/crowbaros/crowbaros/issues/112) adds it later as a share of one window that the person picks in the powerbox.

## Lock

1. **Lock is a state of the compositor.** The shell's request, the idle timer, suspend, and one of the compositor's keys each enter it.
2. **While locked, the compositor shows the shell's lock screen and nothing else.** Every other display is blank, and input goes to the lock screen alone.
3. **Only the broker ends it.** The broker tells the compositor that the person authenticated. The shell's word is not enough, so a bug in the shell cannot unlock the session.
4. **The compositor starts locked**, at boot and after a crash. Apps do not survive a crash of the compositor.
5. **Unlock begins with the confirm chord.** Until the person presses Ctrl+Alt+Enter, the lock screen takes no text.
6. **The chord always reaches the trusted UI.** What it does depends on what is showing:

| State | What the chord does |
| --- | --- |
| An armed ask is showing | Answers it (ADR 0018) |
| An ask is showing and is not armed | Nothing (ADR 0018) |
| The session is locked | Opens the unlock entry |
| No ask is showing, and the session is unlocked | Locks the session |

The last row is what makes rule 5 hold. The compositor withholds Enter, and not Ctrl or Alt, so a focused app can guess that the chord was pressed. A fullscreen app that imitates the lock screen could show its own field for the secret at that moment. Because the chord locks an unlocked session, the real lock screen covers the imitation as soon as the person presses it.

This changes two sentences of ADR 0018. It says the chord does nothing with no armed ask: with no ask showing at all, it now locks the session. It says the gesture does nothing while the session is locked: that still holds for asks, and the chord's one job on the lock screen is to open the unlock entry.

What the secret is, and how the person sets it, belongs to issues [87](https://github.com/crowbaros/crowbaros/issues/87) and [100](https://github.com/crowbaros/crowbaros/issues/100).

## Displays and input devices

- **Several displays**, attached and removed while the session runs.
- **A chosen mode and place for each display.** The compositor takes the monitor's preferred mode until the person chooses another.
- **Scale from 1 to 3, in steps of 0.25**, set for each display.
- **The layout is machine state.** The compositor holds it, keyed by the monitor's identity, and changes it only through the shell's interface. It belongs to one machine, so it is not in the Crowbar State.
- **Keyboards, mice, and touchpads**, on one seat.
- **Left out:** mirroring, rotation, variable refresh, HDR, colour management, night light, touchscreens, tablets, and a second seat.

## Effects

- **There are none.** No window animation, blur, shadow, or rounded-corner clipping. [`DESIGN.md`](../../DESIGN.md) asks for none, and Settings already says this desktop has no animations. The dim behind an ask is a flat overlay.
- **The compositor draws clients' cursor images**, and Base ships a cursor theme.
- **The compositor redraws only when something changed.** The spike redrew on a timer.

## What each client is offered

One default-deny table decides every global for every client, as in the spike. A global with no row is offered to nobody, and that includes one a later Smithay adds.

| Offered to | Protocols |
| --- | --- |
| Every compartment with a Wayland listener | The core protocols, `xdg-shell`, the data device (under the clipboard rules above), viewporter, fractional scale, cursor shape, presentation time, idle inhibit |
| A compartment whose needs include the GPU | `linux-dmabuf` and explicit sync (`linux-drm-syncobj-v1`) |
| The shell alone | Layer shell |
| The Warden alone | The security context manager |
| Nobody | Activation, primary selection, data control, every capture protocol, virtual keyboard, virtual pointer, input method, text input, session lock, foreign toplevel lists, shortcut inhibit, pointer constraints, relative pointer, tablets, DRM leasing, the XWayland shell |

**Idle inhibit is honoured only for a fullscreen window that has the keyboard.** A film does not lock halfway through, and an app in the background cannot keep the session unlocked.

## Left out on purpose

| Left out | Why, and who owns it |
| --- | --- |
| Text input through an input method | The input-method route is one ADR 0014 closes. Compose and dead keys work. Chinese, Japanese, and Korean text waits for a trusted input method in the shell: issues [63](https://github.com/crowbaros/crowbaros/issues/63) and [124](https://github.com/crowbaros/crowbaros/issues/124) |
| A screen reader across apps | An accessibility bus exists only inside one compartment (ADR 0016). Scale, contrast, and reduced motion are the whole of the Beta's offer: issues 63 and 124 |
| X11 | The compositor never holds X11 code. A bridge for one app is an ordinary client inside that app's compartment, and issue 63 decides whether to package one |
| Screen sharing, clipboard managers, primary selection, drag and drop between compartments | See above. Issue 112 |
| Games that capture the pointer | Pointer constraints are offered to nobody |
| Tiling and scrolling layouts, a workspace set for each display | Not needed for a first desktop |
| Animation, blur, shadows, rounded corners | The design asks for none |
| Mirroring, rotation, variable refresh, HDR, colour management, night light, touchscreens, tablets, a second seat | Not needed for a first desktop |
| VT switching | There is no login account (ADR 0016), so a getty is a way around the lock. Owner authority has its own route: [issue 139](https://github.com/crowbaros/crowbaros/issues/139) |
| Apps that survive a compositor crash | The compositor starts locked, with no clients |
| Rebinding the compositor's keys | The set is fixed for the Beta |

## Hardware

Mesa is the open-source set of graphics drivers that ships with Linux. It drives Intel, AMD, and most ARM GPUs. NVIDIA's own driver is separate and comes from Void's nonfree repository.

| Class | Machine | Status |
| --- | --- | --- |
| The Void lab guest, with software rendering | Any | Supported. Checked in every pull request that touches the compositor |
| One NVIDIA GPU, on Void's `nvidia` driver 560.35.3 or later | Jim's tower, an RTX 4090 on 595.112.07 | Supported, and tested |
| One Intel GPU, on Mesa | Jason's workstation, Raptor Lake-S integrated graphics | To be qualified before the Beta |
| A laptop with an ARM GPU, on Mesa | Jim's Surface Laptop, a Snapdragon on aarch64 | The compositor alone is qualified on it, before the Beta: the lid, suspend, the touchpad, and fractional scale on a dense panel |
| Laptops with two GPUs, nouveau, NVIDIA drivers before 560.35.3, and AMD GPUs until one of us has one | None | Excluded |

- **The Surface qualifies the compositor, not CrowbarOS.** The compositor is built and run on the Linux the Surface already has. Base on aarch64 needs its own build and a kernel for that laptop. The Beta does not promise it, and [issue 53](https://github.com/crowbaros/crowbaros/issues/53) decides it.
- **Explicit sync is built**, in [issue 79](https://github.com/crowbaros/crowbaros/issues/79). The GPUI apps did not need it on NVIDIA. Installed software that draws through EGL may.
- **Idle power is a requirement.** A still desktop draws no frames.
- **A row is supported when its checks have run on that machine and their logs are in the repository.** Nothing else is promised.

## The Smithay pin

A [measurement](../research/compositor-smithay-bump-2026-11-15.md) ported the compositor across 198 days and 245 commits of Smithay's `master`. Three causes broke the build, the fix was 31 lines in 4 files, and no logic in the protocol table or the confirm chord changed.

- **The pin moves at least once a month, and sooner whenever either of us wants.** It moves at once for a Smithay fix that affects security.
- **A bump is a pull request of its own.** It changes the pin and what the bump breaks, and nothing else.
- **Whoever opens the bump fixes what it breaks.** Jim's side owns it by default.
- **Every bump runs** the compositor's tests, the Void guest's compositor checks, and the TTY check on the NVIDIA machine.
- **A test guards what is linked.** It fails if the compositor's binary gains libei, XWayland, or X11-backend code. The list of crates no longer shows this: Smithay depends on the `reis` crate without a feature gate, so the crate is built even though none of its code is linked.
- **The first real bump gets a short record** of what broke, because the measurement is a proxy.

## Where the work sits, and its order

- **The production compositor is Beta work.** It starts after the security rules are proven ([issue 17](https://github.com/crowbaros/crowbaros/issues/17)), because Milestone 2 can still change what the compositor must enforce, and two people cannot do both at once. In the vision's terms it runs beside Milestones 1b and 3, and it is done before Milestone 4's shell work, which waits on it.
- **It starts clean in `compositor/`.** The first slice moves the spike to `lab/compositor-spike/`. The Alpha desktop check runs the spike until the production compositor passes the same check, and then the spike is deleted. The spike's tests, probe client, and display checks move to the production compositor.

The slices, in order. Each lands with the attacks below that test it.

| Slice | Issue | What it builds |
| --- | --- | --- |
| 1 | [49](https://github.com/crowbaros/crowbaros/issues/49) | A clean skeleton, client identity through a stand-in for the Warden, the protocol table, redraw on change only, and the guard on what is linked |
| 2 | [71](https://github.com/crowbaros/crowbaros/issues/71) | The shell protocol, arming an ask, the chord, and the restart notice (ADR 0018) |
| 3 | [78](https://github.com/crowbaros/crowbaros/issues/78), narrowed | Frames, what is drawn above what, focus, floating windows, workspaces, and the compositor's keys |
| 4 | A new issue, split from 78 | The clipboard and capture |
| 5 | [79](https://github.com/crowbaros/crowbaros/issues/79) | Displays, scale, input devices, explicit sync, and the hardware rows. It can run beside slices 2 to 4 |
| 6 | [87](https://github.com/crowbaros/crowbaros/issues/87) | Lock and recovery from failure |

A second new issue holds the recurring bump of the Smithay pin.

## Attacks the tests must refuse

Each of these is a test, and each must be refused. They add to the attacks in ADRs 0016 and 0018. The slice that owns each is in brackets.

1. A client asks for every global in the "Nobody" row, and for one the table has no row for. None is offered. [1]
2. A window opens with no launch behind it while the person types in another window. It gets no key and no clipboard offer. [3]
3. A compartment the person started a minute ago opens a second window with no parent. It does not take the keyboard. [3]
4. A window draws a picture of a frame and a label tab inside itself. A fullscreen window draws a picture of the desktop, of an ask, and of the lock screen. The real frame is still drawn at the edge of the display in each case. [3]
5. A client puts a popup, a subsurface, a cursor image, and a drag icon over another window's frame, over the bar, and over an ask. None covers them. [3]
6. A focused client waits for a Super chord. It sees neither the press nor the release of the chord's key. [3]
7. A client in another compartment reads the clipboard with no paste key: when it gains focus, on a timer, and from a menu click. It gets nothing, and the hint shows. [4]
8. The person presses a paste key in one window. A window of another compartment reads the clipboard in the same moment. It gets nothing. [4]
9. A drag that starts in one compartment is dropped in another. Nothing is delivered. [4]
10. A peer that is not the shell asks the compositor's interface for pixels. It is refused on its user ID. [4]
11. A window that is not fullscreen, and a fullscreen window without the keyboard, each inhibit idle. The session still locks. [6]
12. While the session is locked, a client maps a window, makes it fullscreen, and opens a popup. Nothing is shown, and it gets no input. [6]
13. The shell tells the compositor that the session is unlocked, with no word from the broker. It stays locked. [6]
14. The shell is killed while the session is locked. It stays locked, and no app gets the keyboard. [6]
15. The compositor is killed. It starts again locked, with no clients. [6]
16. Text is typed on the lock screen before the chord. None of it reaches the unlock entry. [6]
17. A fullscreen window imitates the lock screen, and the person presses the chord. The session locks, and the real lock screen covers the window. [6]
18. A display is removed and attached again, with a window on it and an ask showing on it. No window is lost, and the ask is disarmed until it is steady again. [5]

## Considered Options

- **A tiling or scrolling layout first.** Rejected: the brand kit's default is floating windows, and one layout is enough to test.
- **A workspace set for each display.** Rejected for now: it needs a bar that differs on each display, and more cases for where an ask and a new window go.
- **A title bar on every window, drawn by the compositor, that names the principal.** Rejected: GTK apps draw their own and would have two, and it puts a font renderer in the program that holds the seat. It names the owner more clearly than the bar does, and it is the design to return to if the frame and the bar prove too little.
- **No frame on a fullscreen window.** Rejected: fullscreen is where an imitation of trusted UI is easiest.
- **Focus for every new window, as other desktops do.** Rejected: focus is what offers the clipboard and the next keys typed.
- **The clipboard as apps expect it, with safe focus alone.** Rejected: the focused app could read every copy with no paste by the person, and the broker could not label what crossed.
- **A paste through the powerbox every time.** Rejected: a paste is too common an act for a picker. The paste key in the destination's window is the person's say.
- **A change inside Smithay's selection code.** Rejected: the compositor making the transfer itself needs none.
- **A lock screen the compositor draws.** Rejected: it needs text entry, and so text layout, in the compositor. The shell draws it, and the compositor and the broker hold the rules.
- **Unlock on the shell's word.** Rejected: the largest trusted program would be able to unlock the session.
- **Unlock with no chord first.** Rejected: a fullscreen app that imitates the lock screen could collect the secret.
- **A chord that does nothing when no ask is showing, as ADR 0018 first said.** Rejected: an imitation of the lock screen can guess the chord from Ctrl and Alt, and nothing real would answer it. The frame on a fullscreen window is a second defence, and it rests on the person noticing a thin line.
- **An input method, a shared accessibility bus, or a shared X server in the first compositor.** Rejected: each is a route ADR 0014 closes, and each needs a trusted part that does not exist yet.
- **Pointer constraints for games.** Rejected for now: a window that holds the pointer is one the person may not be able to leave.
- **Support for every GPU that Mesa drives.** Rejected: one success on one machine does not qualify another. A row needs a machine one of us has.
- **Base on aarch64 for the Beta, because the Surface is an ARM machine.** Rejected: it is a larger job than the compositor, and it is issue 53's to decide.
- **A VT switch to a text console.** Rejected: there is no login account to protect it.
- **Follow Smithay's `master` with no pin, or wait for a release.** Rejected: an unpinned build cannot be repeated, and the last release is from June 2025 and lacks the dispatch traits the protocol table rests on.
- **Build on the spike's code.** Rejected: it was written to answer questions, and the reviewers said to keep it as a reference.
- **Start the compositor before Milestone 2 is done.** Rejected: see above.

## Consequences

- Issues 49, 71, 78, 79, and 87 build to this record. Issue 78 is narrowed, and two issues are new.
- The glossary gains **trusted UI**, **shell**, and **frame**. "Marking" is no longer used for what the compositor draws, because a **mark** is already the record that a value came from open untrusted input.
- The shell gains work: the bar names the focused window's principal, shows that a window wants attention, and shows the paste hint. It draws the lock screen and reports nothing about unlock.
- The broker gains one message from the compositor, the source and destination of each paste, and sends it one: that the person authenticated.
- An app that reads the clipboard when it gains focus, as some password managers and terminals do, gets nothing from another compartment.
- A person who uses an input method, a screen reader, a touchscreen, or screen sharing cannot use the Beta as their only desktop. The Beta's cohort is chosen with that known ([issue 51](https://github.com/crowbaros/crowbaros/issues/51)).
- A chord pressed just after an ask expired or was withdrawn locks the session, and the person unlocks again.
- The compositor has no font renderer. Every word it shows is an image built into it.
- The design brief's decisions are all answered: 1 by ADR 0016, 2 by ADR 0018, and 3 to 9 here.
