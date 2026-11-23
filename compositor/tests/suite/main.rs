//! Every test that can share a process, as one binary: Cargo links each file
//! in `tests/` on its own, and linking forty took most of the time the tests
//! did. Each file here is a module, and a new one takes a line below.
//!
//! The tests run side by side on threads, so each keeps to its own
//! compositor and its own files. A test that changes something the whole
//! process shares does not belong here: `tests/exhaustion.rs` lowers the
//! limit on descriptors, `tests/descriptors.rs` reads the process's whole
//! descriptor table, and `tests/shell_outputs.rs` starts a program that
//! inherits descriptors and the environment. Those stay binaries of their
//! own.

#[path = "../common/mod.rs"]
mod common;

mod attacks;
mod connections;
mod control;
mod cursor;
mod cursor_shape;
mod damage;
mod data_device;
mod dependencies;
mod dmabuf;
mod evidence;
mod explicit_sync;
mod focus;
mod fractional_scale;
mod gles;
mod gpu_import;
mod grabs;
mod idle_inhibit;
mod keyboard;
mod layers;
mod layout;
mod linked;
mod lock;
mod modes;
mod nested;
mod outputs;
mod presentation;
mod primary_selection;
mod probe_apps;
mod protocol;
mod redraw;
mod scale;
mod seat;
mod summon;
mod surfaces;
mod table;
mod viewporter;
mod windows;
mod workspaces;
