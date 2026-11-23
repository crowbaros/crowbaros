# The Harness

The program that runs the Agent
([ADR 0034](../docs/adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)):
[Pi Durable](https://github.com/earendil-works/pi), pinned to an exact
version, and our TypeScript for what must run inside it. Base bundles both
into one file and runs it from boot as the person's account, on a Node.js of
its own. Everything on the other side of a socket is Rust, and no Pi Durable
name, type, or ID crosses one.

This is part of
[Milestone 3](https://github.com/crowbaros/crowbaros/issues/291). The Harness
holds conversations, reaches a Model the person signed in to, and streams
each answer to the person's clients on its person socket. The first client
is the terminal's: [`crowbar-agent`](client/README.md). The Agent has no act
yet, and the shell's surface socket has no message yet.

| Path | Contents |
| --- | --- |
| `src/main.ts` | The process: its options, its state, starting and stopping |
| `src/channel.ts` | What both sockets share: the one user ID served, the framing, and the version |
| `src/surface.ts` | The surface socket, where the shell reaches the Harness |
| `src/person.ts` | The person socket's messages |
| `src/conversations.ts` | Conversations as clients follow them |
| `src/models.ts` | Reaching a Model: pi-ai's providers, signing in, and the Model in use |
| `src/credentials.ts` | The credential store |
| `src/frames.ts` | The framing, with no socket |
| `src/peer.ts` | The user ID at the other end of a socket |
| `src/store.ts` | Pi Durable and its database. Nothing outside this file names Pi Durable |
| `src/log.ts` | The log |
| `test/` | The tests, which start the bundle as one process, and the stub Model |
| `bundle.mjs` | Bundles `src/` and all it needs into `dist/harness.mjs` |
| [`client/`](client/README.md) | `crowbar-agent`, the terminal's client, in Rust |

## Build and test

```sh
base/void_base.py agent   # type-check, test, and bundle, with the Node.js Base pins, and test the client
```

That is the command CI's `Harness` job runs, and Base's build runs it before
it writes an image. It downloads the pinned Node.js, checks its SHA-256, and
runs the npm that came with it. Then it runs every test of
[`crowbar-agent`](client/README.md) against the bundle, with the host's
Cargo. With a Node.js of the same version on `PATH`, the Harness's own steps
are:

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
that runs them stands for the shell's and for the person's. Where a test
needs a Model, it starts [the stub Model](#the-stub-model). A few faults
cannot be caused from outside the process: a stop before the socket listens
or at the moment it starts to, an error on the listening socket, a write the
kernel takes only part of. For those, a test has Node.js load
`test/faults.ts` ahead of the bundle, which causes the one fault the test
names. The Harness never loads it.

## Running it

```sh
node dist/harness.mjs --state DIRECTORY --surface-socket PATH --shell-uid USER_ID \
  --person-socket PATH --person-uid USER_ID
```

- `--state`: where the Harness keeps its database, its credentials, and its
  log. It makes the directory.
- `--surface-socket`: where it listens for the shell.
- `--shell-uid`: the one user ID it serves there.
- `--person-socket`: where it listens for the person's clients. It makes the
  socket's directory, the person's alone.
- `--person-uid`: the one user ID it serves there.

Base's `crowbar-agent` service
([`base/desktop/agent-run`](../base/desktop/agent-run)) runs it as the person,
in the environment the launcher gives an app, starting in their home, with
its state in `~/.local/state/crowbaros/agent` and its person socket at
`/run/user/<the person's user ID>/crowbar-agent/person`. runit starts it again
whenever it exits: at once, or a second later when it had run for less than a
second. The [footprint record](../docs/research/harness-footprint-2026-11-22.md)
has its memory and its start time in the VM before it reached any Model, and
Base's boot check holds the idle Harness to ADR 0034's 150 MiB on every run.

Told to stop with SIGTERM or SIGINT, it takes no new connection, closes the
database, removes its sockets, and exits with status 0. A turn it was working
on goes on when it starts again. An error it cannot go on from is logged, and
it exits with status 1. When the log is what failed, it prints the error, and
Base's service keeps that in `/var/log/crowbar-agent.log`.

### On a developer's machine

With a Node.js of the pinned version on `PATH`:

```sh
npm start
```

It bundles the Harness and runs it in the foreground, with its state in
`~/.local/state/crowbaros/agent-dev` and both sockets in
`$XDG_RUNTIME_DIR/crowbar-agent/`. `crowbar-agent` finds the person socket
there, as it does in Base. Ctrl-C stops it. `crowbar-agent` never starts a
Harness itself.

## State

Everything the Harness keeps is in the state directory, which is the person's
alone: the directory has mode 0700 and each file the Harness writes 0600,
whatever mask the process starts with. The Harness leaves the mask as it is,
since the commands the Agent will run inherit it.

| File | Contents |
| --- | --- |
| `harness.sqlite` | Pi Durable's database, with SQLite's `-wal` and `-shm` files beside it. It keeps Pi Durable's default sync setting. The conversations are in it |
| `credentials.json` | The API key or OAuth tokens of each provider the person is signed in to |
| `settings.json` | The Model the person chose, and this installation's ID, which some sign-ins ask for |
| `endpoint.json` | Not the Harness's: a person or the lab writes it, to [point the Harness at one endpoint](#one-endpoint-by-hand) |
| `harness.log` | The log, and `harness.log.1`, the one before it |
| `running` | The process ID of the Harness that is running. It is removed on a stop, so one left behind means the last Harness ended without stopping |

Any program running as the person can read the credentials: ADR 0034 runs the
Harness as the person, with no isolation inside their account.

## The log

`harness.log` gets one line for each event: the time in UTC, the event, and
its details. An error's line goes on over indented lines that say where it was
thrown. A line that would take the log past 4 MiB starts a new log, and the
old one is set aside as `harness.log.1`, in place of the one set aside before
it. So the two stay within 8 MiB, and neither a Harness that cannot start nor
a peer that connects without end fills the person's disk.

```text
2026-11-21T18:03:11.123Z start: process 812, Pi Durable 1.6.0, Node.js v26.13.1
2026-11-21T18:03:11.131Z restart: the Harness before this one, process 640, ended without stopping
2026-11-21T18:03:11.280Z listening: /run/user/1003/crowbar-agent/person, for user ID 1003, 157 ms after the process started
2026-11-21T18:03:11.281Z listening: /run/crowbar-agent/surface, for user ID 1001, 158 ms after the process started
2026-11-21T18:03:12.040Z connection: the shell connected
2026-11-21T18:03:12.041Z connection: the shell stated version 1
2026-11-21T18:03:15.770Z connection: refused user ID 1003, which is not the shell's
2026-11-21T18:04:02.118Z connection: a client connected
2026-11-21T18:04:02.119Z connection: a client stated version 1
2026-11-21T18:04:02.131Z conversation: k3f9q2xz started
2026-11-21T18:04:02.140Z turn: conversation k3f9q2xz, turn 1 began
2026-11-21T18:04:05.902Z turn: conversation k3f9q2xz, turn 1 ended: done
2026-11-21T18:04:05.911Z connection: a client left
2026-11-21T18:09:42.300Z stop: SIGTERM
```

| Event | When |
| --- | --- |
| `start` | The process started |
| `restart` | This start found that the Harness before it ended without stopping: it was killed, it crashed, or the machine lost power |
| `listening` | A socket takes connections: the person socket, then the surface socket |
| `connection` | A peer connected, stated the version, left, was refused, or was closed, with why. The shell is "the shell", and a client of the person's "a client" |
| `conversation` | A conversation started, by its ID |
| `turn` | A turn began, or ended: `done`, `stopped`, or `failed` |
| `sign-in` | The person signed in to a provider or out of one, a sign-in failed or was cancelled, or the Model in use changed, by the provider's name |
| `stop` | The Harness was told to stop, and has |
| `error` | An error ended the Harness |

The log never holds what a conversation says, what an act printed, a
credential, or why a Model failed. The code that logs is given events with
the IDs, counts, and paths that go with them, and nothing a peer sent.

## The sockets

The Harness listens on two sockets. Each serves one user ID: the Harness asks
the kernel for the peer's user ID (`SO_PEERCRED`) and closes any other peer at
once, root's included, having read nothing and sent nothing. A peer the
kernel gives no user ID for is refused.

Node.js has no call for a peer's user ID. `src/peer.ts` asks through
`node:ffi`, which Node.js 26 marks experimental: it calls the C library's
`getsockopt` in the process itself, so there is no native addon to build and
nothing to load. The tests fail if a new Node.js changes this.

### Framing

Both sockets speak the framing every channel of the Agent speaks.
[ADR 0035](../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md)
gave it to Fulcrum's channels, and the
[Milestone 3 spec](https://github.com/crowbaros/crowbaros/issues/291) gives
it to the rest. `src/frames.ts` holds it, and `src/channel.ts` serves it:

- **A message is a frame**: its length in four bytes, the most significant
  first, and then that many bytes of JSON in UTF-8. Nothing comes between
  frames.
- **The first frame each way states the protocol version** and nothing else:
  `{"version":1}`. The peer sends its first, and the Harness answers with its
  own. A peer whose first frame is anything else, or states a version the
  Harness does not speak, is closed with no answer, and the log says why.
- **A frame from the peer has a longest length**, not counting its four
  bytes: 4096 bytes on the surface socket, and a mebibyte on the person
  socket, for a long message of the person's. A longer frame closes the
  connection as soon as its length is read.
- **A peer that reads nothing** until 16 MiB waits for it is closed.

Each socket's version is 1. The lengths are shown in hexadecimal:

```text
-> 00 00 00 0d {"version":1}
<- 00 00 00 0d {"version":1}
```

### The surface socket

The shell reaches the Harness at the surface socket. In Base it is
`/run/crowbar-agent/surface`. The service makes its directory as root, owned
by the person so the Harness can bind there, with the shell's group and mode
2750, so the shell's account is the only other one that can enter.

The person's own programs can enter it too, since the Harness is one of them.
One of them that connects is refused by its user ID. One of them can also
remove the socket and bind its own there, and the shell would then be talking
to it: the shell takes what the surface socket sends as the person's, never
as the system's. That follows from
[ADR 0034](../docs/adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)
running the Harness as the person, with no isolation inside their account.
The [spec](https://github.com/crowbaros/crowbaros/issues/291) gives the
surface socket no message that answers an ask, so no yes rests on it.

It has no message yet. The slice that builds the Agent's surface
([#295](https://github.com/crowbaros/crowbaros/issues/295)) gives it the
person socket's conversation messages, so the drawer is a second client of
the same conversations. Until then, a frame after the version is one the
Harness does not speak, and it closes the connection.

### The person socket

The person's own clients reach the Harness at the person socket, the
terminal's [`crowbar-agent`](client/README.md) first. It is in the person's
runtime directory, at `crowbar-agent/person`: in Base,
`/run/user/<the person's user ID>/crowbar-agent/person`. The Harness makes
the `crowbar-agent` directory, with mode 0700, and the socket has mode 0600.
The Harness serves the person's user ID alone there.

Any program running as the person can connect, as the person's terminal does.
That is the same level of trust as the rest of their account (ADR 0034). The
person socket has no message that answers an ask, starts a share, or changes
the mode, so no yes rests on it either.

A client only relays. The conversations are the Harness's, and a turn goes on
whether or not any client is connected: **a client that goes away stops
nothing**, and only `stop` stops a turn.

## The person socket's messages

A request is one JSON object with an `op`. Each request gets one reply, in
the order the requests came, and the reply comes before any event the request
caused. An event may come at any other time.

A request the Harness will not carry out gets
`{"reply":"error","code":"...","message":"..."}`, and the channel goes on.
`message` is a sentence for the person. A frame that is not JSON, or has no
`op`, closes the connection.

| `code` | When |
| --- | --- |
| `request` | The Harness has no such request, or the request lacks a field |
| `no-conversation` | There is no conversation with that ID |
| `working` | The Agent is still working on the message before this one |
| `no-model` | No Model is signed in, or none is chosen |
| `signing-in` | A sign-in is already under way |
| `refused` | A sign-in, a sign-out, or a choice of Model that cannot be made, with why |

### Conversations

A conversation has an ID of the Harness's own: eight letters and digits, such
as `k3f9q2xz`. A turn is one message from the person and the Agent's answer
to it. Turns are counted from 1 within their conversation.

A client follows at most one conversation at a time: the one it last started
or resumed. It is sent that conversation's events, whichever client sent the
message, so every client of a conversation shows the same thing.

| Request | Reply | What it does |
| --- | --- | --- |
| `{"op":"start"}` | `{"reply":"started","conversation":"k3f9q2xz"}` | Starts a conversation, and follows it |
| `{"op":"list"}` | `{"reply":"conversations","conversations":[...]}` | Lists every conversation, the newest first. Each is `{"conversation":"k3f9q2xz","started":1795395526,"title":"How do I…","turns":2,"working":false}`: when it started, in seconds since 1970, how its first message begins, how many turns it has, and whether one is working |
| `{"op":"resume","conversation":"k3f9q2xz"}` | `{"reply":"resumed","conversation":"k3f9q2xz","earlier":false,"turns":[...]}` | Follows a conversation, and gives it as it stands: its newest turns, the oldest first, and whether it has turns before them. Each is `{"turn":1,"message":"...","answer":"...","state":"done"}`. `state` is `working`, `done`, `stopped`, or `failed`, and a failed turn has `error`, which says why. A turn that is working has its answer so far, and its events follow |
| `{"op":"send","conversation":"k3f9q2xz","text":"..."}` | `{"reply":"sent","conversation":"k3f9q2xz","turn":2}` | Sends a message of the person's, which begins a turn. The Model in use answers it |
| `{"op":"stop","conversation":"k3f9q2xz"}` | `{"reply":"ok"}` | Stops the turn that is working. It is answered once the turn has stopped. With no turn working, it stops nothing |

| Event | When |
| --- | --- |
| `{"event":"message","conversation":"k3f9q2xz","turn":2,"text":"..."}` | A turn began, with the person's message. The client that sent it gets this too |
| `{"event":"text","conversation":"k3f9q2xz","turn":2,"text":"..."}` | The next piece of the answer |
| `{"event":"retry","conversation":"k3f9q2xz","turn":2,"message":"...","seconds":2}` | The Model failed, and is asked again after that many seconds |
| `{"event":"done","conversation":"k3f9q2xz","turn":2}` | The turn ended, with the whole answer |
| `{"event":"stopped","conversation":"k3f9q2xz","turn":2}` | The turn ended because a client stopped it |
| `{"event":"failed","conversation":"k3f9q2xz","turn":2,"message":"..."}` | The turn ended because the Model gave no answer, with why in the Model's or its provider's words |

Here a client starts a conversation and sends a message. Each line is one
frame, without its length:

```text
-> {"op":"start"}
<- {"reply":"started","conversation":"k3f9q2xz"}
-> {"op":"send","conversation":"k3f9q2xz","text":"Hello, Agent."}
<- {"reply":"sent","conversation":"k3f9q2xz","turn":1}
<- {"event":"message","conversation":"k3f9q2xz","turn":1,"text":"Hello, Agent."}
<- {"event":"text","conversation":"k3f9q2xz","turn":1,"text":"Hello"}
<- {"event":"text","conversation":"k3f9q2xz","turn":1,"text":" there."}
<- {"event":"done","conversation":"k3f9q2xz","turn":1}
```

### Signing in

The Harness does the sign-in, through pi-ai's providers, and keeps the
credential. The client shows the Harness's questions and sends back what the
person types. One sign-in runs at a time. It belongs to the client that began
it, and ends when that client cancels it or goes away.

| Request | Reply | What it does |
| --- | --- | --- |
| `{"op":"login","provider":"anthropic"}` | `{"reply":"ok"}` | Begins a sign-in to that provider, by pi-ai's name for it. With no `provider`, the Harness asks which. The reply says only that it began: events carry the rest |
| `{"op":"answer","prompt":2,"text":"..."}` | `{"reply":"ok"}` | Answers a `prompt` event: what the person typed, or the `id` of the option they picked |
| `{"op":"cancel"}` | `{"reply":"ok"}` | Cancels this client's sign-in |
| `{"op":"logout","provider":"anthropic"}` | `{"reply":"signed-out","provider":"anthropic","provider_name":"Anthropic"}` | Signs out, and removes the credential. With no `provider`, the only one signed in to |
| `{"op":"status"}` | `{"reply":"status","model":{...},"signed_in":[...]}` | The Model in use, as `{"provider":"anthropic","provider_name":"Anthropic","model":"...","name":"..."}`, and each sign-in, as `{"provider":"anthropic","name":"Anthropic","how":"OAuth"}`, where `how` is `OAuth` or `API key`. With no Model in use, `model` is null and `missing` says why |
| `{"op":"models"}` | `{"reply":"models","models":[...]}` | Every Model of every provider signed in to, each as `status` gives the Model in use, with `in_use` |
| `{"op":"model","provider":"anthropic","model":"..."}` | `{"reply":"model", ...}` | Makes that the Model in use, and gives it as `status` does |

| Event | When |
| --- | --- |
| `{"event":"prompt","prompt":2,"kind":"select","message":"...","options":[{"id":"oauth","label":"..."}]}` | A question for the person. `kind` is `select`, to pick one of `options`; `text`; `secret`, which the client must not show as it is typed; or `code`, a code or an address to paste from a browser. All but `select` may have a `placeholder` |
| `{"event":"withdrawn","prompt":2}` | The question no longer needs an answer, as when the browser came back before the person pasted a code |
| `{"event":"browser","url":"...","message":"..."}` | An address to open in a browser. The client opens one when it runs on a desktop, and shows the address either way |
| `{"event":"code","code":"BD4A-FE0E","url":"..."}` | A code to enter at an address, for a sign-in from another device |
| `{"event":"notice","message":"..."}` | Something to show the person |
| `{"event":"signed-in","provider":"anthropic","provider_name":"Anthropic","model":"...","name":"..."}` | The sign-in ended, and the person is signed in. `model` and `name` are the Model in use, and are absent when none was chosen |
| `{"event":"sign-in-failed","cancelled":false,"message":"..."}` | The sign-in ended without one, with why |

A sign-in asks, in order: which provider, unless the request named one; how
to sign in, when the provider takes both an API key and OAuth; whatever the
provider's own flow asks; and which of its Models the Agent is to use, unless
it has only one or the person had already chosen one of them.

For OAuth, pi-ai's flow for the provider listens on a port of the machine for
the browser to come back to. Where the browser is on another machine, as over
SSH, it cannot, and the person pastes the address the browser ended at, or
the code the provider shows, in answer to the `code` question.

## Reaching a Model

- **The providers** are every one pi-ai has built in, loaded into the one
  bundled file. `crowbar-agent login` lists them.
- **Signed in** means the Harness holds a credential for the provider in
  `credentials.json`. pi-ai refreshes an OAuth token when it expires, inside
  the credential store, so two requests never refresh one token twice.
- **One Model is in use**, for every conversation: the one the person chose
  at sign-in or with `crowbar-agent model`. Each turn is answered by the
  Model in use when its message is sent, whichever answered the turns before.
- **With no Model in use**, `send` is refused with `no-model` and begins no
  turn, and `status` says why. The Harness goes on.
- **A Model that fails**, or cannot be reached, is asked again up to three
  times, with a `retry` event each time, and then the turn ends `failed`. The
  Harness goes on.
- **The Agent is told what it is** before every conversation, in
  `src/store.ts`: the Agent built into CrowbarOS, with no act yet.
- **Requests carry no attribution** to pi or to CrowbarOS.

### One endpoint, by hand

A file named `endpoint.json` in the state directory points the Harness at one
OpenAI-compatible endpoint, as the
[spec](https://github.com/crowbaros/crowbaros/issues/291) has it until
[#237](https://github.com/crowbaros/crowbaros/issues/237) decides local
Models:

```json
{ "url": "http://127.0.0.1:8080/v1", "model": "its-model", "key": "optional" }
```

While the file is there, its Model is the Model in use, whatever the person
is signed in to. The Harness reads the file at each use, so it counts from
when it is written until it is removed, with no restart. No client writes it.

### The stub Model

`test/stub-model.ts` is a Model for tests and checks: a server that speaks
the OpenAI-compatible chat API on `127.0.0.1` and answers from a script, so
each reply is known before it is asked for. It is one file that needs nothing
but Node.js, so the lab can copy it into a guest and run it with the Node.js
the image ships. No image holds it.

```sh
node test/stub-model.ts --script script.json --requests requests.jsonl
```

It prints `listening http://127.0.0.1:PORT/v1`, which goes in
`endpoint.json` as `url`, with any `model`. The script is a JSON list of
replies, given in order, and read again at each request:

| Reply | What the stub does |
| --- | --- |
| `{"text":"Hello there."}` | Answers with the text, a word at a time |
| `{"text":"One two","pause_ms":50}` | The same, with a pause before each word after the first |
| `{"text":"One two","stall":true}` | Sends the first word, and then nothing until the Harness goes away: a turn to stop |
| `{"status":401,"message":"No such key"}` | Fails as an API does |

A request past the script's end fails, and says so. `--requests` gets each
request's body, a line of JSON for each, so a check can read what the Harness
sent the Model.

## Upgrading Pi Durable or Node.js

Pi Durable calls itself experimental and has broken its API between releases,
so its version in `package.json` is exact, as is every other dependency's, and
`package-lock.json` pins what they bring. An upgrade is a pull request of its
own, and runs the desktop check (ADR 0034).

Node.js is pinned in [`base/config.json`](../base/config.json), by address and
SHA-256: the official Linux x64 binary, on the newest LTS line the pinned Pi
Durable supports. The checksum is the one in that release's `SHASUMS256.txt`
on nodejs.org.
