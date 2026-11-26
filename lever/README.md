# Lever

Lever (`crowbar-lever`) is the CrowbarOS service that drives the person's
harnesses and keeps the record of every thread
([ADR 0036](../docs/adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md)).
One runs on each machine from boot as the person's account, in the launcher's
environment for apps, supervised by runit (`crowbar-lever`).

Lever listens on its surface and person sockets, verifies peer identity with
`SO_PEERCRED`, speaks Agent Client Protocol (ACP) version 1 over a child
harness's standard input and output using its own JSON-RPC 2.0 loop and the
pinned `agent-client-protocol-schema` crate (`=1.22.0`), keeps every thread in
its private SQLite database, and runs many threads at once on one harness
process. Lever starts the harness on demand when a thread needs it and stops it
after 10 minutes with no thread working and no request waiting.

This workspace also holds:

- `crowbar-stub-harness` (`src/bin/stub_harness.rs`): a scripted ACP v1 stub
  harness in Rust for Lever's and `crowbar-agent`'s tests.
- [`client/`](client/README.md) (`crowbar-agent`): the person's terminal client
  of Lever's person socket.

## Build and test

From this directory (builds and tests both `crowbar-lever` and `crowbar-agent`):

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Invocation

```sh
crowbar-lever \
  --state <dir> \
  --surface-socket <path> --shell-uid <uid> \
  --person-socket <path> --person-uid <uid> \
  [--fulcrum-socket <path>] \
  [--harness <program> [--harness-arg <arg>]... [--harness-env <KEY=VALUE>]... [--harness-name <name>]] \
  [--idle-timeout-ms <ms>] [--stop-timeout-ms <ms>]
```

On Base:

- `--state` is `$HOME/.local/state/crowbaros/lever`
- `--surface-socket` is `/run/crowbar-lever/surface`, for `crowbar-shell`'s UID
- `--person-socket` is `/run/user/1003/crowbar-lever/person`, for the person's UID
- `--fulcrum-socket` is `/run/crowbar-trusted/agent/compositor`, Fulcrum's agent socket for desktop context
- `--harness` is `/usr/local/bin/opencode` with `--harness-arg acp` and
  `--harness-name OpenCode`

When `--harness` is omitted, Lever runs
with no harness configured and refuses `send` with `no-harness`. When
`--harness` is given, Lever starts the harness child only when a thread or a
Model query needs it and stops it after `--idle-timeout-ms` (default `600000`,
10 minutes) with no working thread, waiting request, or pending Model query. The harness child
runs in Lever's environment (on Base, the launcher's environment for apps) with
OpenCode's self-update turned off and the Ask first agent added through
`OPENCODE_CONFIG_CONTENT` (`"update":"disable"`, `"default_agent"`, and
`"agent":{"crowbar-ask-first":{...}}`, merged with any existing keys) and
`OPENCODE_DISABLE_FREE_MODELS=1` plus `OPENCODE_DISABLE_MODELS_FETCH=1` until
the person enables OpenCode's free hosted Models. Lever writes nothing in the
person's own OpenCode files. The harness child dies with Lever. Lever is the
subreaper of what the harness starts: a process whose parent ends before it
comes to Lever, which waits for it when it ends.

Told to stop (`SIGTERM` or `SIGINT`), Lever closes its peer connections, stops
any running harness child, marks any turn still working `interrupted`, removes
its sockets and `running` marker, closes its database cleanly, logs the stop,
and exits with status `0`.

## The harness

Lever is the client in ACP and the harness is the agent. What Lever does with
ACP today:

- **One session for each thread.** A thread's first turn asks the harness for
  a session (`session/new`), and Lever keeps its ID with the thread. A harness
  that was stopped and started again is asked to load it (`session/load`) when
  it says it can, and Lever drops what the harness replays as it loads: the
  record has it already.
