# Lever

Lever (`crowbar-lever`) is the CrowbarOS service that drives the person's
harnesses and keeps the record of every thread
([ADR 0036](../docs/adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md)).
One runs on each machine from boot as the person's account, in the launcher's
environment for apps, supervised by runit (`crowbar-lever`).

Lever currently listens on its two sockets, verifies peer identity, keeps its
database and private log in the person's home, and answers `status` requests
reporting that no harness is started yet. Driving a harness over the Agent
Client Protocol follows in the next slice (#374).

## Build and test

From this directory:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

`lever/client/` holds `crowbar-agent`, excluded from this Cargo package until
it is ported to Lever in #374.

## Invocation

```sh
crowbar-lever \
  --state <dir> \
  --surface-socket <path> --shell-uid <uid> \
  --person-socket <path> --person-uid <uid>
```

On Base:

- `--state` is `$HOME/.local/state/crowbaros/lever`
- `--surface-socket` is `/run/crowbar-lever/surface`, for `crowbar-shell`'s UID
- `--person-socket` is `/run/user/1003/crowbar-lever/person`, for the person's UID

Told to stop (`SIGTERM` or `SIGINT`), Lever closes its peer connections,
removes its sockets and `running` marker, closes its database cleanly, logs the
stop, and exits with status `0`.

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

### Handshake (protocol version 1)

The first frame each way states the protocol version and nothing else:

```json
{"version":1}
```

The peer states it first. If the first frame is anything else, Lever closes the
connection at once without replying.

### Messages

Once both sides have stated version 1, the peer sends JSON objects carrying an
`"op"` string and Lever answers with JSON objects carrying a `"reply"` string.

#### `status`

Request:

```json
{"op":"status"}
```

Reply:

```json
{"reply":"status","version":"0.1.0","harness_started":false,"harnesses":[]}
```

- `version`: Lever's version string.
- `harness_started`: `false` while no harness process is running.
- `harnesses`: list of available harnesses (empty until Lever drives harnesses).

#### `error`

A request Lever cannot answer gets one of three outcomes:

- An `"op"` Lever does not know is answered, and the connection stays open:

  ```json
  {"reply":"error","code":"unsupported","message":"This request is not supported."}
  ```

- JSON that is not an object with a string `"op"` is answered, and the
  connection stays open:

  ```json
  {"reply":"error","code":"invalid-request","message":"Expected a JSON object with a string op field."}
  ```

- A frame that is not JSON gets no reply, and Lever closes the connection. The
  peer connects again and states its version to go on.

## State and logging

Lever stores its state in `--state` (mode `0700`, owned by the person):

- `lever.sqlite` (mode `0600`, SQLite WAL mode with `synchronous = NORMAL`,
  plus `0600` sidecar `-wal`/`-shm` files): Lever's database.
- `lever.log` (mode `0600`, rotated at 4 MiB to `lever.log.1`): UTC-timestamped
  lines for `start`, `restart`, `listening`, `connection`, `stop`, and `error`.
  Nothing a peer sent is ever written to `lever.log`.
- `running` (mode `0600`): holds Lever's PID while running and is removed on a
  clean stop. If present at the next start, Lever logs a `restart` entry naming
  the previous PID.
