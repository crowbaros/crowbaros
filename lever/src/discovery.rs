//! Harness-agnostic external session discovery and imported history types.
//!
//! When a person runs a harness by hand in a terminal, Lever discovers those
//! sessions without keeping the harness running:
//!
//! 1. Each harness implements [`SessionDiscoverer`], declaring which on-disk
//!    store paths to watch ([`WatchSpec`]) and how to list sessions on demand.
//! 2. [`StoreWatcher`](crate::watcher::StoreWatcher) watches those paths with
//!    Linux `inotify` inside Lever's `poll(2)` loop (zero idle wakeups).
//! 3. After a short debounce, [`DiscoveryRunner`] runs
//!    [`SessionDiscoverer::list_sessions`] on a background thread and wakes
//!    Lever's `poll(2)` loop over a non-blocking pipe.
//! 4. When the person opens a discovered thread, Lever reads its history into
//!    Lever's own SQLite record as a harness-agnostic [`SessionHistory`].

use crate::acp::{self, HarnessConfig};
use crate::opencode;
use crate::sys;
use crate::watcher::{StoreWatcher, WatchSpec};
use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, ClientCapabilities, Implementation, InitializeRequest, InitializeResponse, JsonRpcMessage,
    ListSessionsRequest, ListSessionsResponse, Request, RequestId,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Upper bound for one-shot ACP discovery so a wedged harness never stalls
/// future discovery passes.
const DISCOVERY_RPC_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum pages followed via `nextCursor` in one `session/list` pass.
pub(crate) const MAX_LIST_PAGES: usize = 50;
/// Maximum line length accepted from a one-shot discovery child.
const MAX_DISCOVERY_LINE: usize = 16 * 1024 * 1024;

/// One session reported by a harness's own store or listing command, before
/// Lever maps it to its own `t-...` thread ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSession {
    pub harness_session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    /// The harness's own `updated` stamp, at its full precision: what tells
    /// one version of a session from the next.
    pub stamp: Option<String>,
    /// True when the harness reports this session is a child/subagent session
    /// rather than a top-level thread.
    pub is_subagent: bool,
}

/// One imported act within a turn read from a harness's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAct {
    pub harness_call_id: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub detail: Option<String>,
}

/// One imported subagent within a turn read from a harness's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSubagent {
    pub harness_session_id: String,
    pub title: String,
    pub state: String,
    pub error: Option<String>,
    pub prompt: Option<String>,
}

/// One imported turn read from a harness's history into Lever's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedTurn {
    pub message: String,
    pub answer: String,
    pub acts: Vec<ImportedAct>,
    pub notes: Vec<(String, String)>,
    pub subagents: Vec<ImportedSubagent>,
    pub origin: Option<String>,
}

/// Harness-agnostic history read when opening an externally-started thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHistory {
    pub title: Option<String>,
    pub turns: Vec<ImportedTurn>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Pluggable per-harness contract for discovering sessions started outside Lever.
pub trait SessionDiscoverer: Send + Sync {
    /// On-disk store paths whose changes mean sessions may have been created or
    /// updated by hand.
    fn watch_specs(&self) -> Vec<WatchSpec>;

    /// Whether the harness's store already holds data on disk. When `false`,
    /// Lever skips any startup scan so a fresh boot with no prior sessions
    /// spawns nothing.
    fn has_store_data(&self) -> bool;

    /// Lists sessions from the harness's store without keeping a long-lived
    /// harness process running. What it lists is then classified by
    /// [`Self::classify_sessions`].
    fn list_sessions(&self) -> Result<Listing, String>;

    /// Whether raw ACP `session/list` results need harness-specific
    /// classification (e.g. distinguishing subagent or empty sessions).
    fn needs_classification(&self) -> bool {
        false
    }

    /// Classifies raw `session/list` results, from [`Self::list_sessions`] or
    /// an already-running ACP harness, given what Lever already knows.
    fn classify_sessions(&self, sessions: Vec<ExternalSession>, _known: &KnownSessions) -> Classified {
        Classified {
            sessions,
            ..Classified::default()
        }
    }
}

/// What a one-shot listing found.
#[derive(Default)]
pub struct Listing {
    pub sessions: Vec<ExternalSession>,
    /// The store, watched since the listing was taken, when it is.
    pub watcher: Option<StoreWatcher>,
}

