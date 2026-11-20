//! The compositor opens every descriptor close-on-exec (ADR 0016), so that
//! nothing a process starts could inherit a connection, a listener, or the
//! seat. The compositor itself starts no process at all.
//!
//! This file holds one test, so that no other test's descriptors are open in
//! the process while it looks.

mod common;

use std::{collections::BTreeSet, os::fd::BorrowedFd};

use common::Compositor;

fn open_descriptors() -> BTreeSet<i32> {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().to_str()?.parse().ok())
        .collect()
}

#[test]
fn every_descriptor_the_compositor_opens_is_close_on_exec() {
    let before = open_descriptors();

    let compositor = Compositor::start();
    let _shell = compositor.shell();
    let mut client = compositor.app();
    client.roundtrip().unwrap();

    let opened: Vec<i32> = open_descriptors().difference(&before).copied().collect();
    // The event loop and its waker, the shell's and the apps' sockets, the
    // control channel's, and each accepted connection, at the least.
    assert!(opened.len() >= 7, "only {} descriptors were opened", opened.len());
    for fd in opened {
        // Safety: the descriptor is open, and is only looked at.
        let Ok(flags) = rustix::io::fcntl_getfd(unsafe { BorrowedFd::borrow_raw(fd) }) else {
            // Closed since it was listed, such as the handle on the listing.
            continue;
        };
        let target = std::fs::read_link(format!("/proc/self/fd/{fd}")).unwrap_or_default();
        assert!(
            flags.contains(rustix::io::FdFlags::CLOEXEC),
            "descriptor {fd} ({}) would be inherited by a child process",
            target.display()
        );
    }
}
