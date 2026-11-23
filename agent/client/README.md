# `crowbar-agent`

The person's way to the Agent from any terminal: foot, a console, or SSH. It
is the first client of the [Harness](../README.md)'s person socket, and
speaks [its messages](../README.md#the-person-sockets-messages). The Harness
holds the conversations, reaches the Model, and keeps the credentials. The
client only relays: it sends what the person types, and writes the answer as
it comes.

Pi's own `pi` runs its agent in its own process and cannot attach to a
running Pi Durable, so this client is ours, in Rust
([ADR 0034](../../docs/adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)).
A full-screen interface is
[#326](https://github.com/crowbaros/crowbaros/issues/326).

## Using it

```sh
crowbar-agent                         # talk with the Agent, a line at a time
crowbar-agent "<request>"             # one turn, and out
crowbar-agent --list                  # list the conversations
crowbar-agent --resume ID             # go on with a conversation
crowbar-agent --resume ID "<request>" # one more turn of it, and out
crowbar-agent login [PROVIDER]        # sign in to a Model
crowbar-agent logout [PROVIDER]       # sign out
crowbar-agent status                  # the Model the Agent uses, and each sign-in
crowbar-agent model [PROVIDER/MODEL]  # list the Models signed in to, or choose one
```

- **The answer is plain text on standard output**, a piece at a time, so it
  reads the same in a terminal and down a pipe. Everything else the client
  says goes to standard error.
- **A conversation starts with its first message.** `crowbar-agent` with no
  argument asks for a line, sends it, writes the answer, and asks again.
  Ctrl-D leaves. `--resume ID` shows the conversation's newest turns first.
- **Ctrl-C stops the turn** the Agent is working on, and the client waits for
  the turn to end. Between turns, Ctrl-C leaves.
- **A client that goes away stops nothing.** When its terminal closes or its
  SSH connection drops, the turn goes on in the Harness. `--list` shows it as
  working, and `--resume ID` picks it up, where Ctrl-C stops it.
- **`login`** asks which provider, how to sign in, and which of its Models
  the Agent is to use. Every question is the Harness's. With an API key, the
  terminal does not show the key as it is typed. With OAuth, the client opens
  a browser when it runs on a desktop (`WAYLAND_DISPLAY` or `DISPLAY` is set),
  with `xdg-open` or `gio open`, and prints the address either way. Where the
  browser is on another machine, paste the code it shows, or the address it
  ends at.
- **`login`, `logout`, `status`, and `model` are commands only as the first
  word.** A request that starts with one of them goes in quotes with the rest
  of it, or after `--`: `crowbar-agent -- status of my disk`.

| Exit status | When |
| --- | --- |
| 0 | The turn was answered, or the command was carried out |
| 1 | The Model gave no answer, no Model is signed in, the Harness refused the request, or no Harness answers. `status` also exits 1 with no Model in use |
| 2 | The command line asks for nothing the client does |
| 130 | Ctrl-C stopped the turn, or cancelled the sign-in |

## Finding the Harness

The client connects to `crowbar-agent/person` in the person's runtime
directory: `$XDG_RUNTIME_DIR`, or `/run/user/<user ID>` where that is not set,
as on a console or over SSH. It never starts a Harness. Where none answers,
it says so and exits. In Base, runit runs the Harness from boot. On a
developer's machine, [`npm start` in `agent/`](../README.md#on-a-developers-machine)
runs one.

## Build and test

```sh
cargo test --locked                          # the tests that need no Harness
cargo test --locked -- --include-ignored     # all of them
base/void_base.py agent                      # the Harness's tests, then all of these, as CI runs them
```

`src/` holds unit tests of the command line, the framing, and the sign-in's
choices. `tests/harness.rs` runs `crowbar-agent` against the Harness itself:
it starts the bundle Base ships, `../dist/harness.mjs`, as one process, with
[the stub Model](../README.md#the-stub-model) for a Model, and drives a turn,
a conversation, a list, a resume, a stop, a sign-in, and a refused peer over
the real person socket. Those tests need the bundle and Node.js
(`CROWBAR_AGENT_NODE` names it, or `node` on `PATH`), which Base's build root
does not have. So they are marked ignored: Base's build runs the rest in its
build root, and `base/void_base.py agent` runs all of them with the pinned
Node.js. CI's `Harness` job runs that command, after `cargo fmt --check` and
`cargo clippy`.

Base builds the client in its Void build root with the desktop, so it links
Void's own libraries, and installs it at `/usr/local/bin/crowbar-agent`.
Base's boot check runs one turn with it as the person, against the stub
Model.

| Path | Contents |
| --- | --- |
| `src/main.rs` | The command line |
| `src/session.rs` | The connection, and waiting on the Harness, the terminal, and Ctrl-C at once |
| `src/talk.rs` | A turn, a conversation, and the list |
| `src/sign_in.rs` | Signing in and out, and the Model in use |
| `src/sys.rs` | The kernel calls std does not offer, and all of the client's `unsafe` code |