impl Listing {
    /// Whether the harness's store has been written since the listing was
    /// taken. The listing harness writes nothing then itself, so the write
    /// was someone else's, or a command the pass ran since.
    pub fn changed_since(&mut self) -> bool {
        self.watcher.as_mut().is_some_and(StoreWatcher::drain_changed)
    }
}

/// What Lever already knows of a harness's sessions.
#[derive(Debug, Clone, Default)]
pub struct KnownSessions {
    /// Sessions of Lever's threads, and those its harness holds now.
    pub owned: HashSet<String>,
    /// Sessions that are never threads: subagents, deleted, probes, replaced.
    pub ignored: HashSet<String>,
    /// Sessions found empty or archived, by the `updated` stamp they had.
    pub quiet: HashMap<String, String>,
}

impl KnownSessions {
    /// Whether a listed session is one found empty or archived, unchanged
    /// since: it is not a thread.
    pub fn is_quiet(&self, session: &ExternalSession) -> bool {
        session.stamp.is_some() && self.quiet.get(&session.harness_session_id) == session.stamp.as_ref()
    }

    /// Whether a listed session needs classifying.
    pub fn unseen(&self, session: &ExternalSession) -> bool {
        let sid = &session.harness_session_id;
        !session.is_subagent && !self.owned.contains(sid) && !self.ignored.contains(sid) && !self.is_quiet(session)
    }
}

/// What classifying a listing found.
#[derive(Debug, Default)]
pub struct Classified {
    /// The sessions to apply: threads, subagents, and those already known.
    pub sessions: Vec<ExternalSession>,
    /// Whether the store may have changed after the listing was taken: it
    /// ran commands of the harness's own then, which write to its store, or
    /// the listing saw a write then.
    pub changed_after: bool,
    /// Whether sessions were left for the next pass.
    pub more: bool,
    /// Sessions newly found empty or archived, with their stamps.
    pub quiet: Vec<(String, String)>,
    /// The soonest a session whose export failed may be tried again, when
    /// one was left for then.
    pub retry_at: Option<Instant>,
}

/// What a job found, and a fingerprint of the listing it classified.
type JobResult = Result<(Classified, u64), String>;
type PendingScan = Arc<Mutex<Option<JobResult>>>;
/// A finished job: what it found and the fingerprint of its listing, or
/// why it failed, and whether another pass was asked for while it ran.
pub type FinishedJob = (Result<(Classified, u64), String>, bool);

/// Non-blocking runner for a [`SessionDiscoverer`] that integrates with
/// Lever's `poll(2)` loop over a self-pipe.
pub struct DiscoveryRunner {
    discoverer: Arc<dyn SessionDiscoverer>,
    read_fd: OwnedFd,
    write_fd: Arc<OwnedFd>,
    result: PendingScan,
    in_flight: bool,
    rescan_wanted: bool,
}

