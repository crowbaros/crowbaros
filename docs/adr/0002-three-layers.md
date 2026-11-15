# Three layers: Base, Environment, Data

A person's OS is split into three layers, and each one changes and rolls back on its own. **Base** (kernel, drivers, core services, built with xbps from a complete pinned package set, [ADR 0009](0009-base-from-a-pinned-package-set.md)) changes rarely, and a change becomes a Btrfs root generation that takes effect at boot. **Environment** (tools, services, desktop, automations) is where almost all agent work happens. It changes by fork, verify, promote ([ADR 0007](0007-fork-verify-promote.md)), with one-step undo. **Data** (the person's own things) is never rolled back with the other two and has its own history and undo window.

We split the layers because an agent-first OS makes many small changes a day. A reboot for each one, or a rollback that also rewinds the person's files, would make that unworkable.

## Consequences

- xbps packages are limited to Base. An agent request reaches Base only when the person really needs to get close to the metal (a driver, a kernel feature, a core service), and that is a deliberate change that needs a reboot.
- Native apps live in the Environment as sandboxed bundles, not as packages in Base.
- Data can be synced across machines, while Base stays specific to each machine.
