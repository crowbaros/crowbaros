//! The Warden: every compartment in its own Linux sandbox.
//!
//! Each start of a task, a Tool, installed software, or a workbench is a
//! compartment. The Warden gives it a user ID and a sandbox that no other
//! compartment shares, and it is the only part that starts or stops one.
//! The broker decides; the kernel enforces what is written in the order.
//! Native code is promised only what Linux can enforce.
//!
//! Built so far, for one program in one compartment:
//!
//! - A [`StartOrder`] says everything the compartment gets: the program,
//!   its [view](ViewEntry), its [`Budget`], its [`Needs`], and its broker
//!   connections. [`StartOrder::check`] refuses an order that cannot be
//!   carried out exactly.
//! - [`Warden::start`] builds the sandbox and runs the program in it:
//!   - **a user ID of its own**, never given again until the machine
//!     restarts, with no group but its own and no capability;
//!   - **a filesystem of its own**: an empty root with a private `/proc`,
//!     `/dev`, and `/tmp`, and the view. Each view entry is the file or
//!     directory the order names, ID-mapped so that the storage user's
//!     files are the compartment's own and what it writes is the storage
//!     user's on disk. A file of any other owner is nobody's: it can be
//!     read as far as its permissions for others allow, and never written.
//!     No entry honours a setuid bit or a device file;
//!   - **processes, IPC, and a network of its own**: PID, IPC, UTS, cgroup,
//!     and network namespaces. The network has no interface but a loopback
//!     that is down, so the broker connections are the only way out;
//!   - **its broker connections and no other descriptor**: they are
//!     descriptors 3, 4, and so on, after a `/dev/null` for standard input
//!     and both outputs;
//!   - **no ambient credential**: the environment of the order alone, a
//!     new session with no terminal, a new session keyring, and no core
//!     file;
//!   - **a second check**: Landlock states the view again, and seccomp
//!     takes away tracing, mounts, user namespaces, the kernel's keyrings,
//!     and calls no Tool needs. Tracing inside the compartment and user
//!     namespaces come back only as stated [`Needs`];
//!   - **a budget**: a cgroup with limits on memory, processes, CPU time,
//!     and disk I/O.
//! - [`Compartment::stop`] ends every process with `cgroup.kill`. That is
//!   what revoking a native task's access means: Linux cannot take back an
//!   open descriptor, and it can end the processes that hold it.
//! - A [`TrustedListener`] is the path socket of a trusted part, which
//!   serves one user ID as the kernel reports it.
//!
//! The Warden needs root, cgroup v2 with the `cpu`, `io`, `memory`, and
//! `pids` controllers, and a kernel with Landlock, ID-mapped mounts, and
//! `cgroup.kill`. Where one is missing it starts nothing.
//!
//! Not here yet:
//!
//! - the Warden as a service: a root program that takes these orders from
//!   the broker over its own path socket. So far the Warden is this
//!   library, and `crowbar-warden` holds only its init stage;
//! - what needs Base: the Wayland listener, the GPU and `/dev/kvm` as
//!   needs, a block of user IDs for a workbench, the wide compartment, and
//!   the check at boot;
//! - staged writes. A view entry that can be written is written live;
//! - the compartment's label and mark, which the broker fixes at start
//!   from the view it orders.
//!
//! Decisions: ADR 0004, ADR 0011, and ADR 0016 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "What native code is
//! promised" and "Enforcement on Linux". Scope:
//! `docs/milestones/milestone-2.md`. Glossary: compartment, sandbox, Warden.

mod cgroup;
mod init;
mod landlock;
mod order;
mod seccomp;
mod sys;
mod trusted;
mod uids;
mod warden;
mod wire;

pub use init::init_stage;
pub use order::{Access, Budget, Needs, OrderError, StartOrder, ViewEntry};
pub use trusted::{Stranger, TrustedListener};
pub use warden::{Compartment, Ended, StartError, Warden, WardenConfig};
