//! The Warden: starts every compartment, and stops it.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::cgroup::{Cgroup, Subtree};
use crate::order::{OrderError, StartOrder};
use crate::sys;
use crate::uids::{NoUserId, UserIds};
use crate::wire::{BUILT, InitOrder};

/// What the Warden is given by the machine it runs on.
#[derive(Debug, Clone)]
pub struct WardenConfig {
    /// A directory of the Warden's own, on a filesystem that is empty after
    /// a restart of the machine, such as `/run/crowbar/warden`.
    pub runtime_dir: PathBuf,
    /// The Warden's cgroup v2 subtree, such as `/sys/fs/cgroup/crowbar`.
    pub cgroup: PathBuf,
    /// The user IDs Base reserves for compartments. Each is also the
    /// compartment's group ID.
    pub user_ids: Range<u32>,
    /// The user and the group that own Data on disk. A compartment sees the
    /// files in its view that they own as its own, and no other file as its
    /// own.
    pub storage_user: u32,
    pub storage_group: u32,
    /// The `crowbar-warden` program, whose init stage builds each sandbox.
    pub init: PathBuf,
}

/// Starts every compartment, and stops it (ADR 0016). It runs as root.
pub struct Warden {
    config: WardenConfig,
    user_ids: UserIds,
    subtree: Subtree,
    /// Locked for as long as this Warden lives, so there is one Warden.
    _lock: File,
}

/// Why a compartment was not started. Nothing of it is left.
#[derive(Debug)]
pub enum StartError {
    /// The order is one the Warden does not carry out.
    Order(OrderError),
    /// Every user ID of the range has been used since the machine started.
    UserIdsSpent,
    /// A compartment with this ID is live.
    Live(String),
    /// The machine refused a step of the start.
    Setup(String),
}

impl Warden {
    /// Takes over the runtime directory and the cgroup subtree. A
    /// compartment that an earlier Warden left running is stopped.
    pub fn new(config: WardenConfig) -> io::Result<Warden> {
        // SAFETY: the call reads the process's own credentials.
        if unsafe { libc::geteuid() } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the Warden runs as root",
            ));
        }
        let ids = &config.user_ids;
        if ids.is_empty() || ids.start == 0 || ids.contains(&config.storage_user) {
            return Err(io::Error::other(format!(
                "{ids:?} is not a range of user IDs for compartments alone"
            )));
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.runtime_dir)?;
        let held = fs::metadata(&config.runtime_dir)?;
        if held.uid() != 0 || held.mode() & 0o022 != 0 {
            return Err(io::Error::other(format!(
                "{} can be changed by someone other than root",
                config.runtime_dir.display()
            )));
        }
        let lock = File::create(config.runtime_dir.join("lock"))?;
        // SAFETY: `lock` is an open descriptor for the whole call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::other(format!(
                "another Warden holds {}",
                config.runtime_dir.display()
            )));
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(config.runtime_dir.join("root"))?;
        Ok(Warden {
            user_ids: UserIds::open(&config.runtime_dir, config.user_ids.clone())?,
            subtree: Subtree::open(&config.cgroup)?,
            config,
            _lock: lock,
        })
    }

    /// Starts one compartment, and returns when its program is running.
    pub fn start(&self, order: StartOrder) -> Result<Compartment, StartError> {
        order.check().map_err(StartError::Order)?;
        let uid = self.user_ids.take().map_err(|none| match none {
            NoUserId::Spent => StartError::UserIdsSpent,
            NoUserId::Record(why) => StartError::Setup(why),
        })?;
        let cgroup = self
            .subtree
            .create(&order.compartment, &order.budget)
            .map_err(|e| match e.kind() {
                io::ErrorKind::AlreadyExists => StartError::Live(order.compartment.clone()),
                _ => StartError::Setup(e.to_string()),
            })?;
        // From here the compartment exists. Dropping it takes it away again.
        let mut compartment = Compartment {
            principal: order.principal.clone(),
            id: order.compartment.clone(),
            cgroup,
            init: None,
            ended: false,
        };
        let setup = |e: io::Error| StartError::Setup(e.to_string());

        let (ours, theirs) = sys::socket_pair().map_err(setup)?;
        let init = Command::new(&self.config.init)
            .arg("init")
            .env_clear()
            .stdin(Stdio::from(theirs))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| StartError::Setup(format!("{}: {e}", self.config.init.display())))?;
        // The init stage does nothing until it has its order, and it gets
        // the order only once it is inside the cgroup.
        let pid = init.id();
        compartment.init = Some(init);
        compartment.cgroup.add(pid).map_err(setup)?;

        let body = InitOrder {
            uid,
            gid: uid,
            storage_uid: self.config.storage_user,
            storage_gid: self.config.storage_group,
            root: self.config.runtime_dir.join("root"),
            needs: order.needs,
            program: order.program,
            args: order.args,
            env: order.env,
            view: order.view,
        }
        .encode();
        let connections: Vec<_> = order.connections.iter().map(|fd| fd.as_fd()).collect();
        sys::send_with_fds(ours.as_fd(), &(body.len() as u64).to_le_bytes(), &connections).map_err(setup)?;
        // The compartment's ends are its own now.
        drop(connections);
        drop(order.connections);
        let mut ours = File::from(ours);
        ours.write_all(&body).map_err(setup)?;

        // The init stage says what failed, or says that the sandbox is
        // built and then closes the socket by becoming the program.
        let mut said = Vec::new();
        ours.read_to_end(&mut said).map_err(setup)?;
        let failure = match said.split_first() {
            Some((&BUILT, [])) => return Ok(compartment),
            Some((&BUILT, failure)) => String::from_utf8_lossy(failure).into_owned(),
            Some(_) => String::from_utf8_lossy(&said).into_owned(),
            None => "the init stage ended before the sandbox was built".to_string(),
        };
        Err(StartError::Setup(failure))
    }
}

