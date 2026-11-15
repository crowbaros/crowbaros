# Every compartment has its own user and sandbox, and only the Warden starts one

**Status:** proposed. This is a draft that grows as the design session for
[issue 39](https://github.com/crowbaros/crowbaros/issues/39) settles each part.

The [compositor spike](../research/compositor-spike-2026-11-15.md) identified a client by how its Wayland connection was made, and then ran the compositor, the shell, and every client as one user. A process of that user that could trace the shell could copy its sockets and become the shell. So the rules of [ADR 0014](0014-a-yes-comes-from-the-person.md) held inside Wayland and nowhere else. This record says what keeps one principal's processes from another's, and from the trusted parts, outside Wayland.

## Isolation

- **A compartment is the unit that is isolated.** Each compartment gets a kernel user ID of its own and a sandbox of its own when it starts: namespaces, Landlock, seccomp, and a cgroup. No two live compartments share either, even when they belong to one principal. A live Tool and its Preview are one principal in two compartments, and the Preview's unconfirmed code cannot trace or signal the live build.
- **The sandbox is the main control and the user ID is the second.** The sandbox decides what a compartment can see ([ADR 0004](0004-capability-safe-tools.md)). The user ID makes the kernel's own checks on tracing, signals, process memory, and socket permissions refuse one compartment reaching another, so a mistake in sandbox setup does not expose everything.
- **A compartment has two names.** The **principal ID** is stable, and it is what authority binds to. The **compartment ID** is new at each start and is never used twice, so a ledger entry names one start. The broker mints both, and the broker, the Warden, and the compositor know a compartment by them. The kernel user ID is the Warden's means of enforcing and never leaves it.
- **A compartment's user ID is temporary.** It comes from a range Base reserves and is not used again until the machine restarts. It owns nothing that outlives the compartment. Data on disk belongs to a Base storage user, and a compartment sees the view it was given through an ID-mapped mount.
- **No process runs as the person.** There is no login account. A terminal the owner opens is a compartment with a view. Owner authority is root, taken deliberately, outside every compartment.

## The Warden

- **Only the Warden starts a compartment.** The Warden is a small Base program that runs with root privileges. It creates a compartment's user ID, sandbox, cgroup, and connections, and it stops the compartment. It takes orders from the broker alone. The shell asks the broker to start installed software, so the gate sees every start.
- **The Warden owns a cgroup v2 subtree, with one cgroup for each compartment.** The cgroup carries the compartment's budget of memory, CPU, I/O, and process count. The Warden stops a compartment with `cgroup.kill`, which ends every process in it, including one that forked twice to get away. This is what "stopping its sandbox" means in ADR 0004. Base mounts cgroup v2 only.

## The trusted parts

- **The compositor, the shell, the broker, and the Warden are four runit services.** The first three each run as a system user of their own. runit starts and restarts them, and supervises nothing else: a compartment is not a runit service. runit does not need to give limits for each principal, which was an open question in the vision.
- **None of them starts a principal's process except the Warden.** The compositor starts no process at all.
- **A fifth trusted part follows the same rules.** The gate, the vault, the runner, and the plan interpreter may live inside the broker. One that becomes a process of its own gets a system user, a runit service, and a path socket with the check below.
- **Everything else is a compartment.** The Agent's harness runs in one, with a broker connection like any other. The WASM runtime runs inside the compartment of the Tool it runs.
- **They find each other by path sockets that no sandbox contains.** The sockets are in a Base runtime directory. Each is in a directory that only its two ends can enter, and the listener checks the peer's user ID as the kernel reports it. Any part can restart and reconnect, in any order. The spike's socket pair for the shell is gone, and a control socket of the shell's own gets the same check or is removed.

## Wayland connections

- **The compositor has no public socket.** No process can find a socket and become a client. The sockets of the trusted parts are the one exception, and there the identity comes from the kernel.
- **The Warden makes every compartment's connection.** It holds a connection of its own to the compositor, and it is the only client offered `wp_security_context_manager_v1`. For a compartment it registers one listening socket, with the principal ID as the app ID and the compartment ID as the instance ID, and binds that socket into that compartment's sandbox alone. The compositor never takes a client's identity from the client.
- **No compartment can register a security context.** A program that sandboxes its own children gets no weaker connection for them. Its children are in its compartment and carry its identity.
- **The Warden tells the compositor who a compartment is and nothing more.** What kind of thing it is, such as a Preview, reaches the compositor through the shell's interface ([issue 55](https://github.com/crowbaros/crowbaros/issues/55)).

## What a compartment starts with

- **A broker connection and nothing else.** The Warden closes every other descriptor before the program runs, and every trusted part opens its own descriptors close-on-exec.
- **A descriptor arrives later only from the broker**, over that connection: a file the person chose in the powerbox, for example. It is part of the compartment's view, and stopping the compartment is the only way to take it back (ADR 0004).
- **A Wayland listener only when the broker orders one.** A Tool gets none, because the shell draws a Tool's UI from its description. Installed software, a terminal, and a task browser context that is shown get one. Most compartments are not Wayland clients.
- **The GPU render node is a need like any other.** It is absent from a sandbox unless the principal's stated needs include it, and the person sees it among the needs. Without it a client draws through shared memory. A render node lets a program call the GPU driver directly, which is a large part of the kernel to expose.
- **What a program expects to share runs inside its own sandbox.** A D-Bus session bus, an accessibility bus, or an X server that a program needs is started in that compartment, under its user ID, and reaches nothing outside it. Base has no session bus and no shared X server. An X11 app's bridge reaches the compositor through its compartment's listener, so it has that compartment's identity and no more. The only things that cross a sandbox boundary are the broker connection and the Wayland listener.

## The seat and tracing

- **The `_seatd` group has one member, the compositor's user.** The compositor is in neither `input` nor `video`, and gets its devices from seatd. It starts nothing, so nothing inherits the group. The spike's record said to put the person in `_seatd`. This replaces that.
- **Tracing is refused by the user ID, the PID namespace, and seccomp.** `ptrace_scope` is pinned at 1 as a third layer, and the design does not rest on it. A higher value adds nothing between compartments, whose user IDs already differ, and it would stop a debugger inside a developer's own compartment. Tracing inside one compartment is a stated need that the person sees, like the render node and a user namespace.

## What Base sets and checks

Base sets each of these. A check at boot verifies them, and a generation that fails one is not healthy.

- Every compartment has `no_new_privs` and its own PID, IPC, network, and mount namespaces.
- seccomp removes the tracing and process-memory calls, unless that compartment's stated needs include tracing inside it.
- No sandbox's `/dev` holds an input device or `uinput`. `/dev/uinput` can be opened by root only, and the `input` group has no members.
- `_seatd` has its one member.
- `ptrace_scope` is 1 or higher.
- No compartment can create a user namespace unless its stated needs include one. A new user namespace gives a process capabilities inside it, and many kernel privilege bugs start there. A browser's own sandbox is the usual reason to need one.
- No session bus, `/tmp`, or runtime directory is shared between compartments.

## A wide compartment

The owner can give one compartment everything they have: all of their Data, the network, tracing inside itself, and the devices they name. It keeps its own user ID and cgroup. It can damage or leak the owner's Data, and the owner accepted that.

Whatever the owner gives, no compartment gets what the trusted parts hold:

- the input devices and the seat;
- the sockets and the memory of the compositor, the shell, the broker, and the Warden;
- the values in the vault;
- a way to write the authority domain or the ledger.

So a wide compartment cannot answer its own ask, grant itself more, or hide what it did. Above it there is only owner authority, which the owner uses by hand and no agent holds.

**Wide is granted to one compartment at one start.** It is a change to authority, so it needs a passkey, a security key, or the phone (ADR 0014, rule 3). The approval signs what was shown: the principal, the objective, and when it ends. It ends when the compartment ends. It does not carry to the next start, and it cannot be a standing grant. A task, a terminal, or installed software can be wide. The Agent's own context cannot ([ADR 0011](0011-task-principals.md)).

This record fixes only that floor and that grant. A later record decides how a wide compartment reaches the network, whether its writes to Data are staged, what label and mark its output carries, and how its windows are marked.

## Milestone 2 and Base

There is one Warden, and it runs as root in both places. Milestone 2 still needs nothing from Base.

| | Needs |
| --- | --- |
| Milestone 2, from stock Linux | Root for the Warden, cgroup v2, a kernel with Landlock and `cgroup.kill`, and a range of user IDs from configuration |
| Base only | The system users and runit services of the trusted parts, the rules for the seat and the devices, the check at boot, and the Wayland connections |

Milestone 2 runs the Warden with `sudo` in CI, and inside the Void lab guest on a developer's machine, so nobody needs root on their own desktop.

## Attacks the tests must refuse

Each of these is a test, and each must be refused. The first ten run on stock Linux with [issue 68](https://github.com/crowbaros/crowbaros/issues/68). The rest need Base and the compositor, and run with issues [49](https://github.com/crowbaros/crowbaros/issues/49), [82](https://github.com/crowbaros/crowbaros/issues/82), and [136](https://github.com/crowbaros/crowbaros/issues/136).

1. A compartment traces, or reads the memory of, a process in another compartment, and of the broker.
2. A Preview signals the live compartment of the same Tool.
3. A process forks twice and starts a new session to outlive a stop. After `cgroup.kill`, no process with that user ID remains.
4. A compartment runs a setuid program from its view. It gains nothing.
5. A compartment lists its descriptors at start. It finds the broker connection and no other.
6. A compartment is given the path of a trusted part's socket on purpose. It is refused on its user ID. This tests the second check without the first.
7. A fork bomb and a program that takes all the memory reach their budget. The broker and the other compartments keep running.
8. A compartment tries to make a user namespace, a mount, and a second network interface.
9. Two compartments try to meet through an abstract socket, System V shared memory, and `/tmp`.
10. A compartment leaves files and a process behind. The next compartment gets a different user ID and can reach none of it.
11. A compartment with no listener looks for any Wayland socket. It finds none.
12. A client on a listener tries to bind the security-context manager, layer shell, and each input and capture global.
13. A compartment opens `/dev/uinput`, an input device, and the seatd socket: first by looking for them, then with the path given.
14. Installed software and its children are checked for the `_seatd` group and for any descriptor of a trusted part.
15. A process that does not have the shell's user ID connects to the shell's socket, and one that is not root connects to the Warden's.
16. A compartment is stopped. Its windows are gone and its listener accepts nothing.
17. A wide compartment tries each of the four things it never gets.
18. Each Base setting is changed in turn: a second member of `_seatd`, a `uinput` that others can open, `ptrace_scope` at 0. The check at boot fails the generation each time.

## Considered Options

- **A sandbox alone, with every compartment under the person's user ID.** Rejected: it needs no privileged part, but the kernel then sees one user, and tracing, signals, and `/proc/<pid>/mem` are refused only by sandbox setup being right every time.
- **A user ID alone.** Rejected: it gives no filesystem view and no cut from the network, which ADR 0004 needs.
- **One user ID and sandbox for each principal.** Rejected: a Preview would share them with the live build of the same Tool, which holds the Tool's standing grants.
- **A login account for the person that owns their Data.** Rejected: any process that reaches that user ID holds all of the Data.
- **The broker starts compartments itself.** Rejected: the broker reads what arrives from the network, so it should not hold root.
- **The compositor or the shell starts clients, as in the spike.** Rejected: the compositor holds the seat and the GPU, and what it starts inherits from it. The shell is a large program, and a start it makes is one the gate does not see.
- **A public socket that offers the smallest set of globals.** Rejected: it leaves a way for a process with no compartment behind it to draw a window, and [ADR 0015](0015-our-own-compositor-on-smithay.md) has the compositor know which principal owns each surface.
- **A private channel from the Warden to the compositor that passes descriptors.** Rejected: it can carry exact fields, but it is new protocol code in the trusted base, and Smithay already implements the security-context protocol.
- **One root parent that makes socket pairs and starts the trusted parts.** Rejected: when one part restarts, the parent has to connect the others again.
- **A Warden mode that needs no root, built on user namespaces.** Rejected: it is a second way to start a compartment, with weaker guarantees than the first, and the Ubuntu release that CI runs on restricts those namespaces.
- **The render node for every compartment that has a Wayland listener.** Rejected: a terminal would get direct access to the GPU driver for nothing.
- **One session bus for the desktop.** Rejected: it is a way for one compartment to reach another outside Wayland and outside the broker.
- **Owner authority for an agent, as the way to open everything.** Rejected: nothing the system promises holds after it, including that a "yes" came from the person. A wide compartment gives an agent task the person's Data without giving it the trusted parts.
- **A standing grant that makes a task wide at every start.** Rejected: it saves a touch of the key, and one bad run then reaches everything with the person not there.
- **A runit service for each compartment, with `chpst` limits.** Rejected: those limits are for each process, with no budget for the group and no way to end a process that forked twice.

## Consequences

- The production compositor has no flag and no build option that opens a public socket. Its tests register through the channel the Warden uses, with a stand-in for the Warden. The spike's harness and probe client carry over ([issue 49](https://github.com/crowbaros/crowbaros/issues/49)).
- The Alpha desktop lands before the Warden does ([issue 136](https://github.com/crowbaros/crowbaros/issues/136)). Until then it meets three rules. The compositor runs as its own user and is the only member of `_seatd`. The shell is a runit service under its own user. Apps run as a third user, started by a small root stand-in for the Warden, so they inherit nothing from the shell or the compositor. The spike compositor keeps its public socket until issue 49 replaces it, and the session's record says that one app can still reach another.
- There is no login. How the person unlocks the machine belongs to the session lock and to first run (issues [87](https://github.com/crowbaros/crowbaros/issues/87) and [100](https://github.com/crowbaros/crowbaros/issues/100)).
- Software that expects a desktop's shared services finds none. Whether portals and assistive software are offered, and how, stays with issues [63](https://github.com/crowbaros/crowbaros/issues/63) and [112](https://github.com/crowbaros/crowbaros/issues/112).
- The check at boot is part of a generation's health ([issue 76](https://github.com/crowbaros/crowbaros/issues/76)).
- The Warden is new code that runs as root. It is in the trusted base, and it gets adversarial tests like the gate does.
