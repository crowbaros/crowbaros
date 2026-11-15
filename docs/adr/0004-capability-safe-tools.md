# The Tool contract is the broker protocol; WASM is one runtime

A **Tool** is capability-safe because it starts with nothing and gets everything through the capability broker: Data, network, devices, and time. That contract is the same for every runtime. A Tool can be a WASM component, or a native process in a Linux sandbox (namespaces, Landlock, seccomp) that holds only a broker connection. Native Tools are routine, not an escape hatch. Installed software runs under the same sandbox and broker.

WASM stays available for what it is good at: deterministic execution and replay, and one build that runs on an x86 laptop and an ARM inference machine.

**Native code is promised only what Linux can enforce.** A native sandbox is a compartment whose label and mark are fixed when it starts, from everything in the view it is given. It is given Data at or below that label, and everything it writes carries that label and mark. The gate cannot see a native process open a file inside its view, so it cannot change the mark at that moment. The view is a snapshot taken at start: a file that arrives later in a shared directory, such as a download, is not in it. The gate checks every broker operation and every commit of staged writes. It does not see what a process does inside the view it was given; the kernel enforces that view.

| Limit | What enforces it for native code |
| --- | --- |
| Which objects | The view the sandbox is given (mount namespace and Landlock), or a broker operation |
| How many, how often | Broker operations as they happen. Writes made inside a view are staged and counted when they are committed (ADR 0007) |
| For how long | Handle expiry for broker operations. For an open view, the sandbox is stopped |
| At what cost | The broker, which is the only way to reach a model or money |
| Egress | A network namespace with no interfaces. The broker connection is the only way out |

Revoking access from native code means stopping its sandbox, because Linux cannot revoke an open file descriptor.

**A Tool from a stranger runs only as WASM**, unless the recipient rebuilds it locally. The author's description of why the Tool needs something is shown as the author's claim, next to the needs themselves, which the OS states in fixed words.

Tools describe their UI declaratively, and a trusted shell draws it inside system chrome a Tool cannot imitate. The UI description is also the Tool's interface for agents: an agent calls what a person can point at, and never sends input events (ADR 0014).

Revised after council rounds 1, 2, 3, and 4. The first version made WASM components the required default and native code the exception. The second said the gate checked "every use", which is false for a write through a file descriptor that is already open.

## Considered Options

- **WASM components as the required default (the earlier decision).** Rejected: a component's imports show the kind of authority it has, not its scope; the scope comes from what the broker binds, which works the same for a sandboxed native process. Agents also write working code against the Linux ecosystem far more reliably than against the component ecosystem.
- **Native binaries with no common contract.** Rejected: each Tool's authority would have to be judged from its code.
- **Route every native file operation through the broker.** Rejected for the first version: it is too slow for a photo sorter or a build, and software that expects files would not run.

## Consequences

- Linux is part of the trusted base: the broker decides, and the kernel's sandbox enforces.
- For software the broker cannot see inside, label checks are per principal and per destination, not per value.
- A limit on count or rate inside a view holds at commit, not at each write.
- Rehearsal needs every broker interface to have a simulated form, for both runtimes.