- **A session that cannot be picked up again is replaced, and the thread says
  so.** When the harness cannot load sessions, or the load fails, Lever asks
  for a new session and puts a note in the thread. The harness then has none
  of the thread's earlier turns. Giving a new session the thread so far, as
  ADR 0036 has it, is not built yet.
- **A session is one thread's alone.** A harness that gives a thread a session
  ID another thread holds, or is loading, fails that turn, and so does a
  session with no ID. A thread never loads a session another thread holds.
  Lever uses a session only once its database has it as the thread's: a turn
  whose session the database did not take fails, and its message is not sent
  to the harness. When a harness that was started again
  gives a thread an ID the record has for an older thread, the ID is the newer
  thread's from then on, and the older thread is given a new session, with the
  note, at its next turn.
- **How a turn ends.** `end_turn` is `done`. `cancelled`, or the JSON-RPC
  error for a cancelled request (`-32800`), is `stopped`. Any other stop
  reason, and any other error, is `failed`, whether or not a stop was asked
  for.
- **Updates.** A piece of the agent's message is `text`, a tool call and its
  updates are an `act`, a session's new title is `thread-updated`, and
  `config_option_update` refreshes the harness's Models and the thread's
  current Model. Other ACP v1 updates that Lever does not yet show as their own
  kinds (`agent_thought_chunk`, `available_commands_update`,
  `current_mode_update`, `usage_update`, `plan`, `notice`, compaction, and
  subagent updates) are dropped on purpose so threads are not flooded with
  per-token or boilerplate notes. Any genuinely unrecognized or future update
  is kept as JSON in the record and shown plainly as a `note`.
- **An update that comes after its turn ended goes to the thread's newest
  turn.** ACP gives an update no turn. Lever takes what the harness has said
  before it hears a peer, so an update that is already there when the next
  message arrives stays with the turn it came in. One that comes later still
  is recorded with the turn that is working then.
- **Requests from the harness.** Lever tells the harness it reads and writes no
  files and runs no terminals for it, and answers those requests with
  JSON-RPC's "method not found". A `session/request_permission` becomes a
  request in its thread, with the exact command, or the file and the change,
  and the answers the harness offers. The command and the change are shown as
  the harness sends them, and a change longer than 400 lines is cut in what it
  takes out and what it puts in alike, so both show. It waits with no time
  limit until the person answers it, the harness withdraws it
  (`$/cancel_request`), Stop withdraws it, or the harness ends. A withdrawn
  request is answered `cancelled`, as ACP asks, the one the harness withdrew
  too. While a request waits, the harness is not stopped for being idle. A
  request the harness sends for a session Lever holds for no thread, or for a
  turn the person asked to stop, is answered `cancelled` at once. An answer
  the database does not take is kept, said to whoever asks, and saved when
  the database takes writes again.

  A request is the harness asking the person, not a guard on the person's
  account: any program the person runs can answer it over the person's
  socket, as any can run commands as the person already.
- **Modes.** Lever offers the modes the harness reports in its session
  configuration option of category `mode`, and only those it knows:
  - **Ask first** (`ask-first`) is Lever's own. Lever adds to
    `OPENCODE_CONFIG_CONTENT` a primary OpenCode agent, `crowbar-ask-first`,
    whose permission settings ask before a command (`bash`) or an edit
    (`edit`), and makes it OpenCode's `default_agent`. OpenCode puts an
    agent's own rules after every rule in the person's settings, and the last
    that matches wins, so Ask first carries the person's own rules for `bash`
    and `edit` again after its `"*": "ask"`: what they refuse stays refused,
    and what they allow by name stays allowed. A rule of theirs that allows
    every command or edit is the one Ask first takes the place of. Lever reads
    them where OpenCode reads them for every directory, in its order:
    `opencode.json` and `opencode.jsonc` in `OPENCODE_CONFIG_DIR`, or else
    `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode`, then the file
    `OPENCODE_CONFIG` names, then `OPENCODE_CONFIG_CONTENT`. A project's own
    `opencode.json`, which OpenCode reads only for a session in that project,
    is not read, and its refusals do not outrank Ask first. Lever writes
    nothing in the person's own OpenCode files, so their settings, agents, and
    tool servers apply as they made them, and a session they start by hand
    has none of this.
  - **Full access** (`full-access`) is OpenCode's own `build` agent, which runs
    tools as the person's own settings allow.
  - **Plan** (`plan`) is OpenCode's own `plan` agent, as it is.

  A harness whose mode selector offers none of these has no modes Lever sets.
  A thread keeps its mode. A new thread starts in the starting mode, Ask first
  unless the person chose Full access, and only in one the harness offers.
  Before each turn, Lever sets the session's mode to the thread's, with
  `session/set_config_option`, and a turn whose mode the harness refuses
  fails. A turn stopped while its mode is set leaves the thread working until
  the harness answers, so the next turn knows the session's mode. When the
  harness changes a session's mode itself while a turn's prompt runs, or
  between turns, the thread takes the new mode: OpenCode leaving Plan goes to
  `build`, so the thread is then in Full access.
