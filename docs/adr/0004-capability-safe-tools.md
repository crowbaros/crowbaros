# Generated tools run as capability-safe components by default

Generated **tools** default to a capability-safe component runtime (WASM components), not native Linux processes inside a sandbox. In a component, its imports are its manifest: it cannot reach anything it was not handed. That makes "does this tool only do what it says?" checkable, keeps tools portable across kernels, and makes them cheap to throw away. Tools describe their UI declaratively, and a trusted shell draws it. So every tool is themed and accessible, cannot spoof system UI, can be changed by the agent, and can render on any device the person owns.

Native code in a Linux sandbox (Landlock, seccomp, namespaces) with real Wayland surfaces stays available for legacy apps and for work that needs raw performance or the GPU.

## Considered Options

- **Native binaries in a Linux sandbox for everything.** Rejected as the default: the manifest can't be checked against the code, and the sandbox is tied to Linux.
