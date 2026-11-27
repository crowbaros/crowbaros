# `crowbar-agent`

The person's way to the Agent from any terminal: foot, a console, or SSH. It is
the first client of [Lever](../README.md)'s person socket, and speaks
[its thread channel](../README.md#messages). Lever drives the harness and keeps
the record of every thread. The client only relays: it sends what the person
types, writes the answer on standard output as it comes, and shows acts and
status on standard error.

## Using it

```sh
crowbar-agent                          # talk with the Agent, a line at a time
crowbar-agent "<request>"              # one turn, and out
crowbar-agent --list                   # list unarchived threads
crowbar-agent --archive ID             # hide a thread, retaining its record
crowbar-agent --delete ID              # delete a stopped thread and its session
crowbar-agent --resume ID              # go on with a thread
crowbar-agent --resume ID "<request>"  # one more turn of it, and out
crowbar-agent --resume ID --model M    # set a thread's Model
crowbar-agent --model M "<request>"    # start a thread with Model M
crowbar-agent --resume ID --effort E   # set a thread's effort
crowbar-agent --resume ID --mode MODE  # set a thread's mode
crowbar-agent --mode MODE "<request>"  # start a thread in a mode
crowbar-agent --resume ID --answer R A # answer a thread's request R with A
crowbar-agent --resume ID --unqueue Q  # take back message Q that waits in a thread
crowbar-agent --stop ID                # stop the working turn of a thread
crowbar-agent models                   # list the Models the harness offers
crowbar-agent efforts M                # list the effort levels of Model M
crowbar-agent commands                 # list the slash commands the harness offers
crowbar-agent free-models [on|off]     # show or switch OpenCode's free hosted Models
crowbar-agent default-mode [MODE]      # show or set the mode new threads start in
crowbar-agent login                    # sign in to a provider with OpenCode
crowbar-agent status                   # Lever's status, what works, and what waits
```

- **Choosing Models before the first turn.** Until the person chooses, Lever
  starts OpenCode with its free hosted Models switched off, and a message with
  no signed-in provider is refused with the reason. Running `crowbar-agent`
  interactively with no Models available offers the choice: enable OpenCode's
  free hosted Models, or run `opencode auth login`. `crowbar-agent free-models
  on|off` changes that choice later.
- **Sign-in stays with OpenCode.** `crowbar-agent login` runs `opencode auth
  login` directly in the terminal and, on success, asks Lever to refresh its
  Models. Lever holds no credential and relays none.
- **Listing and selecting Models.** `crowbar-agent models` lists the Models the
  harness offers, marking the default. `--model M` sets a thread's Model (alone
  with `--resume ID`, or alongside a message).
- **Effort.** `crowbar-agent efforts M` lists the effort levels of a Model,
  and says when it has none. `--resume ID --effort E` sets a thread's effort
  from its next turn. A new thread starts at its Model's own effort: the
  client sets one only on a thread that exists.
- **The harness's slash commands.** `crowbar-agent commands` lists the slash
  commands the harness offers, each as it is typed, with the harness's
  description. One is
  sent as a request that begins with `/` and its name, as
  `crowbar-agent "/review"`, or at the prompt. The harness takes it for a
  command or for words: the client and Lever only pass it on.

- **A request waits for the person.** In Ask first, a thread asks before it
  runs a command or edits a file. The request goes to standard error with the
  exact command, or the file and the change as lines that begin `-` and `+`.
  On a terminal, the client asks for the answer there, from those the request
  offers (`once`, `always`, `no`). Down a pipe, it says how to answer from
  another terminal: `crowbar-agent --resume ID --answer R once`. The request
  waits with no time limit, Ctrl-C stops the turn and withdraws it, and
  `--list` shows its thread as `(waiting for you)`. Requests that wait at once
  are asked for one at a time, each when Lever has the answer to the one
  before. Those that wait when a thread is resumed are asked again, and one
  that comes while the prompt waits, as after its turn, is asked for there.
  A turn that ends while a request waits leaves it to be answered first, and
  a turn another client begins meanwhile is shown. Ctrl-C there turns down
  the request asked for by stopping its own turn, says how to answer any
  other that waits outside that turn, and brings the prompt back, after a
  turn being followed ends. Otherwise the prompt comes back once nothing
  waits.
- **Modes.** A thread is in `ask-first`, `full-access`, or `plan`, and keeps
  it. New threads start in `ask-first` until `crowbar-agent default-mode
  full-access` changes that. `--mode` starts a thread in another mode, or,
  with `--resume ID` alone, changes a thread's mode for its next turn.
