# Installed software comes from a pinned catalog, and dev tools run in a workbench

**Status:** accepted on November 15, 2026, by Jim Martin, at the end of the design session on installing software and dev tooling.

Installing software must be painless in CrowbarOS, and dev tools are the hardest case: they read and write a project all day, run each other, reach package registries and model APIs, and need root inside their own world. The vision made installed software first-class but gave it no way to arrive. This record says where installed software comes from, how dev tools run, and how installed software writes, reaches the network, and holds a login. It also fills the "later record" that [ADR 0016](0016-the-warden-starts-every-compartment.md) leaves open for a wide compartment's network and writes. T3 Code, a desktop app that runs coding agents, is the worked example and the first acceptance test.

## What painless means

This is the bar the work is accepted against:

- The person approves once per app, or once per workbench, when it is installed or made.
- An update goes live without asking, unless it asks for new access.
- Everyday use never asks about one site, one file, or one tool.
- From the ask to a working app from the catalog takes under a minute.

There is no standing developer mode. When the person wants something opened up, a wide compartment (ADR 0016) does it for one start and one touch of a key. If confined software is not painless, the confinement is fixed, and no bypass is added.

[ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md) makes this the rule for the whole OS and gives it numbers. Revised after council round 5: a project is private or public, a host can be added for one run, and the catalog carries the two lists the browser rule needs.

## The catalog

- **CrowbarOS keeps its own catalog.** It is the one place where an install is recorded and pinned. An entry names the source, pinned by hash; the needs the person sees; the sandbox profile; and what the app needs from the GPU and from portals. Rolling back installed software is a change to which entries are active.
- **Sources, in order of preference:**
  1. **nixpkgs** at a locked snapshot. It is the main source: the largest package set, every build pinned by hash, and it covers every app in our first list, T3 Code included.
  2. **Vendor artifacts**: an AppImage, a tarball, or a `.deb`, pinned by sha256, unpacked once at install, and run on a pinned runtime. FUSE never enters a sandbox.
  3. **Flathub** and **distro images** (OCI layers) are added only when a real app needs them. A Flatpak's declared permissions are input to the approval screen, never the rules.
- **The Warden launches everything.** No app starts through `flatpak run` or a Nix wrapper. The catalog's metadata feeds the needs. The broker and the sandbox enforce them.
- **The project publishes catalog snapshots**, and a machine follows them by default. A pin holds only while its artifact exists, so the project mirrors what it pins.
- **The person can add a private entry** for anything: a vendor URL with its sha256, or a nixpkgs package the project catalog leaves out. The person or their Agent writes its needs, and the person confirms once. The approval screen says the entry was not reviewed by CrowbarOS. This is what makes "there is always a way" true.

## Installing and updating

- **Two ways in, one approval.** The person asks the Agent ("install T3 Code"), or picks the app in the catalog screen the shell draws. Both end at the same trusted approval screen, which shows the app's needs in fixed words. Then the install goes through fork, preview, promote ([ADR 0007](0007-fork-verify-promote.md)).
- **The approval is a signature, and a list takes one.** An app's needs are standing authority, so its approval is a signature ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)). Several apps the person chose, such as the fifteen they want on a new machine, are shown in full, each with its needs, and take one signature.
- **The approval is the app's standing grant and standing rule.** Using what the approval screen showed never asks ([ADR 0011](0011-task-principals.md)).
- **An update that asks for nothing new goes live by itself.** The person sets a standing rule once: an update to an installed app is promoted without asking when it asks for no new access and its checks pass in a fork. An update that asks for more waits for the person. This is the one standing exception to "one confirmation promotes", and the ledger records each promote it makes.

## The workbench

A **workbench** is a changeable Linux system for one project's dev tools.

