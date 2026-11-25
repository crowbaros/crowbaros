# Lever drives the harnesses a person has, and CrowbarOS builds none

**Status:** proposed on November 23, 2026, by Jim Martin. It replaces [ADR 0034](0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md).

[ADR 0034](0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md) made Pi Durable the one Harness, with our TypeScript around it. Building Milestone 3 on it, we were writing again what the harnesses a person already uses have. Sign-ins were done. Fifteen of the milestone's twenty slices were still open, and among them were acts on files and commands, the rules for when to ask, web search, and coming back from a restart. A person who already works in Codex, Claude Code, or OpenCode would have kept their skills, tool servers, and habits in one place and the Agent in another.

These are the facts as read at their sources on November 23, 2026:

- **A harness can be driven by another program.** The [Agent Client Protocol](https://agentclientprotocol.com/) is JSON-RPC over a harness's standard input and output. Its first version is stable, it has an [official Rust library](https://github.com/agentclientprotocol/rust-sdk), and OpenCode, Gemini CLI, Cursor, GitHub Copilot, Goose, and Antigravity are among the harnesses that speak it themselves. [Codex](https://github.com/openai/codex) has `codex app-server`, and writes out the schema of its protocol for the version in hand. [Pi](https://github.com/earendil-works/pi) has `pi --mode rpc`. Claude Code documents [a mode that reads and writes JSON](https://code.claude.com/docs/en/headless). The messages that carry a request and stop a turn are not documented, and can be read in its [Python library](https://github.com/anthropics/claude-agent-sdk-python).
- **ADR 0034's reason against OpenCode no longer holds.** [OpenCode](https://github.com/anomalyco/opencode) is released as one executable for Linux, with its runtime inside, under the MIT licence.
- **No harness can use an app's window on Linux.** Codex's computer use is part of a desktop app for macOS and Windows, and Claude Code's is [for macOS alone](https://code.claude.com/docs/en/computer-use). OpenCode, Codex, and Claude Code will each call tools another program gives them through the Model Context Protocol. Pi takes such tools only through an extension written for it.
- **Claude Code's licence is its maker's own.** Anthropic's [terms](https://code.claude.com/docs/en/legal-and-compliance) allow an OS to install it beforehand only under an agreement with them. A person who installs it themselves needs none.

## The decision

- **CrowbarOS builds no harness.** The Agent works through the harnesses a person has. What a harness does well, CrowbarOS does not write again: its tools for files and commands, its modes, its requests, its subagents, and its sign-ins.
- **Lever is the service that drives them.** It is written in Rust. One runs on each machine, from boot, as the person's account, and it starts each harness as the person. Fulcrum, the shell, the root helper, and the Agent's clients speak Lever's protocol. No harness's name for a thing, type, or ID crosses it, so a harness can be added, upgraded, or dropped without changing them.
- **One adapter for each protocol, behind one contract.** An adapter says what its harness can do: its modes, whether it has subagents, whether a turn can be steered or picked up again. Lever offers the person that and no more, and never stands in for something a harness lacks. The first adapter speaks the Agent Client Protocol. Adapters for Codex's and Claude Code's own protocols follow, for what the Agent Client Protocol does not carry.
- **Lever keeps the record of every thread**, in a database of its own in the person's home: the messages, the acts, the requests, the subagents, and how each turn ended, in one form for every harness. A harness's own history is used only to pick its session up again. With the record, the person reads a thread whose harness is not running, a thread changes harness partway or moves to another machine, and work goes on after a restart.
- **A session started by hand is a thread too.** When the person runs a harness in a terminal, Lever finds the session in that harness's own store and lists it.
- **Base bundles OpenCode**, unchanged and pinned to an exact version, so the Agent works before the person installs another harness. An upgrade is its own pull request and runs the desktop check. Base bundles only a harness whose licence lets anyone pass it on. Claude Code is never bundled.
- **CrowbarOS holds no sign-in to a Model.** Each harness keeps its own, in its own store, and the person signs in as that harness has them do it.
- **Every harness gets CrowbarOS's tools the same way.** Lever serves them over the Model Context Protocol and gives them to each thread's harness: windows and workspaces, apps, settings, acts as root, captures, the Agent's pointer and keyboard, schedules, other threads, and other machines.
- **Modes and requests are the harness's.** A thread works in a mode of its harness's own, as the person set it, up to full access. Lever carries a harness's request to the Agent's surface, and the person answers it there. The glossary's Ask mode and Auto mode end, and CrowbarOS no longer judges which of a harness's acts need an ask.
- **Fulcrum draws the asks for what CrowbarOS guards**, and for nothing else. An act as root raises an ask in every mode, and the root helper acts only on Fulcrum's word. YOLO mode is one switch for the machine, apart from any thread's mode, and only a yes turns it on.
- **The Agent gets a pointer, a keyboard, and workspaces of its own.** A thread that opens a window gets a workspace, and input of its own to work there. It sees that workspace without a share, and the person can go there and work beside it. Input from the Agent's pointer and keyboard is never the person's, so it answers no ask. This reverses the Milestone 3 spec, which left input the Agent makes out of reach. The first version sees with captures and Fulcrum's list of windows.
- **The Agent's home is held once.** A folder in the person's home keeps their instructions, the Agent's skills, and its tool servers, sorted by where they apply: everywhere, one machine, one harness, or one project. Lever hands each harness what applies, in the form it reads. Lever keeps the folder the same across paired machines, and it is a git repository as well. Secrets are kept apart from it.
- **Each machine keeps its own threads, and machines pair directly.** A one-time code and a yes pair two machines, and no server of ours sits between them. From any machine the person sees and answers every thread, a thread hands work to a thread on another machine, and the person moves a thread. A paired machine's Fulcrum can carry the person's yes to another machine, and the person can pair without granting that. Lever's protocol and its record must not depend on the far machine running CrowbarOS.
- **A moved thread takes its record, and its project when it has one.** The harness on the new machine starts a new session and is given the thread so far, as when a thread changes harness. The thread then lives on the new machine alone.
- **Milestone 3 as specified ends.** No more of it is built on Pi Durable. What carries over stays: the summon gesture, the framing, Fulcrum's desktop context, the drawer's code in the shell, and `crowbar-agent` as a client of Lever. The [Milestone 3 spec](https://github.com/crowbaros/crowbaros/issues/291) and its open slices close as replaced when this record is accepted, and a new spec is written against the [vision](../VISION.md).

Later records settle how Fulcrum serves Lever and draws its asks, how it gives the Agent a pointer, a keyboard, and workspaces, how machines pair, and by what means a thread is taken back.

## Considered Options

- **Finish Milestone 3 on Pi Durable.** The Agent would act sooner. Rejected: most of what was left to build is what the harnesses already have, and it would be removed afterwards.
- **Our own harness in Rust**, which ADR 0034 kept open for later. Rejected: it is the same work again, in another language, and a person's own harnesses would still stand outside the Agent.
- **One harness, built in, of our choosing.** Rejected: a person has already chosen theirs, and signed in to it.
- **Each harness as an agent with its own name**, or one lead thread that starts the others. Rejected: the person has one Agent, and which harness answers is a setting of a thread.
- **The harness's own history as the only record.** Less to build. Rejected: reading, searching, changing harness, and moving a thread would each depend on every harness's own files.
- **The Agent Client Protocol alone.** One adapter for everything. Rejected: Codex and Claude Code reach it only through adapters that need Node.js, and lose whatever that protocol does not carry.
- **Bundling Pi.** Its sign-ins are proven on our hardware. Rejected: it takes tool servers only through an extension we would write and keep, and it has no requests.
- **Bundling Codex.** One static binary, in Rust. Rejected: it works best with one provider's accounts, and OpenCode signs in to many.
- **Bundling no harness.** The smallest Base. Rejected: the Agent would do nothing until the person installed one.
- **Fulcrum draws every request**, the harnesses' own included. One way to say yes. Rejected: a harness already runs as the person, so a card for each edit guards nothing and slows every thread.
- **The Agent takes the person's pointer and keyboard.** The least to build. Rejected: the person waits while the Agent works.
- **The Agent uses apps in a session apart**, nested or in a virtual machine. Rejected: the person could not step in beside it, and it could not help with a window the person has open.
- **Machines reach each other through an account and a relay of ours**, or over SSH alone. Rejected: the first means running a service and holding accounts, and the second needs SSH set up on every machine before any two can pair.

## Consequences

- [ADR 0034](0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md) is replaced, and [ADR 0035](0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md), which was never accepted, is withdrawn.
- The TypeScript in `agent/`, the Node.js that Base pins, the `Harness` job in CI, and the stub Model the checks use go when Lever takes their place. Until then `main` keeps the Harness as it is, and it gains nothing more. The glossary's **Harness** now means any harness, so the documents beside that code use the word in its old sense until they go.
- A harness is a program we did not write, running as the person. One the person installed changes on its maker's schedule, and a change to its protocol breaks its adapter until we follow. The messages Lever needs from Claude Code are not documented, so that adapter is the one most likely to break.
- Any program running as the person can read each harness's stored sign-ins, as it could read the Harness's before.
- What is sent to a model provider is each harness's doing. The vision's question of what leaves the machine now covers every harness the person installs.
- Fulcrum gains a second pointer and keyboard, workspaces it draws while they are off screen, and captures. Its plan had one seat and drew no hidden workspace. This is the largest new work here.
- Lever's record and a harness's own history can differ. What the person sees is Lever's.
- The fixed rules ADR 0034 gave every conversation are gone with Ask mode and Auto mode. What stands between a Model and the machine is the harness's mode, and CrowbarOS's own guard on what its tools do.
