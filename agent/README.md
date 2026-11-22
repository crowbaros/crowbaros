# The Harness

The program that runs the Agent
([ADR 0034](../docs/adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)):
[Pi Durable](https://github.com/earendil-works/pi), pinned to an exact
version, and our TypeScript for what must run inside it. Base bundles both
into one file and runs it from boot as the person's account, on a Node.js of
its own. Everything on the other side of a socket is Rust, and no Pi Durable
name, type, or ID crosses one.

This is the first slice of
[Milestone 3](https://github.com/crowbaros/crowbaros/issues/291). The Harness
starts, opens Pi Durable's database, and answers the version on its surface
socket. It holds no conversation and reaches no Model yet.

| Path | Contents |
| --- | --- |
| `src/main.ts` | The process: its options, its state, starting and stopping |
| `src/surface.ts` | The surface socket, where the shell reaches the Harness |
| `src/frames.ts` | The framing, with no socket |
| `src/peer.ts` | The user ID at the other end of a socket |
| `src/store.ts` | Pi Durable and its database. Nothing outside this file names Pi Durable |
| `src/log.ts` | The log |
| `test/` | The tests, which start the bundle as one process |
| `bundle.mjs` | Bundles `src/` and all it needs into `dist/harness.mjs` |

## Build and test

```sh
base/void_base.py agent   # type-check, test, and bundle, with the Node.js Base pins
```

That is the command CI's `Harness` job runs, and Base's build runs it before
it writes an image. It downloads the pinned Node.js, checks its SHA-256, and
runs the npm that came with it. With a Node.js of the same version on `PATH`,
the steps are:

```sh
npm ci          # exactly what package-lock.json names
npm run check   # the types
npm test        # bundles, then runs the tests against the bundle
```

npm runs only here, in the build. `.npmrc` stops it running any script a
package brings. The image holds `dist/harness.mjs` and Node.js's binary, with
no `node_modules` and no npm.

The tests are the Harness's own seam. Each starts `dist/harness.mjs` as one
process, as Base's service does, talks to it over a real socket in a
temporary directory, and reads its log, its files, and how it exits. They
write frames out by hand, so they do not check the framing against the code
that frames. None names a Pi Durable type. They need no root: the user ID
that runs them stands for the shell's.

## Running it

```sh
node dist/harness.mjs --state DIRECTORY --surface-socket PATH --shell-uid USER_ID
```

- `--state`: where the Harness keeps its database and its log. It makes the
  directory.
- `--surface-socket`: where it listens for the shell.
- `--shell-uid`: the one user ID it serves there.

Base's `crowbar-agent` service
([`base/desktop/agent-run`](../base/desktop/agent-run)) runs it as the person,
in the environment the launcher gives an app, starting in their home, with
its state in `~/.local/state/crowbaros/agent`. runit starts it again a second
after any exit.

Told to stop with SIGTERM or SIGINT, it takes no new connection, closes the
database, removes its socket, and exits with status 0. An error it cannot go
on from is logged, and it exits with status 1.

## State

Everything the Harness keeps is in the state directory, which is the person's
alone: the directory has mode 0700 and each file 0600, whatever mask the
process starts with. The Harness leaves the mask as it is, since the commands
the Agent will run inherit it.

| File | Contents |
| --- | --- |
| `agent.sqlite` | Pi Durable's database, with SQLite's `-wal` and `-shm` files beside it. It keeps Pi Durable's default sync setting |
| `agent.log` | The log |
| `running` | The process ID of the Harness that is running. It is removed on a stop, so one left behind means the last Harness ended without stopping |

## The log

`agent.log` gets one line for each event: the time in UTC, the event, and its
details. An error's line goes on over indented lines that say where it was
thrown.

```text
2026-11-21T18:03:11.123Z start: process 812, Pi Durable 1.6.0, Node.js v26.13.1
2026-11-21T18:03:11.131Z restart: the Harness before this one, process 640, ended without stopping
2026-11-21T18:03:11.188Z listening: /run/crowbar-agent/surface, for user ID 1001, 412 ms after the process started
2026-11-21T18:03:12.040Z connection: the shell connected
2026-11-21T18:03:12.041Z connection: the shell stated version 1
2026-11-21T18:03:15.770Z connection: refused user ID 1003, which is not the shell's
2026-11-21T18:09:42.300Z stop: SIGTERM
```

| Event | When |
| --- | --- |
| `start` | The process started |
| `restart` | This start found that the Harness before it ended without stopping: it was killed, it crashed, or the machine lost power |
| `listening` | The surface socket takes connections |
| `connection` | A peer connected, stated the version, left, was refused, or was closed, with why |
| `stop` | The Harness was told to stop, and has |
| `error` | An error ended the Harness |

The log never holds what a conversation says or what an act printed. The code
that logs is given events with the IDs, counts, and paths that go with them,
and nothing a peer sent.

## The surface socket

The shell reaches the Harness at the surface socket. The Harness listens, and
serves the shell's user ID alone: it asks the kernel for the peer's user ID
(`SO_PEERCRED`) and closes any other peer at once, root's included, having
read nothing and sent nothing. In Base the socket is
`/run/crowbar-agent/surface`. The service makes its directory as root, owned
by the person so the Harness can bind there, with the shell's group and mode
2750, so the shell's account is the only other one that can enter.

Node.js has no call for a peer's user ID. `src/peer.ts` asks through
`node:ffi`, which Node.js 26 marks experimental: it calls the C library's
`getsockopt` in the process itself, so there is no native addon to build and
nothing to load. A peer the kernel gives no user ID for is refused. The tests
fail if a new Node.js changes this.

### Framing

The surface socket speaks the framing every channel of the Agent speaks
([ADR 0035](../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md)),
which `src/frames.ts` holds:

- **A message is a frame**: its length in four bytes, the most significant
  first, and then that many bytes of JSON in UTF-8. Nothing comes between
  frames.
- **The first frame each way states the protocol version** and nothing else:
  `{"version":1}`. The shell sends its first, and the Harness answers with its
  own. A peer whose first frame is anything else, or states a version the
  Harness does not speak, is closed with no answer, and the log says why.
- **A frame from the shell is at most 4096 bytes**, not counting its length.
  A longer frame closes the connection as soon as its length is read. The
  slice that adds a message that needs more raises the limit.

The surface socket's version is 1. The lengths are shown in hexadecimal:

```text
-> 00 00 00 0d {"version":1}
<- 00 00 00 0d {"version":1}
```

### Messages

None yet. The slices that follow add them: the shell sending a message to a
conversation, the Harness streaming the conversation back, and the rest of
what the [spec](https://github.com/crowbaros/crowbaros/issues/291) lists.
Each is written here when it is added. Until then, a frame after the version
is one the Harness does not speak, and it closes the connection.

## Upgrading Pi Durable or Node.js

Pi Durable calls itself experimental and has broken its API between releases,
so its version in `package.json` is exact, as is every other dependency's, and
`package-lock.json` pins what they bring. An upgrade is a pull request of its
own, and runs the desktop check (ADR 0034).

Node.js is pinned in [`base/config.json`](../base/config.json), by address and
SHA-256: the official Linux x64 binary, on the newest LTS line the pinned Pi
Durable supports. The checksum is the one in that release's `SHASUMS256.txt`
on nodejs.org.
