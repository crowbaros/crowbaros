# Every compartment has its own user and sandbox, and only the Warden starts one

**Status:** proposed. This is a draft that grows as the design session for
[issue 39](https://github.com/crowbaros/crowbaros/issues/39) settles each part.

The [compositor spike](../research/compositor-spike-2026-11-15.md) identified a client by how its Wayland connection was made, and then ran the compositor, the shell, and every client as one user. A process of that user that could trace the shell could copy its sockets and become the shell. So the rules of [ADR 0014](0014-a-yes-comes-from-the-person.md) held inside Wayland and nowhere else. This record says what keeps one principal's processes from another's, and from the trusted parts, outside Wayland.

1. **A compartment is the unit that is isolated.** Each compartment gets a kernel user ID of its own and a sandbox of its own when it starts: namespaces, Landlock, seccomp, and a cgroup. No two live compartments share either, even when they belong to one principal. A live Tool and its Preview are one principal in two compartments, and the Preview's unconfirmed code cannot trace or signal the live build.
2. **The sandbox is the main control and the user ID is the second.** The sandbox decides what a compartment can see ([ADR 0004](0004-capability-safe-tools.md)). The user ID makes the kernel's own checks on tracing, signals, process memory, and socket permissions refuse one compartment reaching another, so a mistake in sandbox setup does not expose everything.
3. **A compartment has two names.** The **principal ID** is stable, and it is what authority binds to. The **compartment ID** is new at each start. The broker and the compositor know a compartment by both.
4. **Only the Warden starts a compartment.** The Warden is a small Base program that runs with root privileges. It creates a compartment's user ID, sandbox, cgroup, and connections, and it stops the compartment. It takes orders from the broker alone. The shell asks the broker to start installed software, so the gate sees every start.
5. **The trusted parts are separate users that start nothing.** The compositor, the shell, and the broker each run as a system user of their own, as services that runit starts and restarts. None of them runs as the person, and none of them starts a principal's process. The compositor starts no process at all.
6. **The compositor has no public socket.** No process can find a socket and become a client. Every connection is made for one named compartment by the Warden, and the compositor never takes a client's identity from the client.

## Considered Options

- **A sandbox alone, with every compartment under the person's user ID.** Rejected: it needs no privileged part, but the kernel then sees one user, and tracing, signals, and `/proc/<pid>/mem` are refused only by sandbox setup being right every time.
- **A user ID alone.** Rejected: it gives no filesystem view and no cut from the network, which ADR 0004 needs.
- **One user ID and sandbox for each principal.** Rejected: a Preview would share them with the live build of the same Tool, which holds the Tool's standing grants.
- **The broker starts compartments itself.** Rejected: the broker reads what arrives from the network, so it should not hold root.
- **The compositor or the shell starts clients, as in the spike.** Rejected: the compositor holds the seat and the GPU, and what it starts inherits from it. The shell is a large program, and a start it makes is one the gate does not see.
- **A public socket that offers the smallest set of globals.** Rejected: it leaves a way for a process with no compartment behind it to draw a window, and [ADR 0015](0015-our-own-compositor-on-smithay.md) has the compositor know which principal owns each surface.
