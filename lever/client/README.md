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
crowbar-agent --list                   # list the threads
crowbar-agent --resume ID              # go on with a thread
crowbar-agent --resume ID "<request>"  # one more turn of it, and out
crowbar-agent --stop ID                # stop the working turn of a thread
crowbar-agent status                   # Lever's status and available harnesses
```

- **The answer is plain text on standard output**, a piece at a time, so it
  reads the same in a terminal and down a pipe. Acts (`[act] ...`), unrecognized
  updates (`[update] ...`), prompts, and errors go to standard error. On a
  terminal, an act that comes in the middle of a line of the answer ends that
  line first. Down a pipe or into a file, nothing is put in the middle of the
  answer. Either way the answer's last line is ended, as any command's is.
- **Nothing a harness says can drive the terminal.** Control characters in
  answers, acts, notes, and errors are shown as their Unicode pictures (`␛` for
  escape) and do nothing. Line ends and tabs stay, and carriage returns are
  dropped.
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
- **`status` is a command only as the first word.** A request that starts with
  it goes in quotes with the rest of it, or after `--`:
  `crowbar-agent -- status of my disk`.

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
