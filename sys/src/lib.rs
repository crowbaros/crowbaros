//! Fulcrum's bindings to the kernel and to the C libraries it links.
//!
//! This is the only Fulcrum crate with `unsafe` code. Each binding here wraps
//! the unsafe part behind a safe interface, and says in a `SAFETY` comment why
//! the wrapper holds.

mod dl;
pub mod drm;
pub mod gbm;
pub mod gles;
pub mod libinput;
pub mod shm;
pub mod xkb;