- **One workbench for each project, made from a profile.** A profile such as "web dev" lists the folders, the sites (package registries, GitHub, the model APIs), and the needs. The person confirms it once. A dependency that goes bad in one project cannot reach another project's files or logins, and marks stay inside one project.
- **A project is private or public, and the person says which when they make its workbench.** A private project's workbench reaches only the sites on its list. A public project's workbench holds only public data and no login of its own, so it may reach any host on the open internet without asking ([ADR 0011](0011-task-principals.md)). Secrets stay in the vault, and the broker fills them in only for the hosts they are tied to. A workbench that holds its own login, declared as below, is not public in this sense.
- **Declaring a project public gives its folders the public label.** That is a declassify, and it is part of the profile the person signs. From then on, a write into those folders from a compartment above public is refused, unless the person's own hand moves the value in ([ADR 0025](0025-what-the-person-does-by-hand-is-the-answer.md)).
- **A host the profile does not list is added for this run with the confirm gesture.** It lasts until the workbench stops. Adding it to the profile for good takes a signature, and one signature can cover the hosts the person added during the week (ADR 0026).
- **A workbench is a principal, and each opening is a compartment.** Its system files live on a subvolume owned by the storage user. Each time it opens, the Warden starts a new compartment and maps that subvolume in. Snapshots of the subvolume are its undo. The Crowbar State records the workbench's profile and grants, not what is installed inside it.
- **It has a block of user IDs, and it starts as root inside.** Package managers and containers need files owned by many users, so the Warden gives a workbench a block of user IDs and writes the user namespace map itself. Blocks never overlap between live compartments, and the subvolume is stored under a block on disk. Every compartment has `no_new_privs`, so `sudo` and setuid programs do nothing inside. The workbench starts as root in its own namespace instead.
- **Its stated needs come from its profile.** Usually that is a user namespace (root inside, rootless podman, the browser sandbox in Electron and Chromium), tracing inside itself (debuggers), the GPU, and sometimes `/dev/kvm`. The person sees them once, on the approval screen.
- **Installing inside a workbench never asks.** `apt`, `npm`, `cargo`, `uv`, `mise`, and podman work as on any Linux machine, inside what the workbench was granted.
- **Dev apps with a window run inside their workbench.** T3 Code, an editor, and a dev server's browser window come from the catalog, are installed into the workbench, and reach the compositor through its one Wayland listener. They share one principal, one compartment, and one mark. A catalog app outside the workbench that reaches in would need an authority boundary between the two, and we would rather not design one.
- **A workbench is always marked.** It reads the open web all the time. So a value the person pastes into it changes nothing: a paste is never refused, and the workbench already carries the mark ([ADR 0025](0025-what-the-person-does-by-hand-is-the-answer.md)). Its standing grants bind to its principal and the exact needs of its profile, with no clean-build condition ([ADR 0011](0011-task-principals.md)). A new need asks the person again.
- **Standing rules let it cross.** When the person makes a workbench, they can write standing rules such as "may push to `crowbaros/crowbaros`, except to `main`", carried out by a broker adapter that knows git and GitHub operations. An act no rule covers asks, with its exact content shown.
- **It is not wide.** A workbench sees only the folders it was granted and never the vault's values. For a private project it reaches only the sites on its list. The person can start it wide for one debugging session.

## Writes

- **Installed software and workbenches write live.** An editor, T3 Code, and a dev server write into the folders they were granted as they work, and other programs read the result at once. Btrfs snapshots of each granted folder, and the Data undo window, protect the person.
- **Tasks still stage.** A task's writes to Data are staged in its workspace and committed when it finishes, as ADR 0007 says. A wide task stages too. The rule depends on the kind of compartment, not on how much it was given.

## The network

A compartment never has a network interface ([ADR 0004](0004-capability-safe-tools.md)). Every path out is a socket the broker hands in:

- **HTTP and HTTPS** go through a broker proxy. Each sandbox trusts a certificate of its own, so the broker makes the outbound connection itself and puts a secret in only for the host it is tied to ([ADR 0006](0006-broker-owns-all-egress.md)). A granted host the broker cannot see inside, such as an app that trusts only its own server's certificate, is relayed without inspection. It is checked by destination, not by value.
- **SSH** goes through a broker ssh-agent socket, so private keys never enter a sandbox, and the broker makes the TCP connection.
- **Other TCP** is relayed by the broker to a granted host and port.
- **UDP is not carried yet.** Voice and video calls, WebRTC, and many games do not work. This is a known gap.

A wide compartment reaches the network the same way, with every destination granted. The ledger records its egress like any other.

## Logins an app holds itself

- **Adapters come first.** The broker gets adapters for Anthropic, OpenAI, and GitHub first, including their sign-in flows. With them, Claude Code, Codex, and `gh` work with no credential inside the sandbox.
- **An app may hold its own login, declared.** Where no adapter exists, or an app can only work with its own credential, the approval screen says in fixed words "this app holds its own login to X". That compartment reaches only its granted destinations. Code inside it, including code an agent wrote, can read the credential. That is the price, and it is shown.