impl DiscoveryRunner {
    pub fn new(discoverer: Box<dyn SessionDiscoverer>) -> io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` is a valid two-element array for `pipe2`.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `pipe2` returned two owned descriptors.
        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_fd = Arc::new(unsafe { OwnedFd::from_raw_fd(fds[1]) });
        Ok(Self {
            discoverer: Arc::from(discoverer),
            read_fd,
            write_fd,
            result: Arc::new(Mutex::new(None)),
            in_flight: false,
            rescan_wanted: false,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.read_fd.as_raw_fd()
    }

    pub fn watch_specs(&self) -> Vec<WatchSpec> {
        self.discoverer.watch_specs()
    }

    pub fn has_store_data(&self) -> bool {
        self.discoverer.has_store_data()
    }

    pub fn needs_classification(&self) -> bool {
        self.discoverer.needs_classification()
    }

    /// Starts a background scan if none is running; otherwise marks that one
    /// more scan should run after the current one completes.
    pub fn request_scan(&mut self, known: KnownSessions) {
        let discoverer = Arc::clone(&self.discoverer);
        self.start_job("lever-discover", move || {
            let mut listing = discoverer.list_sessions()?;
            let fingerprint = listing_fingerprint(&listing.sessions);
            let mut found = discoverer.classify_sessions(std::mem::take(&mut listing.sessions), &known);
            // Watched to the job's end: only the hand-over to the service is
            // left for the service's own watcher.
            found.changed_after |= listing.changed_since();
            Ok((found, fingerprint))
        });
    }

    /// Classifies raw `session/list` results from an already-running harness on
    /// the background thread so `poll(2)` never blocks.
    pub fn request_classify(&mut self, sessions: Vec<ExternalSession>, known: KnownSessions) {
        let discoverer = Arc::clone(&self.discoverer);
        self.start_job("lever-classify", move || {
            let fingerprint = listing_fingerprint(&sessions);
            Ok((discoverer.classify_sessions(sessions, &known), fingerprint))
        });
    }

    /// Whether a scan or classification is running on the background thread.
    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// Asks for one more scan once the running job ends.
    pub fn want_rescan(&mut self) {
        self.rescan_wanted = true;
    }

    fn start_job(&mut self, name: &str, job: impl FnOnce() -> JobResult + Send + 'static) {
        if self.in_flight {
            self.rescan_wanted = true;
            return;
        }
        self.in_flight = true;
        let result_slot = Arc::clone(&self.result);
        let write_fd = Arc::clone(&self.write_fd);
        let spawned = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            // A job that panics still reports, so discovery is never left in flight.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .unwrap_or_else(|_| Err("session discovery failed".to_string()));
            if let Ok(mut guard) = result_slot.lock() {
                *guard = Some(outcome);
            }
            let byte = 1u8;
            // SAFETY: writing one byte to an owned non-blocking pipe descriptor.
            unsafe {
                let _ = libc::write(write_fd.as_raw_fd(), (&raw const byte).cast(), 1);
            }
        });
        if spawned.is_err() {
            self.in_flight = false;
        }
    }

    /// Drains the wake pipe and returns any completed scan result, the
    /// fingerprint of what it listed, and whether another scan was requested
    /// while it ran.
    pub fn take_result(&mut self) -> Option<FinishedJob> {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: draining an owned non-blocking pipe descriptor.
            let n = unsafe { libc::read(self.read_fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
        let outcome = self.result.lock().ok().and_then(|mut g| g.take())?;
        self.in_flight = false;
        let rescan = std::mem::take(&mut self.rescan_wanted);
        Some((outcome, rescan))
    }
}

/// The longest a store that keeps changing holds back a discovery pass.
pub const DISCOVERY_MAX_WAIT: Duration = Duration::from_secs(2);

/// When Lever runs a discovery pass. A change to a harness's store schedules
/// one after a quiet `debounce`, but never later than `max_wait` after the
/// first change, so a store that keeps changing is still read.
///
/// Listing a harness's sessions writes to its store, so a change seen while a
/// pass runs may be the pass's own, or the person's after the listing was
/// taken. Such a change brings a follow-up pass, and a follow-up brings
/// another only when its listing differs from the one before it: a listing's
/// own writes change no session, so a quiet store goes quiet. A change seen
/// once a pass has ended is the person's, always. The service reads every
/// change a pass made before it calls [`Self::pass_finished`]: a listing's
/// writes are all made before it answers or ends.
#[derive(Debug)]
pub struct DiscoverySchedule {
    debounce: Duration,
    max_wait: Duration,
    /// When the store's changes bring a pass.
    debounce_due: Option<Instant>,
    /// When a pass was asked for, whatever the store does.
    requested_due: Option<Instant>,
    dirty_since: Option<Instant>,
    missed: bool,
    followup_next: bool,
    followup_now: bool,
    last_listing: Option<u64>,
}

/// How a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassEnd {
    /// It listed: the listing's fingerprint ([`listing_fingerprint`]), and
    /// whether the store may have changed after it was taken.
    Listed { fingerprint: u64, changed_after: bool },
    /// It failed, or was lost.
    Failed,
}

impl DiscoverySchedule {
    pub fn new(debounce: Duration, max_wait: Duration) -> Self {
        Self {
            debounce,
            max_wait,
            debounce_due: None,
            requested_due: None,
            dirty_since: None,
            missed: false,
            followup_next: false,
            followup_now: false,
            last_listing: None,
        }
    }

    /// The harness's store changed. `busy` is whether a pass is running.
    pub fn store_changed(&mut self, now: Instant, busy: bool) {
        if busy {
            self.missed = true;
            return;
        }
        // The person's change: the next pass is not a follow-up.
        self.followup_next = false;
        let since = *self.dirty_since.get_or_insert(now);
        self.debounce_due = Some((now + self.debounce).min(since + self.max_wait));
    }

    /// Asks for a pass at `at` at the latest, whatever the store does.
    pub fn request(&mut self, at: Instant) {
        self.requested_due = Some(self.requested_due.map_or(at, |due| due.min(at)));
    }

    fn due(&self) -> Option<Instant> {
        match (self.debounce_due, self.requested_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Whether a pass is due now, and if so, takes it.
    pub fn take_due(&mut self, now: Instant) -> bool {
        if !self.due().is_some_and(|due| now >= due) {
            return false;
        }
        self.debounce_due = None;
        self.requested_due = None;
        self.dirty_since = None;
        self.followup_now = std::mem::take(&mut self.followup_next);
        true
    }

    /// The pass that was taken has ended.
    ///
    /// A change seen while it ran brings a follow-up. A follow-up brings
    /// another only when its listing changed, or when the store may have
    /// changed after its listing was taken: a write the listing saw then, or
    /// the harness's commands run then, which run only for sessions not yet
    /// classified, so this ends. A failed pass brings no follow-up of a
    /// follow-up.
    pub fn pass_finished(&mut self, now: Instant, end: PassEnd) {
        let again = match end {
            PassEnd::Listed {
                fingerprint,
                changed_after,
            } => {
                let changed = self.last_listing != Some(fingerprint);
                self.last_listing = Some(fingerprint);
                !self.followup_now || changed || changed_after
            }
            PassEnd::Failed => !self.followup_now,
        };
        if std::mem::take(&mut self.missed) && again {
            self.followup_next = true;
            self.request(now + self.debounce);
        }
    }

    /// How long until the next pass is due.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.due().map(|due| due.saturating_duration_since(now))
    }
}

/// A fingerprint of a listing: each session's ID, folder, title, and the
/// harness's own `updated` stamp, in any order.
pub fn listing_fingerprint(sessions: &[ExternalSession]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut entries: Vec<_> = sessions
        .iter()
        .map(|s| (&s.harness_session_id, &s.cwd, &s.title, &s.stamp))
        .collect();
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

/// Converts an ACP v1 `SessionInfo` into Lever's harness-agnostic
/// [`ExternalSession`], skipping entries without an ID or absolute `cwd`.
pub(crate) fn session_info_to_external(
    info: &agent_client_protocol_schema::v1::SessionInfo,
) -> Option<ExternalSession> {
    let sid = info.session_id.0.as_ref().to_string();
    if sid.is_empty() {
        return None;
    }
    let cwd = info.cwd.to_string_lossy().into_owned();
    if !Path::new(&cwd).is_absolute() {
        return None;
    }
    let title = info
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let updated = info.updated_at.as_deref().and_then(parse_rfc3339_unix);
    let is_subagent = meta_indicates_subagent(info.meta.as_ref());
    Some(ExternalSession {
        harness_session_id: sid,
        cwd,
        title,
        created: None,
        updated,
        stamp: info.updated_at.clone(),
        is_subagent,
    })
}

/// Generic ACP v1 one-shot session discoverer: spawns any ACP harness, calls
/// `initialize` + paginated `session/list`, and immediately stops the child so
/// no harness process stays running for discovery.
pub struct AcpSessionDiscoverer {
    config: HarnessConfig,
    watch_specs: Vec<WatchSpec>,
}

impl AcpSessionDiscoverer {
    pub fn new(config: HarnessConfig, watch_specs: Vec<WatchSpec>) -> Self {
        Self { config, watch_specs }
    }

    /// Runs a single one-shot ACP `initialize` + `session/list` against
    /// `self.config` and immediately terminates the child process.
    pub fn list_via_acp(&self) -> Result<Listing, String> {
        // The store, watched while the listing runs: what it writes as it
        // starts is its own, and a write once it has answered is not.
        let mut watcher = StoreWatcher::new(self.watch_specs.clone()).ok().flatten();
        let (env_set, env_remove) = acp::build_discovery_env(&self.config);
        let cmd = sys::harness_command(&self.config.program, &self.config.args, &env_set, &env_remove);
        // Dropping the child on any early return ends and waits for it.
        let mut child = sys::WorkerChild::spawn(cmd)
            .map_err(|e| format!("could not start {} to list sessions: {e}", self.config.name))?;
        let mut stdin = child
            .child()
            .stdin
            .take()
            .ok_or_else(|| "discovery child had no stdin pipe".to_string())?;
        let mut stdout = child
            .child()
            .stdout
            .take()
            .ok_or_else(|| "discovery child had no stdout pipe".to_string())?;
        sys::set_nonblocking(stdout.as_raw_fd())
            .map_err(|e| format!("could not set discovery stdout nonblocking: {e}"))?;

        let deadline = Instant::now() + DISCOVERY_RPC_TIMEOUT;
        let mut rpc = OneShotRpc {
            stdin: &mut stdin,
            stdout: &mut stdout,
            read_buf: Vec::new(),
            next_id: 1,
            deadline,
        };

        let outcome = (|| -> Result<Vec<ExternalSession>, String> {
            let init = InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(ClientCapabilities::new())
                .client_info(Implementation::new("crowbar-lever", crate::channel::LEVER_VERSION));
            let init_val = rpc.call(AGENT_METHOD_NAMES.initialize, &init)?;
            if let Some(err) = init_val.get("error").filter(|v| !v.is_null()) {
                return Err(format!("initialize failed during discovery: {err}"));
            }
            let init_resp: InitializeResponse =
                serde_json::from_value(init_val.get("result").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("invalid initialize response during discovery: {e}"))?;
            if init_resp.protocol_version != ProtocolVersion::V1 {
                return Err("harness uses an unsupported protocol version".to_string());
            }
            if init_resp.agent_capabilities.session_capabilities.list.is_none() {
                return Ok(Vec::new());
            }
            // The harness writes to its store as it starts, and not once it
            // has answered: a second listing, taken after the first has
            // answered and its writes are drained, has every write made
            // since in it, or seen by the watcher.
            rpc.list_all()?;
            if let Some(watcher) = watcher.as_mut() {
                let _ = watcher.drain_changed();
            }
            rpc.list_all()
        })();

        // Always close stdin and stop the discovery child immediately so no
        // harness process is ever left running for session discovery.
        drop(rpc);
        drop(stdin);
        drop(stdout);
        child.finish(Duration::from_millis(300));
        outcome.map(|sessions| Listing { sessions, watcher })
    }
}

/// One-shot JSON-RPC calls to a harness started for one listing.
struct OneShotRpc<'a> {
    stdin: &'a mut std::process::ChildStdin,
    stdout: &'a mut std::process::ChildStdout,
    read_buf: Vec<u8>,
    next_id: i64,
    deadline: Instant,
}

impl OneShotRpc<'_> {
    fn call<P: serde::Serialize>(&mut self, method: &str, params: &P) -> Result<Value, String> {
        let id = send_acp_request(self.stdin, &mut self.next_id, method, params)?;
        recv_acp_response(self.stdout, &mut self.read_buf, id, self.deadline)
    }

    /// Every session the harness lists, following its pages.
    fn list_all(&mut self) -> Result<Vec<ExternalSession>, String> {
        let mut all = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut req = ListSessionsRequest::new();
            if let Some(ref c) = cursor {
                req = req.cursor(c.clone());
            }
            let val = self.call(AGENT_METHOD_NAMES.session_list, &req)?;
            if let Some(err) = val.get("error").filter(|v| !v.is_null()) {
                return Err(format!("session/list failed: {err}"));
            }
            let resp: ListSessionsResponse = serde_json::from_value(val.get("result").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("invalid session/list response: {e}"))?;
            if resp.sessions.is_empty() {
                break;
            }
            let mut added = 0;
            for info in resp.sessions {
                let Some(ext) = session_info_to_external(&info) else {
                    continue;
                };
                if !seen_ids.insert(ext.harness_session_id.clone()) {
                    continue;
                }
                all.push(ext);
                added += 1;
            }
            if added == 0 {
                break;
            }
            match resp.next_cursor {
                Some(next) if !next.is_empty() && Some(&next) != cursor.as_ref() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(all)
    }
}

impl SessionDiscoverer for AcpSessionDiscoverer {
    fn watch_specs(&self) -> Vec<WatchSpec> {
        self.watch_specs.clone()
    }

    fn has_store_data(&self) -> bool {
        self.watch_specs.iter().any(spec_has_data)
    }

    fn list_sessions(&self) -> Result<Listing, String> {
        self.list_via_acp()
    }
}

/// Selects the right [`SessionDiscoverer`] for a configured harness.
///
/// A harness with its own store or subagent rules plugs in here behind the
/// single [`SessionDiscoverer`] contract, as OpenCode does. Any other ACP
/// harness configured with `--harness-watch` gets the generic
/// [`AcpSessionDiscoverer`].
pub fn for_harness(config: &HarnessConfig) -> Option<Box<dyn SessionDiscoverer>> {
    if opencode::is_opencode_config(config) {
        return Some(Box::new(opencode::OpenCodeSessionDiscoverer::new(config.clone())));
    }
    let specs = watch_specs_from_paths(&config.watch_paths);
    if specs.is_empty() {
        return None;
    }
    Some(Box::new(AcpSessionDiscoverer::new(config.clone(), specs)))
}

/// Converts the paths given with `--harness-watch` into [`WatchSpec`]s. A path
/// pointing to a specific file watches its parent directory filtered to that
/// filename; a directory (or not-yet-existing path without a clear file parent)
/// watches the directory itself.
pub fn watch_specs_from_paths(paths: &[PathBuf]) -> Vec<WatchSpec> {
    paths
        .iter()
        .map(|path| {
            if path.is_file()
                && let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
            {
                WatchSpec::files(parent.to_path_buf(), vec![name.to_string()])
            } else {
                WatchSpec::directory(path.clone())
            }
        })
        .collect()
}

pub fn spec_has_data(spec: &WatchSpec) -> bool {
    if !spec.dir.is_dir() {
        return false;
    }
    if spec.filenames.is_empty() {
        return std::fs::read_dir(&spec.dir).ok().and_then(|mut it| it.next()).is_some();
    }
    spec.filenames.iter().any(|name| spec.dir.join(name).exists())
}

fn meta_indicates_subagent(meta: Option<&serde_json::Map<String, Value>>) -> bool {
    let Some(meta) = meta else {
        return false;
    };
    if meta.get("isSubagent").and_then(Value::as_bool) == Some(true)
        || meta.get("subagent").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    for key in ["parentSessionId", "parentId", "parent_session_id", "parent_id"] {
        if meta
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
        {
            return true;
        }
    }
    false
}

fn send_acp_request<P: serde::Serialize>(
    stdin: &mut impl Write,
    next_id: &mut i64,
    method: &str,
    params: &P,
) -> Result<i64, String> {
    let id = *next_id;
    *next_id += 1;
    let msg = JsonRpcMessage::wrap(Request {
        id: RequestId::Number(id),
        method: method.into(),
        params: Some(params),
    });
    let line = serde_json::to_string(&msg).map_err(|e| format!("could not encode discovery RPC: {e}"))?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|e| format!("could not write discovery RPC: {e}"))?;
    Ok(id)
}

fn recv_acp_response(
    stdout: &mut std::process::ChildStdout,
    buf: &mut Vec<u8>,
    expected_id: i64,
    deadline: Instant,
) -> Result<Value, String> {
    let mut chunk = [0u8; 8192];
    loop {
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf[..pos].to_vec();
            buf.drain(..=pos);
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if val.get("id").and_then(Value::as_i64) == Some(expected_id) && val.get("method").is_none() {
                return Ok(val);
            }
        }
        if buf.len() > MAX_DISCOVERY_LINE {
            return Err("discovery harness wrote a line longer than 16 MiB".to_string());
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err("timed out waiting for harness to list sessions".to_string());
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let mut pfd = [libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        sys::poll(&mut pfd, timeout_ms).map_err(|e| format!("discovery poll failed: {e}"))?;
        match stdout.read(&mut chunk) {
            Ok(0) => return Err("discovery harness exited before answering".to_string()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("could not read discovery harness: {e}")),
        }
    }
}

/// Parses an RFC 3339 UTC/offset timestamp (e.g. `2026-11-27T05:16:54.415Z` or
/// `2026-11-27T00:16:54-05:00`) into Unix seconds.
pub fn parse_rfc3339_unix(input: &str) -> Option<i64> {
    let s = input.trim();
    if s.len() < 20 || !s.is_ascii() {
        return None;
    }
    let bytes = s.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && digits(5..7) && digits(8..10) && digits(11..13) && digits(14..16) && digits(17..19)) {
        return None;
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let minute: i64 = s[14..16].parse().ok()?;
    let second: i64 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(after_dot) = rest.strip_prefix('.') {
        let digits = after_dot.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        rest = &after_dot[digits..];
    }
    let offset_secs: i64 = if rest == "Z" || rest == "z" {
        0
    } else if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) && rest.as_bytes()[3] == b':' {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        if !rest[1..3].bytes().chain(rest[4..6].bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs)
}

/// Howard Hinnant's civil-from-epoch day count (1970-01-01 => 0).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => return None,
    };
    if day > max_day {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u64;
    let m = u64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe as i64 - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_rfc3339_timestamps_with_utc_and_offsets() {
        assert_eq!(parse_rfc3339_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_unix("2026-11-27T05:16:54.415Z"), Some(1_795_756_614));
        assert_eq!(parse_rfc3339_unix("2026-11-27T00:16:54-05:00"), Some(1_795_756_614));
        assert_eq!(parse_rfc3339_unix("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(parse_rfc3339_unix("2025-02-29T12:00:00Z"), None);
        assert_eq!(parse_rfc3339_unix("not-a-timestamp"), None);
        // Neither panics nor reads a sign as an hour.
        assert_eq!(parse_rfc3339_unix("2026-01-01T00:00:0\u{e9}Z"), None);
        assert_eq!(parse_rfc3339_unix("2026-01-01T-1:00:00Z"), None);
        assert_eq!(parse_rfc3339_unix("2026-01-01T00:00:00+-1:00"), None);
    }

    fn listed(fingerprint: u64) -> PassEnd {
        PassEnd::Listed {
            fingerprint,
            changed_after: false,
        }
    }

    fn schedule() -> (DiscoverySchedule, Instant) {
        let ms = Duration::from_millis;
        (DiscoverySchedule::new(ms(400), ms(2000)), Instant::now())
    }

    #[test]
    fn a_passs_own_store_writes_bring_one_follow_up_and_then_quiet() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.store_changed(t0, false);
        assert!(!s.take_due(t0 + ms(399)));
        assert!(s.take_due(t0 + ms(400)));
        // The listing writes to the store while it runs and as it closes.
        s.store_changed(t0 + ms(450), true);
        s.pass_finished(t0 + ms(600), listed(1));
        assert!(
            s.take_due(t0 + ms(1000)),
            "one follow-up, in case a change was the person's"
        );
        // The follow-up lists the same: its writes were its own.
        s.store_changed(t0 + ms(1050), true);
        s.pass_finished(t0 + ms(1200), listed(1));
        assert_eq!(s.wait(t0 + ms(1200)), None);
        for step in 0..100 {
            assert!(!s.take_due(t0 + ms(1200 + step * 100)));
        }
    }

    #[test]
    fn a_change_once_a_pass_has_ended_is_always_read() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.store_changed(t0, false);
        assert!(s.take_due(t0 + ms(400)));
        s.store_changed(t0 + ms(450), true);
        s.pass_finished(t0 + ms(500), listed(1));
        assert!(s.take_due(t0 + ms(900)));
        s.store_changed(t0 + ms(950), true);
        s.pass_finished(t0 + ms(1000), listed(1));
        // Just after the follow-up ended, the person's session writes.
        s.store_changed(t0 + ms(1001), false);
        assert!(s.take_due(t0 + ms(1401)));
    }

    #[test]
    fn a_follow_up_whose_listing_changed_brings_another() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.store_changed(t0, false);
        assert!(s.take_due(t0 + ms(400)));
        s.store_changed(t0 + ms(450), true);
        s.pass_finished(t0 + ms(500), listed(1));
        assert!(s.take_due(t0 + ms(900)));
        // A session was started by hand while the follow-up ran.
        s.store_changed(t0 + ms(950), true);
        s.pass_finished(t0 + ms(1000), listed(2));
        assert!(s.take_due(t0 + ms(1400)));
        s.store_changed(t0 + ms(1450), true);
        s.pass_finished(t0 + ms(1500), listed(2));
        assert_eq!(s.wait(t0 + ms(1500)), None);
    }

    #[test]
    fn a_follow_up_that_classified_after_listing_brings_a_fresh_listing() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.store_changed(t0, false);
        assert!(s.take_due(t0 + ms(400)));
        s.store_changed(t0 + ms(450), true);
        s.pass_finished(t0 + ms(500), listed(1));
        assert!(s.take_due(t0 + ms(900)));
        // A session's export ran after the listing, while the person wrote.
        s.store_changed(t0 + ms(2000), true);
        s.pass_finished(
            t0 + ms(3000),
            PassEnd::Listed {
                fingerprint: 1,
                changed_after: true,
            },
        );
        assert!(s.take_due(t0 + ms(3400)), "a fresh listing, after the person's change");
        s.store_changed(t0 + ms(3450), true);
        s.pass_finished(t0 + ms(3500), listed(1));
        assert_eq!(s.wait(t0 + ms(3500)), None);
    }

    #[test]
    fn a_failing_listing_is_tried_once_more_and_no_further() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.store_changed(t0, false);
        assert!(s.take_due(t0 + ms(400)));
        s.store_changed(t0 + ms(450), true);
        s.pass_finished(t0 + ms(500), PassEnd::Failed);
        assert!(s.take_due(t0 + ms(900)));
        s.store_changed(t0 + ms(950), true);
        s.pass_finished(t0 + ms(1000), PassEnd::Failed);
        assert_eq!(s.wait(t0 + ms(1000)), None);
    }

    #[test]
    fn the_persons_change_before_a_follow_up_makes_it_a_pass_of_its_own() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.store_changed(t0, false);
        assert!(s.take_due(t0 + ms(400)));
        s.store_changed(t0 + ms(450), true);
        s.pass_finished(t0 + ms(500), listed(1));
        // The person writes before the follow-up starts.
        s.store_changed(t0 + ms(600), false);
        assert!(s.take_due(t0 + ms(1000)));
        // It listed before their session was in; their write follows.
        s.store_changed(t0 + ms(1050), true);
        s.pass_finished(t0 + ms(1100), listed(1));
        assert!(s.take_due(t0 + ms(1500)));
    }

    #[test]
    fn a_pass_asked_for_is_not_put_off_by_the_store() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        s.request(t0 + ms(100));
        s.store_changed(t0 + ms(50), false);
        assert!(s.take_due(t0 + ms(100)));
    }

    #[test]
    fn a_store_that_keeps_changing_is_read_within_the_longest_wait() {
        let ms = Duration::from_millis;
        let (mut s, t0) = schedule();
        let mut taken = None;
        for step in 0..100 {
            let now = t0 + ms(step * 100);
            if s.take_due(now) {
                taken = Some(now);
                break;
            }
            s.store_changed(now, false);
        }
        assert_eq!(taken, Some(t0 + ms(2000)));
    }

    #[test]
    fn a_listings_fingerprint_follows_its_sessions_not_their_order() {
        let session = |id: &str, stamp: &str| ExternalSession {
            harness_session_id: id.into(),
            cwd: "/tmp".into(),
            title: None,
            created: None,
            updated: Some(1),
            stamp: Some(stamp.into()),
            is_subagent: false,
        };
        let a = [
            session("a", "2026-01-01T00:00:00.100Z"),
            session("b", "2026-01-01T00:00:00.100Z"),
        ];
        let b = [a[1].clone(), a[0].clone()];
        assert_eq!(listing_fingerprint(&a), listing_fingerprint(&b));
        // Within the same second, a new version is a change.
        let c = [session("a", "2026-01-01T00:00:00.500Z"), a[1].clone()];
        assert_ne!(listing_fingerprint(&a), listing_fingerprint(&c));
    }

    #[test]
    fn detects_generic_subagent_metadata_hints() {
        assert!(!meta_indicates_subagent(None));
        let empty = serde_json::Map::new();
        assert!(!meta_indicates_subagent(Some(&empty)));
        let by_flag = json!({ "isSubagent": true }).as_object().cloned().unwrap();
        assert!(meta_indicates_subagent(Some(&by_flag)));
        let by_parent = json!({ "parentSessionId": "ses_root" }).as_object().cloned().unwrap();
        assert!(meta_indicates_subagent(Some(&by_parent)));
        let blank_parent = json!({ "parentSessionId": "" }).as_object().cloned().unwrap();
        assert!(!meta_indicates_subagent(Some(&blank_parent)));
    }
}