- **A Stop the harness does not carry out is carried out by Lever.** Lever
  sends `session/cancel` and waits up to `--stop-timeout-ms` (5 seconds) for
  the turn to end. When it has not, Lever ends the turn as `stopped`, and stops
  the harness with every process under it, so no command the turn started goes
  on: each process is held still (`SIGSTOP`) and the tree read again until no
  process is new, then all are killed, those whose parent had ended too. Every
  other turn working on that harness is `interrupted`, or `stopped` if the
  person had asked to stop it, and its requests are withdrawn. The next
  message starts the harness again.
- **Lever reads at most 256 KiB from the harness each time it wakes**, so a
  harness that never stops writing holds back neither Lever's peers nor a stop
  signal. A harness that writes more than 16 MiB with no line end is stopped,
  and its working turns are `interrupted`.

## Channels and framing

Both the surface socket and the person socket are Unix stream sockets checked
immediately on `accept` with `SO_PEERCRED`:

- The **surface socket** (`/run/crowbar-lever/surface`, mode `0660`, in a
  `2750` directory owned by `<person>:crowbar-shell`) serves only the shell's
  user ID. Any other peer is closed at once without a reply.
- The **person socket** (`/run/user/<uid>/crowbar-lever/person`, mode `0600`,
  in a `0700` directory owned by the person) serves only the person's user ID.
  Any other peer is closed at once without a reply.

Every message on both sockets is framed with a 4-byte unsigned big-endian
length prefix followed by that many bytes of UTF-8 JSON. Frames longer than
4 MiB are rejected as soon as their length header is read, and the connection
is closed.

Each socket keeps up to 64 connections open, counted apart, so the person's
clients cannot use up the shell's. One more is closed at once without a reply.
While 1 MiB of replies wait for a peer, Lever reads no more from it and
answers no more of what it has read from it: a peer that reads slowly, or not
at all, finds its own writes stall, and loses nothing it sent. Lever answers
the rest as the peer takes its replies. A peer that closes its sending side
still gets the replies to what it sent before Lever closes the connection, and
what a peer sent whole before it went away altogether is still carried out.

Events are not held back in that way, because a thread goes on working whether
or not a peer that watches it reads. A peer that lets more than 4 MiB of
events wait, over and above the longest one still waiting, is closed, and
Lever's log says why. One event longer than that still reaches a peer that
reads, and a peer that is still taking a long reply is given that much more
room. The
record has what a closed peer missed: it connects again and reads it with
`resume`.

Every thread ID, turn number, act ID, and note ID on both sockets is Lever's
own. Lever puts no harness session ID, tool call ID, or wire message type in
any field of its own, and takes those IDs out of a harness's error messages
and of the updates it shows as notes. An act's detail is the text the harness
gave for it, or the command or path its raw input or output names, and never
the raw value itself. What a harness writes in an answer, or in an act's title
and text, crosses as the harness wrote it.

