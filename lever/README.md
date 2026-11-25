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
after 10 minutes with no thread working.

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
  [--harness <program> [--harness-arg <arg>]... [--harness-name <name>]] \
  [--idle-timeout-ms <ms>]
```

On Base:

- `--state` is `$HOME/.local/state/crowbaros/lever`
- `--surface-socket` is `/run/crowbar-lever/surface`, for `crowbar-shell`'s UID
- `--person-socket` is `/run/user/1003/crowbar-lever/person`, for the person's UID

When `--harness` is omitted (as on Base before OpenCode is bundled), Lever runs
with no harness configured and refuses `send` with `no-harness`. When
`--harness` is given, Lever starts the harness child only when a thread needs
it and stops it after `--idle-timeout-ms` (default `600000`, 10 minutes) with
no working thread. The harness child dies with Lever.

Told to stop (`SIGTERM` or `SIGINT`), Lever closes its peer connections, stops
any running harness child, removes its sockets and `running` marker, closes its
database cleanly, logs the stop, and exits with status `0`.

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
While 1 MiB of replies wait for a peer, Lever reads no more from it: a peer
that reads slowly, or not at all, finds its own writes stall, and loses
nothing it sent. A peer that closes its sending side still gets the replies to
what it sent before Lever closes the connection.

Every thread ID, turn number, act ID, and note ID on both sockets is Lever's
own. No harness session ID, tool call ID, or wire message type crosses Lever's
sockets.

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
to peers subscribed to that thread (via `send` or `resume`).

#### `status`

Request:

```json
{"op":"status"}
```

Reply:

```json
{"reply":"status","version":"0.1.0","harness_started":false,"harnesses":[{"id":"harness-1","name":"Stub"}]}
```

- `version`: Lever's version string.
- `harness_started`: `true` while the harness child process is running.
- `harnesses`: configured harnesses as `{id, name}` entries (empty when no
  harness is configured).

#### `list`

Lists all threads in Lever's database, newest activity first.

Request:

```json
{"op":"list"}
```

Reply:

```json
{"reply":"list","threads":[{"thread":"t-01a2b3c4d5e6","title":"List the files","cwd":"/home/crowbar","started":1764000000,"updated":1764000005,"turns":1,"state":"done","working":false}]}
```

- `state`: the newest turn's state (`"working"`, `"done"`, `"stopped"`,
  `"failed"`, `"interrupted"`, or `"idle"`).
- `working`: `true` while a turn on this thread is running.

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

Reply:

```json
{"reply":"sent","thread":"t-01a2b3c4d5e6","turn":1}
```

Errors: `no-harness`, `no-thread`, `working`, `invalid-request`.

#### `resume`

Reads a thread's full record from Lever's database (without starting a harness)
and subscribes this connection to future events for that thread.

Request:

```json
{"op":"resume","thread":"t-01a2b3c4d5e6"}
```

Reply:

```json
{"reply":"resumed","thread":"t-01a2b3c4d5e6","title":"List the files","cwd":"/home/crowbar","started":1764000000,"updated":1764000005,"working":false,"turns":[{"turn":1,"message":"List the files","answer":"Here are the files.","state":"done","acts":[{"act":"act-01a2b3c4d5e6","kind":"command","title":"Run ls","state":"done","detail":"Cargo.toml"}],"notes":[{"note":"note-01a2b3c4d5e6","text":"Cache warm"}]}]}
```

When a turn ended with `"state":"failed"`, the turn object also carries
`"error":"..."`.

Errors: `no-thread`, `invalid-request`.

#### `stop`

Stops the currently working turn on a thread and tells the harness to cancel
it. When `turn` is given, only that turn is stopped.

Request:

```json
{"op":"stop","thread":"t-01a2b3c4d5e6","turn":1}
```

Reply:

```json
{"reply":"ok"}
```

Errors: `no-thread`, `invalid-request`.

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

- **`message`** — a turn began with the person's message:

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

- **`note`** — an update Lever did not recognize, stored verbatim in SQLite and
  shown plainly (also taken if it arrives after a turn ended):

  ```json
  {"event":"note","thread":"t-01a2b3c4d5e6","turn":1,"note":"note-01a2b3c4d5e6","text":"Cache warm"}
  ```

- **`thread-updated`** — the thread's title changed:

  ```json
  {"event":"thread-updated","thread":"t-01a2b3c4d5e6","title":"Inspect workspace"}
  ```

- **`done`** — the turn finished normally:

  ```json
  {"event":"done","thread":"t-01a2b3c4d5e6","turn":1}
  ```

- **`stopped`** — the turn was stopped by `stop`:

  ```json
  {"event":"stopped","thread":"t-01a2b3c4d5e6","turn":1}
  ```

- **`failed`** — the turn failed with a reason:

  ```json
  {"event":"failed","thread":"t-01a2b3c4d5e6","turn":1,"message":"The harness reported an error."}
  ```

- **`interrupted`** — the harness exited unexpectedly mid-turn (or Lever
  restarted after an unclean stop):

  ```json
  {"event":"interrupted","thread":"t-01a2b3c4d5e6","turn":1}
  ```

## State and logging

Lever stores its state in `--state` (mode `0700`, owned by the person):

- `lever.sqlite` (mode `0600`, SQLite WAL mode with `synchronous = NORMAL`,
  plus `0600` sidecar `-wal`/`-shm` files): Lever's database holding `threads`,
  `turns`, `acts`, and `notes` (with `raw_json` preserving unrecognized updates
  verbatim).
- `lever.log` (mode `0600`, rotated at 4 MiB to `lever.log.1`): UTC-timestamped
  lines for `start`, `restart`, `listening`, `connection`, `harness`, `stop`,
  and `error`. Nothing a peer sent or a thread said is ever written to
  `lever.log`.
- `running` (mode `0600`): holds Lever's PID while running and is removed on a
  clean stop. If present at the next start, Lever logs a `restart` entry naming
  the previous PID and marks any still-working turns `interrupted`.