- **The answer is plain text on standard output**, a piece at a time, so it
  reads the same in a terminal and down a pipe. Acts (`[act] ...`), subagents
  (`[subagent] ...`), requests (`[request] ...`), unrecognized updates
  (`[update] ...`), prompts, and errors go to standard error. `--list` notes
  when a thread has subagents, `--resume ID` shows them with their acts and
  answers, and subagents that outlive their turn keep the watch open so their
  requests can be answered right away (or with `--resume ID --answer R A`).
  Ctrl-C then stops them, with exit status 130, and Ctrl-C while a later
  turn works stops that turn and then them. A turn the harness began
  itself after a subagent reported begins with `[the Agent went on after a
  subagent reported]` in place of a message: the line-at-a-time mode follows
  it, and `--resume ID` shows it.
  An act that ends after a later turn began is still shown. On a
  terminal, an act that comes in the middle of a line of the answer ends that
  line first. Down a pipe or into a file, nothing is put in the middle of the
  answer. Either way the answer's last line is ended, as any command's is.
- **Nothing a harness says can drive the terminal.** Control characters in
  answers, acts, notes, and errors are shown as their Unicode pictures (`␛` for
  escape) and do nothing. Line ends and tabs stay, and carriage returns are
  dropped.
- **A message while the Agent works.** `crowbar-agent --resume ID
  "<request>"` sent while the thread works on a turn is taken into that turn
  where its harness can steer one: the client says so on standard error, and
  writes the rest of the turn's answer, with the message as `> <request>` where
  the Agent took it. Every client that follows the turn shows it there, and
  `--resume ID` shows it there in the record. Where the harness cannot take a
  message into a turn under way, or did not take this one, the message waits
  and goes as its own turn when the turns before it end, and the client
  follows that turn. Ctrl-C while it waits takes it back, as `crowbar-agent
  --resume ID --unqueue Q` does from another terminal. A client that follows
  the thread shows it as `[waiting] <request>`, with how to take it back, and
  a Stop of the turn before it drops it: `[not sent] <request>`, here and in
  `--resume ID`. The line-at-a-time mode takes no message while a turn works.
- **A thread starts with its first message**, in the terminal's current working
  directory. `crowbar-agent` with no argument asks for a line, sends it, writes
  the answer, and asks again. Ctrl-D leaves, and stops nothing: a turn another
  client began, or one that was working when the thread was resumed, goes on.
- **Ctrl-C stops the turn** the Agent is working on, and the client waits for
  the turn to end (a second Ctrl-C leaves immediately). Between turns, Ctrl-C
  leaves. `--stop ID` asks Lever to stop a working turn from another terminal,
  and does not wait for it: `--list` and `--resume ID` show how the turn ended.
- **A client that goes away stops nothing.** When its terminal closes or its
  SSH connection drops, the turn goes on in Lever. `--list` shows it as
  `(working)`, `--resume ID` picks it up, and `--stop ID` or Ctrl-C stops it.
- **`status`, `models`, `efforts`, `commands`, `free-models`, `default-mode`,
  and `login` are commands only as the first word.** A request that starts with one goes in quotes with the rest of
  it, or after `--`: `crowbar-agent -- status of my disk`.

| Exit status | When |
| --- | --- |
| 0 | The turn was answered, or the command was carried out |
| 1 | The Agent gave no answer, the turn was interrupted, no harness is available, Lever refused the request, or Lever is not reachable |
| 2 | The command line asks for nothing the client does |
| 130 | Ctrl-C stopped the turn or left while waiting on Lever |

## Finding Lever

The client connects to `crowbar-lever/person` in the person's runtime
directory: `$XDG_RUNTIME_DIR`, or `/run/user/<user ID>` where that is not set,
as on a console or over SSH. It never starts Lever or a harness.

## Build and test

From `lever/`:

```sh
cargo test --locked
```

`src/` holds unit tests of the command line and framing. `tests/lever.rs` runs
`crowbar-agent` against `crowbar-lever` and `crowbar-stub-harness` over a real
person socket, and verifies every message documented in `lever/README.md`.

| Path | Contents |
| --- | --- |
| `src/main.rs` | The command line |
| `src/connection.rs` | The connection, and waiting on Lever, the terminal, and Ctrl-C at once |
| `src/talk.rs` | Turns, threads, acts, listing, stopping, and status |
| `src/sys.rs` | Kernel calls std does not offer, and all of the client's `unsafe` code |

Archive hides a thread from `--list` while keeping its record readable with
`--resume ID`. Delete requires the thread to finish or stop first. It removes
Lever's record and requests harness session deletion; when the harness cannot
delete sessions, the client prints that its copy may remain.