### Handshake (protocol version 1)

The first frame each way states the protocol version and nothing else:

```json
{"version":1}
```

The peer states it first. If the first frame is anything else, Lever closes the
connection at once without replying.

### Messages

Once both sides have stated version 1, the peer sends JSON objects carrying an
`"op"` string. Lever answers each request with a JSON object carrying a
`"reply"` string, and pushes live thread updates carrying an `"event"` string
to peers subscribed to that thread (via `send` or `resume`). The events a
request brings about are pushed before any later request is answered, so a
record read with `resume` and the events that come after it never say the
same thing twice.

#### `status`

Request:

```json
{"op":"status"}
```

Reply:

```json
{"reply":"status","version":"0.1.0","harness_started":false,"harnesses":[{"id":"harness-1","name":"Stub"}],"free_hosted_models":false,"default_mode":"ask-first"}
```

- `version`: Lever's version string.
- `harness_started`: `true` while the harness child process is running.
- `harnesses`: configured harnesses as `{id, name}` entries (empty when no
  harness is configured).
- `free_hosted_models`: `true` once the person has enabled OpenCode's free
  hosted Models, `false` otherwise.
- `default_mode`: the mode new threads start in, `ask-first` or `full-access`.

#### `models`

Lists the Models the configured harness offers via ACP session configuration
options, starting the harness on demand if needed. Pass `"refresh": true` to
restart an idle harness first (for instance after signing in to a provider).

Request:

```json
{"op":"models"}
```

Reply:

```json
{"reply":"models","supported":true,"models":[{"id":"stub/stub-1","name":"Stub One"}],"default":"stub/stub-1","free_hosted_models":false,"modes":[{"id":"ask-first","name":"Ask first"},{"id":"full-access","name":"Full access"},{"id":"plan","name":"Plan"}]}
```

- `supported`: `true` when the harness exposes Model selection (or reported
  that no Models are currently available), `false` otherwise.
- `models`: `{id, name}` entries offered by the harness (empty when none are
  available).
- `default`: the harness's default Model ID, or `null`.
- `free_hosted_models`: Lever's saved setting.
- `modes`: the modes Lever offers for this harness, as `{id, name}` entries:
  those of `ask-first`, `full-access`, and `plan` the harness reports, in that
  order. Empty when the harness reports no modes.

Errors: `no-harness`, `working`, `harness-error`, `invalid-request`.

#### `set-free-models`

Saves the person's choice for OpenCode's free hosted Models. When changed, an
idle harness is stopped immediately (or as soon as active work finishes, while
refusing `send`, `models`, and `set-model` requests with `working` until the
restart completes) so the next start applies the new
`OPENCODE_DISABLE_FREE_MODELS` and `OPENCODE_DISABLE_MODELS_FETCH` settings.
Only the person's socket may change this setting; the surface socket is refused
with `unsupported`.

Request:

```json
{"op":"set-free-models","enabled":true}
```

Reply:

```json
{"reply":"free-models","enabled":true}
```

Errors: `unsupported`, `invalid-request`, `storage-error`.

#### `set-model`

Sets a thread's Model to one offered by the harness, applying it through ACP's
`session/set_config_option` on an active session (or validating against the
harness's offered Models and applying it on the next turn) and saving it on the
thread in SQLite.

Request:

```json
{"op":"set-model","thread":"t-01a2b3c4d5e6","model":"stub/stub-1"}
```

Reply:

```json
{"reply":"model-set","thread":"t-01a2b3c4d5e6","model":"stub/stub-1"}
```

Errors: `no-harness`, `no-thread`, `working`, `unsupported`, `no-model`,
`invalid-model`, `harness-error`, `invalid-request`, `storage-error`.

#### `list`

Lists all threads in Lever's database, newest activity first.

Request:

```json
{"op":"list"}
```

Reply:

```json
{"reply":"list","threads":[{"thread":"t-01a2b3c4d5e6","title":"List the files","cwd":"/home/crowbar","started":1764000000,"updated":1764000005,"turns":1,"state":"done","working":false,"waiting":false,"mode":"ask-first"}]}
```

- `state`: the newest turn's state (`"working"`, `"done"`, `"stopped"`,
  `"failed"`, `"interrupted"`, or `"idle"`).
