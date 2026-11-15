//! Task principals as compartments in Linux sandboxes.
//!
//! **Skeleton: no code yet.** Milestone 2, part one.
//!
//! Each task is its own principal and runs in its own compartment: a Linux
//! sandbox that holds only a broker connection, with a label fixed when it
//! starts and a view of the Data it was given. The broker decides; namespaces,
//! Landlock, and seccomp enforce. Native code is promised only what Linux can
//! enforce.
//!
//! Decisions: ADR 0004 and ADR 0011 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Enforcement on Linux" and
//! "Where isolation lives". Scope: `docs/milestones/milestone-2.md`.
