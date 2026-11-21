# Fulcrum draws every ask and holds what the Agent may take

**Status:** proposed on November 21, 2026, by Jim Martin.

[ADR 0032](0032-fulcrum-serves-the-shell-and-the-persons-apps.md) settled what Fulcrum offers the shell and the apps, and left what it offers the Agent to Milestone 3. [ADR 0034](0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md) made Pi Durable the Harness, running as the person's account: any program running as the person can pose as it. Fulcrum listens and the Harness connects. The vision needs three things here: the compositor owns the summon gesture, a yes comes from the person's own hand, and the Agent looks at the screen only when the person asks. The glossary has the rest: the summon gesture, the Agent's surface, the mark, shares, allowances, asks, the yes, the modes, and the root helper.

Research for [#232](https://github.com/crowbaros/crowbaros/issues/232) compared the ways Fulcrum could capture the screen for the Agent. Its findings are on the `research/fulcrum-screen-capture` branch.

## The decision

### Who draws what

- **The shell draws the Agent's surface.** The drawer is a layer surface of the shell, on its trusted socket. The shell holds the drawer's state. Fulcrum takes Super+Space and tells the shell. The shell then shows the drawer, hides it, or moves the keyboard by asking Fulcrum to.
- **Fulcrum draws every ask.** While the drawer is up and an ask is waiting, Fulcrum draws a card across the foot of the drawer, above every surface, the shell's included. It shows the exact act, the count of asks waiting, and Yes and No buttons. Pointer input on the card goes to Fulcrum alone. While the card shows and the keyboard is in the drawer, Fulcrum takes Escape as a no and the confirm chord as a yes. With the drawer put away, no card is drawn, and the mark shows that an ask waits.
- **A long act scrolls.** The card grows to the drawer's height, then scrolls. A yes counts only once the act's end has been in view, unchanged and uncovered, for half a second. Until then the chord does nothing and the Yes button shows as not ready.
- **Fulcrum renders the card's text with cosmic-text.** The text is shaped, so every script shows, and nothing is escaped. Its fonts are IBM Plex Mono, Manrope, and a Noto fallback. Base installs them in a directory for Fulcrum alone, and Fulcrum reads no other fonts.
- **The shell draws the mark**, in the bar and on an overlay surface over a fullscreen window. Working and done come from the Harness. Waiting, looking, and YOLO mode come from Fulcrum, which holds the asks, takes the captures, and keeps the mode.
- **Fulcrum draws the outline** around what it captured, as it draws window frames.

### Fulcrum's channels

- **The Harness gets a socket of its own**, checked against the person's user ID. Each message is a 4-byte length followed by one JSON message, and the first frame states the protocol version. A new connection replaces the one before it.
- **The shell's control channel moves to the same framing**, so Fulcrum has one wire format.
- **What the Harness may ask of Fulcrum:**
  - the snapshot of windows, workspaces, and displays, which is also the desktop context;
  - focusing a window or a workspace, and moving a window to a workspace;
  - closing a window, as the person's own close does;
  - setting a window floating or tiled, and moving or resizing a floating window;
  - reading and changing the layout of the displays;
  - raising and withdrawing asks;
  - a capture;
  - the clipboard.
- **A display change by the Agent reverts after 15 seconds** unless the person keeps it with a Keep button. Fulcrum draws Keep, and verifies the click as it does a yes. The Agent cannot keep the change itself.
- **Fulcrum starts no apps for the Harness.** The Harness starts them itself, as the person, at the apps' socket.
- **A capture is one request** for a display or a window. It is answered with the pixels in a sealed memfd, and leaves out the drawer, the mark, and the ask card. No client is offered a Wayland capture protocol.
- **The clipboard is one request**, answered with a file descriptor holding the selection.

### Asks, shares, YOLO mode, and root

- **Fulcrum holds the asks, the shares, the allowances, and YOLO mode**, in memory alone. The Harness raises its parked asks again each time it connects. They show again in order, and each waits its half second afresh.
- **A share is the person's click in the drawer.** The shell reports it, and Fulcrum counts it only if Fulcrum saw a real pointer make it. Fulcrum records a share or an allowance against the conversation it is for. A capture or a clipboard request is refused without one, unless YOLO mode is on.
- **Turning YOLO mode on raises an ask that Fulcrum creates itself.** The shell reports the person's click on the switch. Turning the mode off needs no yes.
- **The root helper asks Fulcrum** over a socket checked against root's user ID: is there a yes for this ask and these exact bytes, or is YOLO mode on? The first check that matches spends the yes, and nothing else spends it. The Harness talks to the helper directly, and the act's output goes from the helper to the Harness, never through Fulcrum.
- **A no or a stopped turn removes the ask.**
- **While the desktop is locked,** Fulcrum refuses captures and the clipboard, YOLO mode included, and draws no ask card. Window acts, and root acts that need no yes, go on.
- **When the Harness's connection drops,** every share and allowance ends. YOLO mode stays on, since the person's yes turned it on.

## Considered Options

- **The shell draws the asks too**, from text Fulcrum or the Harness sends it, and Fulcrum checks where the click landed. Rejected: Fulcrum's word to the root helper would rest on text another process drew.
- **A program of its own for the Agent's surface**, with its own account and socket. Rejected: a fourth account and a second copy of the shell's plumbing, guarding against nothing the shell does not already guard against.
- **Fulcrum draws the whole drawer.** Rejected: the compositor would need text input, layout, and the design system.
- **Fulcrum holds the drawer's state** and moves the keyboard itself. Rejected in favour of keeping the drawer's behaviour in the shell with the rest of its interface.
- **A small rasterizer with no shaping, escaping control, bidi, and invisible characters**, so that an act always looks like its bytes. Rejected: every script should show as written. The cost is under Consequences.
- **A standard Wayland capture protocol** (`ext-image-copy-capture-v1`) for a trusted client. Rejected: nine interfaces across three staging protocols, and the Harness is not a Wayland client.
- **The shell takes captures.** Rejected: under an allowance, background work would need the shell for each look.
- **Fulcrum carries out acts as root**, or hands the Harness a signed token. Rejected: Fulcrum would carry command output, or share a key with the helper.
- **The Agent's display changes need no guard**, or need an ask. Rejected: a bad mode can leave the person no screen to answer on, and an ask for every change is too much for a setting.

## Consequences

- Fulcrum gains a text renderer and three bundled fonts, its first dependencies beyond its own crates and rustix. The test that guards what Fulcrum links lists them.
- A Model that is wrong or misled can raise an ask whose text looks unlike its bytes, through a bidi override, an invisible character, or a look-alike letter. The person's eye is the only guard.
- A yes still rests on what Fulcrum drew, and the root helper still acts only on Fulcrum's word. A program posing as the Harness can raise asks and act on windows, but cannot answer an ask.
- A crash or upgrade of the Harness ends every allowance. The glossary's **Allowance** says so.
- When Fulcrum restarts, YOLO mode ends, as the glossary has it, and every ask shows again once the Harness reconnects.
- The vision's sentence that the compositor draws the Agent's surface changes: the shell draws the surface, and the compositor draws every ask.
- What each channel carries, message by message, is settled in the Milestone 3 spec. How the shell's account reaches the Harness's socket is [#238](https://github.com/crowbaros/crowbaros/issues/238)'s.
