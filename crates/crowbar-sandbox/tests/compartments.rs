//! What a compartment gets, and the attacks a sandbox must refuse
//! (ADR 0016, "Attacks the tests must refuse", 1 to 10).
//!
//! These start real compartments, so they need root, cgroup v2, and a
//! kernel with Landlock. `cargo test` lists them as ignored. The crate's
//! README says how to run them.

use std::fs;
use std::os::unix::fs::MetadataExt;

mod common;
use common::{Probe, STORAGE, USER_IDS, data, field, probe_order, read_only, read_write, root_of, scratch};

#[test]
#[ignore = "needs root"]
fn a_compartment_sees_the_view_it_was_given_and_nothing_else() {
    let files = scratch("view");
    for directory in ["photos", "out"] {
        fs::create_dir(files.join(directory)).unwrap();
        common::stored(&files.join(directory));
    }
    data(&files.join("photos/cat.jpg"), "meow");

    let (mut order, broker) = probe_order(
        "task-1",
        &[
            "ids",
            "ls /",
            "read /photos/cat.jpg",
            "owner /photos/cat.jpg",
            "write /photos/new.jpg purr",
            "write /out/sorted.txt done",
            "owner /out/sorted.txt",
            "owner /probe",
            "write /intruder x",
            "exit 7",
        ],
    );
    order.view.push(read_only(&files.join("photos"), "/photos"));
    order.view.push(read_write(&files.join("out"), "/out"));
    let root = root_of(&order);
    let mut probe = Probe::run(order, broker);

    // It runs as a user of its own, from the Warden's range, in no group
    // but its own.
    let ids = probe.did("ids");
    let uid: u32 = field(&ids, "uid").parse().unwrap();
    assert!(USER_IDS.contains(&uid), "{ids}");
    assert_eq!(ids, format!("ok uid={uid} gid={uid} groups=[]"));

    // The root holds the view and the sandbox's own places.
    assert_eq!(probe.did("ls /"), format!("ok {root}"));

    // Data in the view is the compartment's own to read, and to change
    // where the order says so.
    assert_eq!(probe.did("read /photos/cat.jpg"), "ok meow");
    assert_eq!(probe.did("owner /photos/cat.jpg"), format!("ok {uid}"));
    assert_eq!(probe.did("write /photos/new.jpg purr"), "refused EROFS");
    assert_eq!(probe.did("write /out/sorted.txt done"), "ok");
    assert_eq!(probe.did("owner /out/sorted.txt"), format!("ok {uid}"));
    // A file that is not the storage user's is nobody's.
    assert_eq!(probe.did("owner /probe"), "ok 65534");
    // Outside the view there is nowhere to write.
    assert_eq!(probe.did("write /intruder x"), "refused EROFS");

    let ended = probe.compartment.wait().unwrap();
    assert_eq!(ended.code, Some(7));

    // What it wrote is on disk as the storage user's. The compartment's own
    // user owns nothing that outlives it.
    let written = files.join("out/sorted.txt");
    assert_eq!(fs::read_to_string(&written).unwrap(), "done");
    assert_eq!(fs::metadata(&written).unwrap().uid(), STORAGE);
    assert!(!files.join("photos/new.jpg").exists());
}
