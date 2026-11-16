//! The Warden refuses an order it cannot carry out exactly, before it
//! creates anything. These need no root.

use std::path::PathBuf;

use crowbar_sandbox::{Access, Budget, Needs, OrderError, StartOrder, ViewEntry};

fn entry(source: &str, at: &str) -> ViewEntry {
    ViewEntry {
        source: PathBuf::from(source),
        at: PathBuf::from(at),
        access: Access::ReadOnly,
    }
}

/// An order the Warden accepts: one program, in a view of its own.
fn order() -> StartOrder {
    StartOrder {
        principal: "task-7".to_string(),
        compartment: "c-0001".to_string(),
        program: PathBuf::from("/tool/sort-photos"),
        args: vec!["--by".to_string(), "date".to_string()],
        env: vec![("LANG".to_string(), "C.UTF-8".to_string())],
        view: vec![
            entry("/srv/tools/sort-photos", "/tool"),
            entry("/srv/data/photos", "/photos"),
        ],
        budget: Budget {
            memory_bytes: 64 << 20,
            processes: 16,
            cpu_millicores: 500,
            io_bytes_per_second: 8 << 20,
        },
        needs: Needs::default(),
        connections: Vec::new(),
    }
}

#[test]
fn a_plain_order_is_accepted() {
    assert_eq!(order().check(), Ok(()));
}

#[test]
fn an_id_must_be_safe_to_use_as_a_name() {
    // The compartment ID names a cgroup, and both IDs go into the ledger.
    for id in [
        "",
        ".",
        "..",
        ".hidden",
        "a/b",
        "a b",
        "tab\tbed",
        "nul\0",
        &"x".repeat(65),
    ] {
        let principal = StartOrder {
            principal: id.to_string(),
            ..order()
        };
        assert_eq!(
            principal.check(),
            Err(OrderError::Id(id.to_string())),
            "principal {id:?}"
        );
        let compartment = StartOrder {
            compartment: id.to_string(),
            ..order()
        };
        assert_eq!(
            compartment.check(),
            Err(OrderError::Id(id.to_string())),
            "compartment {id:?}"
        );
    }
    let longest = StartOrder {
        compartment: "x".repeat(64),
        principal: "Tool.photo-sorter_2".to_string(),
        ..order()
    };
    assert_eq!(longest.check(), Ok(()));
}

#[test]
fn every_path_is_absolute_and_says_exactly_where() {
    for path in ["relative/tool", "", "/tool/../etc", "/tool/./x", "/nul\0"] {
        let bad = PathBuf::from(path);
        let program = StartOrder {
            program: bad.clone(),
            ..order()
        };
        assert_eq!(program.check(), Err(OrderError::Path(bad.clone())), "program {path:?}");
        let source = StartOrder {
            view: vec![entry(path, "/photos")],
            ..order()
        };
        assert_eq!(source.check(), Err(OrderError::Path(bad.clone())), "source {path:?}");
        let at = StartOrder {
            view: vec![entry("/srv/data/photos", path)],
            ..order()
        };
        assert_eq!(at.check(), Err(OrderError::Path(bad)), "at {path:?}");
    }
}

#[test]
fn a_view_entry_never_covers_what_the_sandbox_itself_provides() {
    for at in ["/", "/proc", "/proc/1", "/dev", "/dev/shm", "/tmp", "/tmp/in"] {
        let covers = StartOrder {
            view: vec![entry("/srv/data/photos", at)],
            ..order()
        };
        assert_eq!(covers.check(), Err(OrderError::Reserved(PathBuf::from(at))), "{at}");
    }
    // A name that only starts the same way is free.
    let beside = StartOrder {
        view: vec![
            entry("/srv/data/photos", "/devices"),
            entry("/srv/data/notes", "/tmpfiles"),
        ],
        ..order()
    };
    assert_eq!(beside.check(), Ok(()));
}

#[test]
fn no_view_entry_is_inside_another() {
    for (first, second) in [("/photos", "/photos"), ("/photos", "/photos/2026"), ("/a/b/c", "/a")] {
        let nested = StartOrder {
            view: vec![entry("/srv/data/one", first), entry("/srv/data/two", second)],
            ..order()
        };
        let (outer, inner) = if first.len() <= second.len() {
            (first, second)
        } else {
            (second, first)
        };
        assert_eq!(
            nested.check(),
            Err(OrderError::Overlap(PathBuf::from(outer), PathBuf::from(inner))),
            "{first} and {second}"
        );
    }
}

#[test]
fn arguments_and_the_environment_are_plain_text() {
    let argument = StartOrder {
        args: vec!["a\0b".to_string()],
        ..order()
    };
    assert_eq!(argument.check(), Err(OrderError::Text("a\0b".to_string())));
    for name in ["", "A=B", "A\0"] {
        let named = StartOrder {
            env: vec![(name.to_string(), "value".to_string())],
            ..order()
        };
        assert_eq!(named.check(), Err(OrderError::Text(name.to_string())), "name {name:?}");
    }
    let value = StartOrder {
        env: vec![("NAME".to_string(), "va\0lue".to_string())],
        ..order()
    };
    assert_eq!(value.check(), Err(OrderError::Text("va\0lue".to_string())));
}

#[test]
fn every_part_of_the_budget_is_set() {
    // A zero is not "no limit". A compartment always has a budget.
    type Unset = fn(&mut Budget);
    let cases: [(&str, Unset); 4] = [
        ("memory", |budget| budget.memory_bytes = 0),
        ("processes", |budget| budget.processes = 0),
        ("CPU", |budget| budget.cpu_millicores = 0),
        ("I/O", |budget| budget.io_bytes_per_second = 0),
    ];
    for (part, zero) in cases {
        let mut unset = order();
        zero(&mut unset.budget);
        assert_eq!(unset.check(), Err(OrderError::Budget(part)), "{part}");
    }
}