- `working`: `true` while a turn on this thread is running.
- `waiting`: `true` while a request of this thread waits for the person.
- `mode`: the thread's mode.
- `model`: optional Model ID last applied to this thread, omitted when unset.

#### `send`

Sends a message to a thread, starting a new thread when `thread` is omitted or
`null`, and subscribes this connection to events for that thread.

Request:

```json
{"op":"send","thread":null,"text":"List the files","cwd":"/home/crowbar"}
```

- `thread`: existing thread ID, or `null`/omitted to start a new thread.
- `text`: non-empty message text.
- `cwd`: optional absolute path for a new thread's working directory (defaults
  to `$HOME`).
- `model`: optional Model ID to select for the thread before sending the turn.
- `mode`: optional mode for the thread from this turn on (`ask-first`,
  `full-access`, or `plan`). A new thread without one starts in the starting
  mode. One that changes an existing thread's mode comes with a
  `thread-updated` event before the turn's `message`.

Reply:

```json
{"reply":"sent","thread":"t-01a2b3c4d5e6","turn":1}
```

Errors: `no-harness`, `no-model`, `no-thread`, `working`, `invalid-mode`, `invalid-request`,
and `storage-error` when Lever's database does not take the turn, or has yet to
take how the thread's last turn ended.

#### `resume`

Reads a thread's full record from Lever's database (without starting a harness)
and subscribes this connection to future events for that thread. The reply is
one frame that holds the whole record, so a client must take frames as large
as its longest thread. `crowbar-agent` takes frames up to 64 MiB, and says
that Lever sent a frame too long to read when a record is longer. Reading a
record in parts is not built yet.

Request:

```json
{"op":"resume","thread":"t-01a2b3c4d5e6"}
```

Reply:

```json
{"reply":"resumed","thread":"t-01a2b3c4d5e6","title":"List the files","cwd":"/home/crowbar","started":1764000000,"updated":1764000005,"working":false,"mode":"ask-first","turns":[{"turn":1,"message":"List the files","answer":"Here are the files.","state":"done","mode":"ask-first","acts":[{"act":"act-01a2b3c4d5e6","kind":"command","title":"Run ls","state":"done","detail":"Cargo.toml"}],"requests":[{"request":"r-01a2b3c4d5e6","act":"act-01a2b3c4d5e6","kind":"command","title":"Run ls","detail":"ls","answers":[{"answer":"once","name":"Allow once"},{"answer":"always","name":"Always allow"},{"answer":"no","name":"Reject"}],"state":"answered","answer":"once"}],"notes":[{"note":"note-01a2b3c4d5e6","text":"Cache warm"}]}]}
```