## GPU

- **Base provides the GPU's user-space drivers.** Mesa, and the NVIDIA libraries that match the kernel module, are built with Nix and offered at `/run/opengl-driver` to every sandbox whose needs include the GPU. They are pinned with the Base generation, so one driver upgrade in Base serves every app source.
- **The GPU need is more than one device.** On NVIDIA it is several device nodes and the driver folder, not only the render node.

## Considered Options

- **Flathub as the main source.** Rejected: T3 Code and Cursor are not on it. For VS Code, Chrome, Slack, Spotify, and Steam, the Flathub entry is a community wrapper that downloads the vendor's binary at install, outside the pin. Chrome and many Electron apps there expect Flatpak's own spawn service.
- **Void's xbps for apps.** Rejected: xbps builds Base only, and its packages live in one root.
- **A curated catalog filled by hand.** Rejected as the source: it would be small and slow to grow. The catalog is ours, but nixpkgs fills most of it.
- **A standing developer mode with less confinement.** Rejected: agents would run with the person's whole authority, and ADR 0004's promise that installed software follows the rules of Tools would end. A wide compartment covers the need one start at a time.
- **Each dev tool as its own sandboxed app.** Rejected: an editor runs git, a language server, a compiler, and tests, so each would need what the others have. The result is a workbench, built badly.
- **One workbench for all of a person's projects.** Rejected: fewer approvals, but one bad package reaches every project.
- **Pinned workbenches, rebuilt from a spec on every change.** Rejected: every `apt install` would become an edit and a rebuild.
- **Staging every write and committing often.** Rejected: complex, and tools would still see stale files.
- **A network interface with a route for pass-through.** Rejected: it breaks ADR 0004 and ADR 0016. A broker relay does the same job.
- **Adapters only.** Rejected: an app with no adapter would not work, and "there is always a way" would be false.
- **Letting an app hold a login freely.** Rejected: the person would not know which compartments hold credentials.
- **A signature for every host a workbench gains.** Rejected by round 5: a build that needs one more registry for one afternoon cost a passkey. The confirm gesture adds it for the run.
- **Every workbench reaches only its list.** Rejected for a public project: nothing private is inside to leak, and each new host cost an ask.

## Consequences

- **A real key the person typed or pasted into a file in a public project can be sent anywhere by an injected coding agent.** The vault is where a key belongs, and the broker fills it in only for its host.
- The catalog carries two lists for task browser contexts, shared hosts and adapter-only sites ([ADR 0013](0013-agents-never-drive-the-persons-browser.md)).
- **[ADR 0006](0006-broker-owns-all-egress.md)** gains the declared exception for an app that holds its own login.
- **[ADR 0007](0007-fork-verify-promote.md)** names two kinds of writer, and the standing rule that promotes updates which ask for nothing new.
- **[ADR 0011](0011-task-principals.md)** gains what a workbench's standing grants bind to.
- **[ADR 0016](0016-the-warden-starts-every-compartment.md)** gains blocks of user IDs, broker connections in the plural, the full GPU need, `/dev/kvm` as a need, and workbenches in its lists. Its "later record" points here. [Issue 144](https://github.com/crowbaros/crowbaros/issues/144) keeps only how a wide compartment's windows are marked and its attacks.
- A wide compartment's label and mark follow [ADR 0008](0008-private-values-stay-out-of-the-agent-context.md) unchanged: it is at the label of everything it reads, and it reaches only parties cleared for that.
- Attack 17 in ADR 0016, the four things a wide compartment never gets, also runs against a workbench that holds every need.
- One workbench reaches the compositor as one identity. A rule on focus, clipboard, or capture for one principal treats T3 Code and the editor beside it as one.
- Catalog apps expect `xdg-desktop-portal` and a session bus. Both run inside the compartment (ADR 0016), the portal talks to the broker, and the shell draws the chooser. This is work for issues [63](https://github.com/crowbaros/crowbaros/issues/63) and [112](https://github.com/crowbaros/crowbaros/issues/112).
- Nix becomes something CrowbarOS ships for the Environment and for drivers. Where its store lives, and how it is collected, is for the catalog work to decide. Whether a catalog snapshot counts as admitted input is a separate decision.
- Browser engines inside a compartment need a user namespace of their own, so it is a stated need for any app built on Electron or Chromium.