/// One live compartment. Dropping it stops it.
pub struct Compartment {
    principal: String,
    id: String,
    cgroup: Cgroup,
    init: Option<Child>,
    ended: bool,
}

/// How a compartment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    /// The program's exit code, when it ended by itself. A program that a
    /// signal ended has 128 plus the signal's number.
    pub code: Option<i32>,
    /// Whether the compartment was stopped for reaching its memory budget.
    pub out_of_memory: bool,
    /// Whether a new process was refused for the process budget.
    pub processes_refused: bool,
}

impl Compartment {
    /// The principal this compartment belongs to.
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// The name of this one start.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Stops the compartment: every process in it ends, however it tried to
    /// get away. This is the only way to take back what a native process
    /// holds open (ADR 0004).
    pub fn stop(mut self) -> io::Result<Ended> {
        self.end(false)
    }

    /// Waits for the program to end by itself. Whatever it left running
    /// ends with it.
    pub fn wait(mut self) -> io::Result<Ended> {
        self.end(true)
    }

    fn end(&mut self, wait: bool) -> io::Result<Ended> {
        let mut code = None;
        match self.init.take() {
            Some(mut init) if wait => {
                code = init.wait()?.code();
                self.cgroup.kill()?;
            }
            Some(mut init) => {
                self.cgroup.kill()?;
                init.wait()?;
            }
            None => self.cgroup.kill()?,
        }
        let ended = Ended {
            code,
            out_of_memory: self.cgroup.memory_stops()? > 0,
            processes_refused: self.cgroup.processes_refused()? > 0,
        };
        self.cgroup.remove()?;
        self.ended = true;
        Ok(ended)
    }
}

impl Drop for Compartment {
    fn drop(&mut self) {
        if !self.ended {
            let _ = self.end(false);
        }
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Order(order) => write!(f, "the order is refused: {order}"),
            StartError::UserIdsSpent => f.write_str("no user ID is left until the machine restarts"),
            StartError::Live(id) => write!(f, "a compartment with the ID {id} is live"),
            StartError::Setup(step) => write!(f, "the sandbox could not be built: {step}"),
        }
    }
}

impl std::error::Error for StartError {}
