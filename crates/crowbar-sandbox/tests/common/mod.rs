//! Helpers the root-only tests share: one Warden, places for files, and
//! the probe.

// Each test file is its own crate and uses only some of these.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crowbar_sandbox::{Access, Budget, Compartment, Needs, StartOrder, ViewEntry, Warden, WardenConfig};

/// The user and group that own Data on disk in these tests.
pub const STORAGE: u32 = 1_800_000_000;

/// The user IDs the tests' Warden gives to compartments.
pub const USER_IDS: std::ops::Range<u32> = 1_900_000_000..1_900_100_000;

/// A program the tests need. Cargo builds it beside them. A run on another
/// machine names the directory it was copied to.
pub fn program(name: &str) -> PathBuf {
    match std::env::var_os("CROWBAR_SANDBOX_BIN_DIR") {
        Some(directory) => PathBuf::from(directory).join(name),
        None => PathBuf::from(match name {
            "crowbar-warden" => env!("CARGO_BIN_EXE_crowbar-warden"),
            "crowbar-sandbox-probe" => env!("CARGO_BIN_EXE_crowbar-sandbox-probe"),
            _ => panic!("no program {name}"),
        }),
    }
}

pub fn config(name: &str, user_ids: std::ops::Range<u32>) -> WardenConfig {
    WardenConfig {
        runtime_dir: PathBuf::from(format!("/run/{name}")),
        cgroup: PathBuf::from(format!("/sys/fs/cgroup/{name}")),
        user_ids,
        storage_user: STORAGE,
        storage_group: STORAGE,
        init: program("crowbar-warden"),
    }
}

/// The one Warden every test shares, as a machine has one.
pub fn warden() -> &'static Warden {
    static WARDEN: OnceLock<Warden> = OnceLock::new();
    WARDEN.get_or_init(|| Warden::new(config("crowbar-warden-test", USER_IDS)).expect("a Warden"))
}

/// A new, empty directory on the machine, owned by the storage user.
pub fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("CROWBAR_SANDBOX_TEST_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let directory = base.join(format!("crowbar-sandbox-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    stored(&directory);
    directory
}

/// Gives a file or directory to the storage user, as Data on disk is.
pub fn stored(path: &Path) {
    std::os::unix::fs::chown(path, Some(STORAGE), Some(STORAGE)).unwrap();
}

/// A file of Data: owned by the storage user.
pub fn data(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    stored(path);
}

pub fn read_only(source: &Path, at: &str) -> ViewEntry {
    ViewEntry {
        source: source.to_path_buf(),
        at: PathBuf::from(at),
        access: Access::ReadOnly,
    }
}

pub fn read_write(source: &Path, at: &str) -> ViewEntry {
    ViewEntry {
        access: Access::ReadWrite,
        ..read_only(source, at)
    }
}

/// A budget no test reaches unless it sets a smaller one.
pub const ROOMY: Budget = Budget {
    memory_bytes: 256 << 20,
    processes: 64,
    cpu_millicores: 1000,
    io_bytes_per_second: 256 << 20,
};

/// An order for the probe, with its broker connection, and the broker's
/// end of that connection. The view holds the probe at `/probe`, and what a
/// probe that is not one static file needs to load.
pub fn probe_order(principal: &str, actions: &[&str]) -> (StartOrder, UnixStream) {
    static STARTS: AtomicU32 = AtomicU32::new(0);
    let (broker, compartment) = UnixStream::pair().unwrap();
    let mut view = vec![read_only(&program("crowbar-sandbox-probe"), "/probe")];
    if !cfg!(target_feature = "crt-static") {
        for libraries in ["/usr", "/lib", "/lib64"] {
            if let Ok(source) = fs::canonicalize(libraries) {
                view.push(read_only(&source, libraries));
            }
        }
    }
    let order = StartOrder {
        principal: principal.to_string(),
        compartment: format!("c-{:04}", STARTS.fetch_add(1, Ordering::SeqCst)),
        program: PathBuf::from("/probe"),
        // Each action is a name and its arguments, and none holds a space.
        args: actions
            .iter()
            .flat_map(|action| action.split(' '))
            .map(str::to_string)
            .collect(),
        env: Vec::new(),
        view,
        budget: ROOMY,
        needs: Needs::default(),
        connections: vec![OwnedFd::from(compartment)],
    };
    (order, broker)
}

/// The names in the sandbox's root for this order: what the sandbox itself
/// provides, and the top of each view entry.
pub fn root_of(order: &StartOrder) -> String {
    let mut names = BTreeSet::from(["dev", "proc", "tmp"].map(str::to_string));
    for entry in &order.view {
        let top = entry.at.components().nth(1).unwrap();
        names.insert(top.as_os_str().to_string_lossy().into_owned());
    }
    names.into_iter().collect::<Vec<_>>().join(" ")
}

/// A probe running in a compartment, and the broker's end of its
/// connection.
pub struct Probe {
    pub compartment: Compartment,
    broker: BufReader<UnixStream>,
}

impl Probe {
    /// Starts the probe in a new compartment of the principal `task-1`.
    pub fn start(view: Vec<ViewEntry>, actions: &[&str]) -> Probe {
        Probe::start_as("task-1", actions, |order| order.view.extend(view))
    }

    /// Starts the probe for `principal`, after `change` has had the order.
    pub fn start_as(principal: &str, actions: &[&str], change: impl FnOnce(&mut StartOrder)) -> Probe {
        let (mut order, broker) = probe_order(principal, actions);
        change(&mut order);
        Probe::run(order, broker)
    }

    pub fn run(order: StartOrder, broker: UnixStream) -> Probe {
        broker.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let compartment = warden().start(order).expect("the compartment starts");
        Probe {
            compartment,
            broker: BufReader::new(broker),
        }
    }

    /// The next line from the probe, or `None` when its end of the broker
    /// connection is closed.
    pub fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.broker.read_line(&mut line).expect("the probe answers in time") {
            0 => None,
            _ => Some(line.trim_end().to_string()),
        }
    }

    /// What happened when the probe carried out `action`, which is the next
    /// thing it reports.
    pub fn did(&mut self, action: &str) -> String {
        let line = self.line().unwrap_or_else(|| panic!("the probe ended before {action}"));
        match line.split_once(": ") {
            Some((said, happened)) if said == action => happened.to_string(),
            _ => panic!("expected {action}, and the probe said {line:?}"),
        }
    }

    /// Whether the probe, which is holding, still answers.
    pub fn answers(&mut self) -> bool {
        use std::io::Write;
        self.broker.get_mut().write_all(b"ping\n").is_ok() && self.line().as_deref() == Some("pong")
    }

    /// The compartment, and the broker's end of its connection.
    pub fn into_parts(self) -> (Compartment, UnixStream) {
        (self.compartment, self.broker.into_inner())
    }
}

/// The value after `key=` in a line of `key=value` words.
pub fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("no {key} in {line:?}"))
        .to_string()
}

/// The processes on the machine that run as `uid`.
pub fn processes_of(uid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        // A process that ends while this reads has no status any more.
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let ids = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .unwrap_or_default();
        if ids.split_whitespace().any(|id| id == uid.to_string()) {
            found.push(pid);
        }
    }
    found
}

/// The user a probe runs as, from the `ids` it reported.
pub fn user_of(probe: &mut Probe) -> u32 {
    field(&probe.did("ids"), "uid").parse().unwrap()
}

/// Waits until `met` holds, for a few seconds at most.
pub fn eventually(what: &str, met: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !met() {
        assert!(std::time::Instant::now() < deadline, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(5));
    }
}
