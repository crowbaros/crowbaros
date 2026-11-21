# Pi Durable runs the Agent, behind a protocol CrowbarOS owns

**Status:** proposed on November 21, 2026, by Jim Martin.

The [vision](../VISION.md) left open whether we write our own harness or run an existing one. Milestone 3 needs a Harness that keeps a conversation going while the Agent's surface is put away, parks an act until the person's yes, and comes back from a crash, a suspend, or a restart without running an act twice. It also has to reach Models through API keys, OAuth, and local servers.

Research for [#231](https://github.com/crowbaros/crowbaros/issues/231) read the candidates at their sources on November 18, 2026. [Pi Durable](https://github.com/earendil-works/pi) (`@earendil-works/pi-durable`, MIT) has all of this today: a single daemon that owns a SQLite store, acts that park durably before they run, an interrupted act that is reported and never run again on recovery, more than 30 providers, 9 OAuth flows, local OpenAI-compatible servers, and image input. It has two costs. It needs Node.js 22.19 or newer, in a Base where everything of ours is Rust. And it calls itself experimental: six of its releases in 40 days broke its API. The findings are on the `research/pi-durable-harness` branch.

## The decision

- **Pi Durable is the Harness.** It is pinned to an exact version. An upgrade is its own pull request, and runs the desktop check.
- **CrowbarOS owns the protocol around it.** The Harness talks to Fulcrum and to the Agent's surface over a protocol we define. No Pi Durable name, type, or ID crosses it. Fulcrum, the shell, and the root helper never depend on Pi Durable, so the Harness can be upgraded or replaced without changing them. What the protocol carries is settled in the Milestone 3 spec, not here.
- **The Harness runs as the person's account**, in one process. It has no account of its own and no helper to carry out the person's acts. This follows the rule that the person's account is one level of trust and the desktop adds no isolation inside it.
- **Node.js ships beside the Harness.** The build downloads the official Linux x64 binary at an exact version and checks its SHA-256, as it does for the static xbps. It is the newest LTS line the pinned Pi Durable supports. It is not on `PATH`, and Void's `nodejs` package is not added to Base. npm runs only in the build.
- **Our code lives in `agent/`.** TypeScript is used only for what must run inside Pi Durable: the bridge to the protocol, the fixed rules, the tool definitions, and the credential store. CI type-checks it, tests it, and bundles it into one file. Everything on the other side of a socket is Rust.
- **The fixed rules cover every conversation and background task.** One place in the Harness creates them, always with the rules, and the code that carries out an act checks the rules again. A test that starts a task without them must fail. Pi Durable applies rules only to a conversation set up with them, so this is ours to hold.
- **The Harness's state is the person's data.** The database, the transcripts, and the credentials live in the person's home, readable by the person alone. A rollback of Base never touches them.
- **A rollback of Base never leaves the Agent unable to start.** Pi Durable migrates its database forward only, and refuses one newer than itself. So the Harness copies the database before each migration and keeps one copy for each schema version. An older Harness that finds a newer database sets it aside untouched, opens the copy, and tells the person in the Agent's surface that the conversations since the upgrade are set aside. Nothing is merged later.
- **The database keeps Pi Durable's default sync setting.** It is safe when the Harness crashes, and can lose its last commits when power is cut.
- **No prototype comes first.** The first slice of the work runs the Harness under runit in the VM and writes a dated record of its idle memory and its start time. This decision is reopened if the idle Harness holds more than 150 MiB, takes more than 2 seconds from start to answering on its socket, or loses a parked ask when it is killed and restarted.

## Considered Options

- **Our own Harness in Rust.** One language, a daemon of 5 to 15 MiB, and full control of the database. Rejected for Milestone 3: we would build the provider adapters, the OAuth flows, crash recovery, compaction, and the parking of asks before a person could use anything. The protocol keeps this open for later.
- **Goose.** Rust, so nothing new in Base. Rejected: it has no built-in OAuth for the main providers, and neither Pi Durable's parking of an act nor its refusal to run an interrupted act again.
- **OpenAI's Codex app server.** Rust. Rejected: it is built around one provider's accounts.
- **OpenCode.** Rejected: it needs Bun, which Void does not package.
- **The Harness in its own account**, with a helper running the Agent's acts as the person, as the research proposed. It would keep credentials and the database from commands the Model runs. Rejected: it is isolation inside the person's account, which the desktop does not otherwise have, and the yes for an act as root does not rest on the Harness.
- **Void's `nodejs` package.** Nothing of ours to maintain. Rejected: it ties the Harness to the Node.js version Void ships, and Pi's own app has already moved to Node.js 26.
- **The Harness's state rolls back with Base.** The Harness and its database would always match. Rejected: a rollback would erase every conversation and sign-in since the snapshot.

## Consequences

- Any program running as the person can read the stored API keys and OAuth tokens.
- Any program running as the person can pose as the Harness to Fulcrum: it can act on windows, and take captures while an allowance is on.
- While YOLO mode is on, any program running as the person can get root through the root helper. Outside YOLO mode an act as root still needs the person's yes, and that is the one thing the OS enforces.
- After a power cut, a yes just given may be asked for again. The record that an act began may be lost, so the Model may request the act a second time; as root, that needs a new yes.
- Base grows by one Node.js binary, about 55 MiB, and the repository gains a third language. A Node.js security release is ours to pick up, by changing one version and one checksum.
- Base's rollback, when it is built, must leave the person's home alone.
- After a rollback the person sees the conversations from before the upgrade. The newer ones stay on disk and come back when Base moves forward again, without what was said in between.
