# Three layers: Base, Environment, Data

A person's OS is split into three layers, and each one changes and rolls back on its own. **Base** (kernel, drivers, core services, built with xbps) changes rarely, and a change becomes a Btrfs root generation that takes effect at boot. This is what Milestone 1 built. **Environment** (tools, services, desktop, automations) is where almost all agent work happens. Changes to it apply live and atomically, with instant undo. **Data** (the person's own things) is never rolled back with the other two and has its own history and undo window.

We split the layers because an agent-first OS makes many small changes a day. A reboot for each one, or a rollback that also rewinds the person's files, would make that unworkable.

## Consequences

- xbps packages are limited to Base. An agent request reaches Base only when the person really needs to get close to the metal (a driver, a kernel feature, a core service), and that is a deliberate change that needs a reboot.
- Legacy native apps live in the Environment as sandboxed bundles, not as packages in Base.
- Data can be synced across machines, while Base stays specific to each machine.