When a turn ended with `"state":"failed"`, the turn object also carries
`"error":"..."`. When a turn was sent from the drawer with desktop context from
Fulcrum, it also carries `"context":{...}`. When a thread has a saved Model,
the reply also carries `"model":"..."`. The reply carries the thread's
`"mode"`, and each turn the mode it began in (`"mode"`, absent for turns
recorded before modes) and its requests (`"requests":[...]`, each as the
[`request` event](#events) has it, without `event`, `thread`, and `turn`).

Errors: `no-thread`, `invalid-request`.

#### `stop`

Stops the currently working turn on a thread and tells the harness to cancel
it. When `turn` is given, only that turn is stopped. The reply says that Lever
took the request, not that the turn stopped: a turn the harness had not begun
is `stopped` at once, and one it was working on ends when the harness answers,
as `stopped`, or as `failed` if the harness failed instead. A harness that has
not ended the turn after `--stop-timeout-ms` is stopped, as
[the harness](#the-harness) says. Stop withdraws every request of the thread
that waits, whether or not a turn is working, and with `turn`, only that
turn's. A thread with no turn working is answered `ok` too.

Request:

```json
{"op":"stop","thread":"t-01a2b3c4d5e6","turn":1}
```

Reply:

```json
{"reply":"ok"}
```

Errors: `no-thread`, `invalid-request`.

#### `answer`

Answers a request that waits, with one of the answers it offers. Either socket
may answer.

Request:

```json
{"op":"answer","thread":"t-01a2b3c4d5e6","request":"r-01a2b3c4d5e6","answer":"once"}
```

Reply:

```json
{"reply":"ok"}
```

A `request` event with `"state":"answered"` follows. Errors: `no-thread`,
`no-request` (no such request, or it no longer waits), `invalid-answer` (not
one the request offers), `invalid-request`.

#### `set-mode`

Sets a thread's mode, which applies from its next turn. Either socket may set
it.

Request:

```json
{"op":"set-mode","thread":"t-01a2b3c4d5e6","mode":"full-access"}
```

Reply:

```json
{"reply":"mode-set","thread":"t-01a2b3c4d5e6","mode":"full-access"}
```

A `thread-updated` event with the mode follows. Errors: `no-thread`, `working`
(a turn of the thread is working), `invalid-mode` (not `ask-first`,
`full-access`, or `plan`, or one the harness is known not to offer),
`invalid-request`, `storage-error`.

#### `set-default-mode`

Sets the mode new threads start in. Either socket may set it.

Request:

```json
{"op":"set-default-mode","mode":"full-access"}
```

Reply:

```json
{"reply":"default-mode","mode":"full-access"}
```

Errors: `invalid-mode` (not `ask-first` or `full-access`, or one the harness
does not offer), `invalid-request`, `storage-error`.

#### `watch`

Says which threads are working and which wait for the person, and subscribes
this connection to `activity` events, which say it again each time either
changes. The shell's mark reads it.

Request:

```json
{"op":"watch"}
```

Reply:

```json
{"reply":"activity","working":["t-01a2b3c4d5e6"],"waiting":[]}
```

#### `error`

A request Lever cannot answer gets one of three outcomes:

- An `"op"` Lever does not know is answered, and the connection stays open:

  ```json
  {"reply":"error","code":"unsupported","message":"This request is not supported."}
  ```

- JSON that is not an object with a string `"op"` (or fails field validation)
  is answered, and the connection stays open:

  ```json
  {"reply":"error","code":"invalid-request","message":"Expected a JSON object with a string op field."}
  ```

- A frame that is not JSON gets no reply, and Lever closes the connection. The
  peer connects again and states its version to go on.

#### Events

Pushed to every connection subscribed to `thread`:

- **`message`** — a turn began with the person's message (when sent from the
  surface socket with Fulcrum reachable, also carries `"context"` with the
  desktop context snapshot; never window contents or clipboard. When sent to
  the harness, Lever wraps desktop context in a `<desktop-context>` metadata
  fence marked as untrusted app metadata, not instructions):

  ```json
  {"event":"message","thread":"t-01a2b3c4d5e6","turn":1,"text":"List the files"}
  ```

- **`text`** — a streamed piece of the answer (also taken if it arrives after a
  turn ended):

  ```json
  {"event":"text","thread":"t-01a2b3c4d5e6","turn":1,"text":"Here are the files."}
  ```

- **`act`** — an act started, progressed, or finished (with Lever's own `act`
  ID, `kind` in `read`, `edit`, `delete`, `move`, `search`, `command`, `think`,
  `fetch`, `mode`, `other`, `state` in `pending`, `working`, `done`, `failed`,
  and optional `detail`):

  ```json
  {"event":"act","thread":"t-01a2b3c4d5e6","turn":1,"act":"act-01a2b3c4d5e6","kind":"command","title":"Run ls","state":"done","detail":"Cargo.toml"}
  ```

- **`note`** — an update Lever shows as neither text nor an act, kept as JSON
  in SQLite and shown plainly (also taken if it arrives after a turn ended).
  A note can also be Lever's own, as when a thread's harness began a new
  session for it:

  ```json
  {"event":"note","thread":"t-01a2b3c4d5e6","turn":1,"note":"note-01a2b3c4d5e6","text":"Cache warm"}
  ```

- **`request`** — a request that waits for the person, and each change to
  it. `kind` is the kind of the act it asks about, `act` that act's ID,
  `title` what the harness calls it, and `detail` the exact command, or the
  change as lines that begin `-` and `+`. `path` is the file, for an edit. `answers` are those the harness offers, as `once`, `always`, `no`,
  and `never`, each with the harness's `name` for it. `state` is `waiting`,
  `answered` (with `answer`), or `withdrawn`:

  ```json
  {"event":"request","thread":"t-01a2b3c4d5e6","turn":1,"request":"r-01a2b3c4d5e6","act":"act-01a2b3c4d5e6","kind":"command","title":"echo hi","detail":"echo hi","answers":[{"answer":"once","name":"Allow once"},{"answer":"always","name":"Always allow"},{"answer":"no","name":"Reject"}],"state":"waiting"}
  ```

- **`thread-updated`** — the thread's title or mode changed. It carries
  `title`, `mode`, or both:

  ```json
  {"event":"thread-updated","thread":"t-01a2b3c4d5e6","title":"Inspect workspace"}
  ```

- **`activity`** — to connections that sent `watch`, when the threads working
  or waiting change, as the `watch` reply has them:

  ```json
  {"event":"activity","working":[],"waiting":["t-01a2b3c4d5e6"]}
  ```

- **`done`** — the turn finished normally:

  ```json
  {"event":"done","thread":"t-01a2b3c4d5e6","turn":1}
  ```

- **`stopped`** — the turn was stopped by `stop`:

  ```json
  {"event":"stopped","thread":"t-01a2b3c4d5e6","turn":1}
  ```

- **`failed`** — the turn failed with a reason. A turn the harness finished
  also fails, with "Lever could not save all of this turn.", when Lever's
  database did not take all of it: the answer still reached who was watching,
  and the record does not have all of it.

  ```json
  {"event":"failed","thread":"t-01a2b3c4d5e6","turn":1,"message":"The harness reported an error."}
  ```

- **`interrupted`** — the harness exited unexpectedly mid-turn, or Lever
  stopped or restarted with the turn still working:

  ```json
  {"event":"interrupted","thread":"t-01a2b3c4d5e6","turn":1}
  ```

## State and logging

Lever stores its state in `--state` (mode `0700`, owned by the person):

- `lever.sqlite` (mode `0600`, SQLite WAL mode with `synchronous = NORMAL`,
  plus `0600` sidecar `-wal`/`-shm` files): Lever's database holding `threads`,
  `turns`, `acts`, `requests`, and `notes` (with `raw_json` keeping the JSON
  of each update shown as a note), and the starting mode. A request that waits
  when Lever starts is withdrawn, as the turn that asked it is interrupted. A write the database does not take is logged as an
  `error` that names what could not be done and nothing a thread said. How a
  turn ended is tried again until the database takes it, so a thread is not
  left `working` in the record after its turn is over, and `list` and `resume`
  say how the turn ended in the meantime.
- `lever.log` (mode `0600`, rotated at 4 MiB to `lever.log.1`): UTC-timestamped
  lines for `start`, `restart`, `listening`, `connection`, `harness`, `stop`,
  and `error`. Nothing a peer sent, a thread said, or a harness answered is
  ever written to `lever.log`.
- `running` (mode `0600`): holds Lever's PID while running and is removed on a
  clean stop. If present at the next start, Lever logs a `restart` entry naming
  the previous PID and marks any still-working turns `interrupted`.
