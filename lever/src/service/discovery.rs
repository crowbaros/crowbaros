//! External harness session discovery coordination, store settling, retry
//! backoff, and on-demand history import for unopened discovered threads.

use super::Service;
use super::wire::{Broadcast, Effects, PeerReply, error_json, initial_title, store_failed, unix_now};
use crate::discovery::{self, DiscoveryRunner, DiscoverySchedule, KnownSessions, PassEnd, SessionDiscoverer};
use crate::harness::{ExternalSession, ModelCatalog, SessionHistory};
use crate::log::{Event, Log};
use crate::sys;
use crate::watcher::StoreWatcher;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// How long a discovery pass waits after Lever's database could not be read,
/// doubling while the reads fail, up to [`DISCOVERY_READ_RETRY_MAX`].
const DISCOVERY_READ_RETRY: Duration = Duration::from_secs(1);
const DISCOVERY_READ_RETRY_MAX: Duration = Duration::from_secs(60);

/// How long the harness's store, once it appears, goes unwritten before a
/// listing may start a harness on it, and the most it waits for that.
const STORE_SETTLE_QUIET: Duration = Duration::from_secs(1);
const STORE_SETTLE_MAX: Duration = Duration::from_secs(3);

/// How long a running harness has to answer a `session/list`, by default.
const DEFAULT_LIVE_LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// Default debounce after a harness's session store changes on disk before
/// Lever lists its sessions to pick up any started outside Lever.
pub const DEFAULT_DISCOVERY_DEBOUNCE: Duration = Duration::from_millis(400);

/// The harness's store, as discovery last saw it. A listing starts the
/// harness, which sets up its store when it has none, and two OpenCode
/// 2.4.1 processes that set one up at once can end one of them: the
/// person's in their terminal, or Lever's own. So no listing starts a
/// harness while the store has no data, nor until whoever made it has had
/// time to set it up.
#[derive(Debug, Clone, Copy)]
enum StoreSeen {
    Absent,
    Settling { appeared: Instant, written: Instant },
    Ready,
}

/// A `send` to an unopened discovered thread, carried out once the thread's
/// history is read. It is carried out even if its peer has gone, as any
/// complete request is: peer IDs are never used again, so its reply reaches
/// no one.
pub(super) struct PendingSend {
    pub(super) peer_id: u64,
    pub(super) is_person: bool,
    pub(super) thread_id: String,
    pub(super) value: Value,
    /// The person stopped the thread before it went: it is answered
    /// `working` and not sent.
    pub(super) stopped: bool,
}

/// What applying a pass's sessions did.
#[derive(Default)]
struct Applied {
    effects: Effects,
    /// Whether a session Lever did not know of is now a thread or known as a
    /// subagent.
    settled: bool,
    /// Whether a session could not be written.
    write_failed: bool,
    /// Whether what Lever knows could not be read, so nothing was applied.
    read_failed: bool,
}

pub(super) struct DiscoveryCoordinator {
    watcher: Option<StoreWatcher>,
    runner: Option<DiscoveryRunner>,
    schedule: DiscoverySchedule,
    debounce: Duration,
    store_seen: StoreSeen,
    /// True while a live `session/list` RPC is in flight on `self.harness`.
    pub(super) live_list_in_flight: bool,
    /// When the live `session/list` in flight was asked.
    live_list_asked: Option<Instant>,
    /// How long a running harness has to answer a `session/list`.
    live_list_timeout: Duration,
    /// True once the running harness left a `session/list` unanswered: until
    /// it is gone, discovery lists with a one-shot harness instead.
    live_list_unanswered: bool,
    /// True if another pass was asked for while a live `session/list` RPC
    /// was already in flight on `self.harness`.
    live_list_rescan_wanted: bool,
    /// How long a pass waits after Lever's database could not be read.
    read_retry: Duration,
    /// How long a pass waits after a listing failed while a session waited
    /// to be tried again, or after what it found could not be written: it
    /// doubles each time, and starts again once a pass's writes all succeed.
    listing_retry: Duration,
    /// Whether a session whose export failed waits to be tried again.
    export_retry_pending: bool,
    /// Whether a session a pass found could not be written, and waits to be.
    write_retry_pending: bool,
    /// Discovered threads whose history load is queued or in flight:
    /// `thread_id -> (harness_session_id, cwd)`.
    pub(super) loading_history: HashMap<String, (String, String)>,
    /// Threads whose `session/load` RPC for history import is already in flight
    /// on the currently running harness.
    dispatched_history: HashSet<String>,
    /// Peers waiting for `resume` on a discovered thread whose history is being
    /// read from its harness.
    pub(super) pending_resumes: Vec<(u64, String)>,
    /// `send` requests targeting an unopened discovered thread, waiting until
    /// its history is read into Lever's record so the new turn follows it.
    pub(super) pending_sends: Vec<PendingSend>,
}

impl DiscoveryCoordinator {
    pub(super) fn new(discoverer: Option<Box<dyn SessionDiscoverer>>, debounce: Duration) -> Self {
        let runner = discoverer.and_then(|d| DiscoveryRunner::new(d).ok());
        let watcher = runner
            .as_ref()
            .and_then(|d| StoreWatcher::new(d.watch_specs()).ok().flatten());
        let mut schedule = DiscoverySchedule::new(debounce, discovery::DISCOVERY_MAX_WAIT);
        let store_seen = if runner.as_ref().is_some_and(DiscoveryRunner::has_store_data) {
            schedule.request(Instant::now());
            StoreSeen::Ready
        } else {
            StoreSeen::Absent
        };
        Self {
            watcher,
            runner,
            schedule,
            debounce,
            store_seen,
            live_list_in_flight: false,
            live_list_asked: None,
            live_list_timeout: DEFAULT_LIVE_LIST_TIMEOUT,
            live_list_unanswered: false,
            live_list_rescan_wanted: false,
            read_retry: DISCOVERY_READ_RETRY,
            listing_retry: DISCOVERY_READ_RETRY,
            export_retry_pending: false,
            write_retry_pending: false,
            loading_history: HashMap::new(),
            dispatched_history: HashSet::new(),
            pending_resumes: Vec::new(),
            pending_sends: Vec::new(),
        }
    }

    pub(super) fn has_active_work(&self) -> bool {
        !self.loading_history.is_empty()
            || !self.pending_resumes.is_empty()
            || !self.pending_sends.is_empty()
            || self.live_list_in_flight
    }

    pub(super) fn has_pending_send(&self, thread_id: &str) -> bool {
        self.pending_sends.iter().any(|p| p.thread_id == thread_id)
    }

    pub(super) fn is_loading_history(&self, thread_id: &str) -> bool {
        self.loading_history.contains_key(thread_id)
    }

    pub(super) fn mark_pending_sends_stopped(&mut self, thread_id: &str) {
        for pending in &mut self.pending_sends {
            if pending.thread_id == thread_id {
                pending.stopped = true;
            }
        }
    }

    pub(super) fn on_peer_gone(&mut self, peer_id: u64) {
        self.pending_resumes.retain(|(id, _)| *id != peer_id);
    }

    pub(super) fn next_wait(&self, now: Instant) -> Option<Duration> {
        self.schedule
            .wait(now)
            .into_iter()
            .chain(
                self.live_list_asked
                    .map(|asked| self.live_list_timeout.saturating_sub(asked.elapsed())),
            )
            .min()
    }

    pub(super) fn clear_history_loads(&mut self) {
        self.loading_history.clear();
        self.dispatched_history.clear();
    }

    pub(super) fn fail_pending(&mut self, message: &str, replies: &mut Vec<PeerReply>) {
        self.clear_history_loads();
        for (peer_id, _) in self.pending_resumes.drain(..) {
            replies.push(PeerReply {
                peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
        for item in self.pending_sends.drain(..) {
            replies.push(PeerReply {
                peer_id: item.peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
    }

    pub(super) fn take_due(&mut self, now: Instant) -> bool {
        self.schedule.take_due(now)
    }
}

impl Service {
    /// Sets how long a running harness has to answer a `session/list`.
    pub fn set_live_list_timeout(&mut self, timeout: Duration) {
        self.discovery.live_list_timeout = timeout;
    }

    pub fn watcher_fd(&self) -> Option<RawFd> {
        self.discovery.watcher.as_ref().map(StoreWatcher::fd)
    }

    pub fn discovery_fd(&self) -> Option<RawFd> {
        self.discovery.runner.as_ref().map(DiscoveryRunner::fd)
    }

    /// Drains `inotify` events from watched harness stores and schedules a
    /// debounced discovery scan when anything changed.
    pub fn on_store_changed(&mut self) {
        if self.discovery.watcher.as_mut().is_some_and(StoreWatcher::drain_changed) && self.discovery.runner.is_some() {
            let now = Instant::now();
            if let StoreSeen::Settling { written, .. } = &mut self.discovery.store_seen {
                *written = now;
            }
            let busy = self.discovery_busy();
            self.discovery.schedule.store_changed(now, busy);
        }
    }

    /// Whether a listing may start a harness on the store now. When it may
    /// not, a pass is asked for when it may, or none while the store has no
    /// data: its appearing brings one.
    fn store_settled(&mut self) -> bool {
        let now = Instant::now();
        let has_data = self
            .discovery
            .runner
            .as_ref()
            .is_some_and(DiscoveryRunner::has_store_data);
        self.discovery.store_seen = match self.discovery.store_seen {
            _ if !has_data => StoreSeen::Absent,
            StoreSeen::Absent => StoreSeen::Settling {
                appeared: now,
                written: now,
            },
            seen => seen,
        };
        let StoreSeen::Settling { appeared, written } = self.discovery.store_seen else {
            return matches!(self.discovery.store_seen, StoreSeen::Ready);
        };
        let due = (written + STORE_SETTLE_QUIET).min(appeared + STORE_SETTLE_MAX);
        if now >= due {
            self.discovery.store_seen = StoreSeen::Ready;
            return true;
        }
        self.discovery.schedule.request(due);
        false
    }

    /// Whether a discovery pass is running: a one-shot listing or
    /// classification on the background thread, or a live `session/list`.
    fn discovery_busy(&self) -> bool {
        self.discovery.live_list_in_flight || self.discovery.runner.as_ref().is_some_and(DiscoveryRunner::in_flight)
    }

    /// A live `session/list` the harness has not answered in
    /// `live_list_timeout` is given up: the pass is lost, and discovery
    /// lists with a one-shot harness while this one runs.
    pub(super) fn give_up_unanswered_live_list(&mut self, log: &mut Log) {
        if !self
            .discovery
            .live_list_asked
            .is_some_and(|asked| asked.elapsed() >= self.discovery.live_list_timeout)
        {
            return;
        }
        if let Some(harness) = self.harness.as_mut() {
            harness.abandon_list_sessions();
        }
        let _ = log.write(Event::Error, "the harness did not answer a session listing");
        self.discovery.live_list_asked = None;
        self.discovery.live_list_in_flight = false;
        self.discovery.live_list_rescan_wanted = false;
        self.discovery.live_list_unanswered = true;
        self.discovery_pass_finished(true, PassEnd::Failed);
    }

    /// A live `session/list` the harness will not answer, as it stopped or
    /// exited, is a lost pass: another runs after a quiet period.
    pub(super) fn lose_live_list(&mut self) {
        self.discovery.live_list_asked = None;
        self.discovery.live_list_unanswered = false;
        let lost = std::mem::take(&mut self.discovery.live_list_in_flight);
        let rescan = std::mem::take(&mut self.discovery.live_list_rescan_wanted);
        if lost || rescan {
            self.discovery_pass_finished(true, PassEnd::Failed);
        }
    }

    /// A discovery pass ended, or was lost; one more runs after a quiet
    /// period if another was asked for while it ran.
    fn discovery_pass_finished(&mut self, rescan: bool, end: PassEnd) {
        let now = Instant::now();
        self.discovery.schedule.pass_finished(now, end);
        if rescan {
            self.discovery.schedule.request(now + self.discovery.debounce);
        }
    }

    /// Collects the completed background discovery scan and records any newly
    /// discovered external sessions as threads in Lever's database.
    pub fn on_discovery_ready(&mut self, log: &mut Log) -> Effects {
        // What the pass's listing wrote to the store was written before it
        // ended: it is read while the pass still counts as running.
        self.on_store_changed();
        let Some((outcome, rescan)) = self.discovery.runner.as_mut().and_then(DiscoveryRunner::take_result) else {
            return Effects::default();
        };
        let (found, fingerprint) = match outcome {
            Ok(done) => done,
            Err(_) => {
                self.listing_failed(rescan, log);
                return Effects::default();
            }
        };
        self.discovery.export_retry_pending = found.retry_at.is_some();
        if let Some(retry) = found.retry_at {
            // A session whose export failed is tried again then, though the
            // store is quiet by then: a follow-up, which brings no run of
            // its own.
            self.discovery.schedule.request_followup(retry);
        }
        let mut quiet_failed = false;
        for (sid, stamp) in &found.quiet {
            if let Err(err) = self.store.remember_quiet_session(sid, stamp) {
                store_failed(log, &err);
                quiet_failed = true;
            }
        }
        // What it found is written first: a pass made progress only when it
        // recorded something it had not, as no session it settled is
        // classified again; one that settled nothing is a follow-up like any.
        let applied = self.apply_discovered_sessions(found.sessions, log);
        let write_failed = applied.write_failed || quiet_failed;
        if !applied.read_failed {
            self.discovery_writes_done(write_failed);
        }
        let settled = !applied.read_failed && (applied.settled || (!found.quiet.is_empty() && !quiet_failed));
        // What is left for the next pass is classified again only once its
        // writes can be: until then, the write's own retry paces it.
        let more = found.more && !write_failed && !applied.read_failed;
        self.discovery_pass_finished(
            rescan || more,
            PassEnd::Listed {
                fingerprint,
                changed_after: found.changed_after,
                progressed: settled || more,
            },
        );
        applied.effects
    }

    /// A pass's writes have all been tried: one that failed is tried again
    /// later each time, and once they all succeed the wait starts again.
    fn discovery_writes_done(&mut self, failed: bool) {
        if failed {
            self.discovery_write_failed();
        } else {
            self.discovery.write_retry_pending = false;
            self.discovery.listing_retry = DISCOVERY_READ_RETRY;
        }
    }

    /// What a pass found could not be written: it is listed again, later
    /// each time while it cannot be, as a follow-up, so its own writes to
    /// the store bring no run of follow-ups of their own.
    fn discovery_write_failed(&mut self) {
        self.discovery.write_retry_pending = true;
        self.discovery
            .schedule
            .request_followup(Instant::now() + self.discovery.listing_retry);
        self.discovery.listing_retry = (self.discovery.listing_retry * 2).min(DISCOVERY_READ_RETRY_MAX);
    }

    pub(super) fn trigger_discovery(&mut self, log: &mut Log) {
        // One pass at a time: a classification still running takes this one
        // after it.
        match self.discovery.runner.as_mut() {
            None => return,
            Some(runner) if runner.in_flight() => {
                runner.want_rescan();
                return;
            }
            Some(_) => {}
        }
        // When Lever's own harness is already running and supports ACP
        // `session/list`, ask it over the existing pipe instead of spawning a
        // second harness process on every write Lever's own turns make.
        if let Some(harness) = self.harness.as_mut()
            && harness.capabilities().supports_list_sessions
            && !self.discovery.live_list_unanswered
        {
            if harness.is_listing_sessions() {
                self.discovery.live_list_rescan_wanted = true;
            } else {
                self.discovery.live_list_rescan_wanted = false;
                self.discovery.live_list_in_flight = harness.list_sessions();
                self.discovery.live_list_asked = self.discovery.live_list_in_flight.then(Instant::now);
            }
            return;
        }
        if !self.store_settled() {
            return;
        }
        let Some(known) = self.known_sessions_for_discovery(log) else {
            return;
        };
        if let Some(runner) = self.discovery.runner.as_mut() {
            runner.request_scan(known);
        }
    }

    /// Handles `HarnessEvent::SessionsListed`.
    pub(super) fn on_sessions_listed(
        &mut self,
        result: Result<Vec<ExternalSession>, String>,
        log: &mut Log,
    ) -> Effects {
        // What the listing wrote, it wrote before it answered.
        self.on_store_changed();
        self.discovery.live_list_in_flight = false;
        self.discovery.live_list_asked = None;
        let rescan = std::mem::take(&mut self.discovery.live_list_rescan_wanted);
        let sessions = match result {
            Ok(sessions) => sessions,
            Err(_) => {
                self.listing_failed(rescan, log);
                return Effects::default();
            }
        };
        let needs_classify = self
            .discovery
            .runner
            .as_ref()
            .is_some_and(DiscoveryRunner::needs_classification);
        if needs_classify {
            let Some(known) = self.known_sessions_for_discovery(log) else {
                self.discovery_pass_finished(rescan, PassEnd::Failed);
                return Effects::default();
            };
            let has_unseen = sessions.iter().any(|s| known.unseen(s));
            if has_unseen && let Some(runner) = self.discovery.runner.as_mut() {
                // The pass goes on in the classification; a rescan asked for
                // meanwhile follows it.
                runner.request_classify(sessions, known);
                if rescan {
                    runner.want_rescan();
                }
                return Effects::default();
            }
        }
        let fingerprint = discovery::listing_fingerprint(&sessions);
        // Nothing in it waits to be classified any more.
        self.discovery.export_retry_pending = false;
        let applied = self.apply_discovered_sessions(sessions, log);
        if !applied.read_failed {
            self.discovery_writes_done(applied.write_failed);
        }
        self.discovery_pass_finished(
            rescan,
            PassEnd::Listed {
                fingerprint,
                // A write seen while the running harness listed may be its own
                // turn's or the person's, after it read its sessions: it is
                // listed again, a few times at most while nothing it lists
                // changes.
                changed_after: true,
                progressed: applied.settled,
            },
        );
        applied.effects
    }

    /// What Lever knows of the harness's sessions, or `None` when its
    /// database could not be read: without it, Lever's own sessions and
    /// deleted ones would be taken for new.
    fn known_sessions_for_discovery(&mut self, log: &mut Log) -> Option<KnownSessions> {
        let read = self
            .store
            .known_harness_sessions()
            .and_then(|known| Ok((known, self.store.quiet_sessions()?)));
        let ((mut owned, mut ignored), quiet) = match read {
            Ok(read) => read,
            Err(err) => {
                self.discovery_read_failed(&err, log);
                return None;
            }
        };
        self.discovery.read_retry = DISCOVERY_READ_RETRY;
        if let Some(harness) = self.harness.as_ref() {
            for sid in harness.own_session_ids() {
                owned.insert(sid);
            }
        }
        for cid in &self.turns.harness_children {
            ignored.insert(cid.clone());
        }
        Some(KnownSessions { owned, ignored, quiet })
    }

    /// A listing failed. A session waiting to be tried again, or to be
    /// written, still waits: the pass that was to try it is tried again, later
    /// each time until a listing succeeds.
    fn listing_failed(&mut self, rescan: bool, log: &mut Log) {
        self.discovery_pass_finished(rescan, PassEnd::Failed);
        let _ = log.write(Event::Error, "could not list external harness sessions");
        if self.discovery.export_retry_pending || self.discovery.write_retry_pending {
            self.discovery
                .schedule
                .request_followup(Instant::now() + self.discovery.listing_retry);
            self.discovery.listing_retry = (self.discovery.listing_retry * 2).min(DISCOVERY_READ_RETRY_MAX);
        }
    }

    /// Lever's database could not be read for a pass: the pass is listed
    /// again after a wait that doubles while the reads fail.
    fn discovery_read_failed(&mut self, err: &str, log: &mut Log) {
        store_failed(log, err);
        self.discovery
            .schedule
            .request(Instant::now() + self.discovery.read_retry);
        self.discovery.read_retry = (self.discovery.read_retry * 2).min(DISCOVERY_READ_RETRY_MAX);
    }

    fn apply_discovered_sessions(&mut self, sessions: Vec<ExternalSession>, log: &mut Log) -> Applied {
        // Without what Lever knows of, a deleted thread could come back.
        let read = self
            .store
            .known_harness_sessions()
            .and_then(|known| Ok((known, self.store.quiet_sessions()?)));
        let ((known_owned, known_ignored), quiet) = match read {
            Ok(read) => {
                self.discovery.read_retry = DISCOVERY_READ_RETRY;
                read
            }
            Err(err) => {
                // What was listed is not lost: it is listed again.
                self.discovery_read_failed(&err, log);
                return Applied {
                    read_failed: true,
                    ..Applied::default()
                };
            }
        };
        let live_own: HashSet<String> = self
            .harness
            .as_ref()
            .map(|h| h.own_session_ids().into_iter().collect())
            .unwrap_or_default();
        let harness_name = self.harness_config.as_ref().map(|c| c.name.clone());
        let default_mode = self.default_mode();
        let now = unix_now();
        let mut broadcasts = Vec::new();
        let mut create_failed = false;
        // Whether a session Lever did not know of is now a thread or known
        // as a subagent.
        let mut settled = false;

        for session in sessions {
            // One found empty or archived, unchanged since, is no thread.
            if session.stamp.is_some() && quiet.get(&session.harness_session_id) == session.stamp.as_ref() {
                continue;
            }
            let sid = session.harness_session_id;
            if sid.is_empty() {
                continue;
            }
            if session.is_subagent {
                match self.store.remember_ignored_session(&sid, "subagent", now) {
                    Ok(()) => settled |= !known_ignored.contains(&sid),
                    Err(err) => {
                        store_failed(log, &err);
                        create_failed = true;
                    }
                }
                continue;
            }
            if known_ignored.contains(&sid) || self.turns.harness_children.contains(&sid) {
                continue;
            }
            // Skip probe or in-flight sessions currently held by Lever's own
            // harness process that are not yet tied to a thread row.
            if live_own.contains(&sid) && !known_owned.contains(&sid) {
                continue;
            }
            match self.store.thread_by_harness_session(&sid) {
                Ok(Some((tid, cur_title, _cur_updated, needs_history))) => {
                    if needs_history {
                        let updated = session.updated.unwrap_or(now);
                        match self
                            .store
                            .update_discovered_thread(&tid, session.title.as_deref(), updated)
                        {
                            Ok(true) => {
                                let title = session.title.as_deref().unwrap_or(&cur_title);
                                broadcasts.push(Broadcast {
                                    thread_id: tid.clone(),
                                    message: json!({
                                        "event": "thread-updated",
                                        "thread": tid,
                                        "title": title,
                                    }),
                                });
                            }
                            Ok(false) => {}
                            Err(err) => store_failed(log, &err),
                        }
                    }
                }
                Ok(None) => {
                    let tid = format!("t-{}", sys::random_hex(6));
                    let title = session
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Thread");
                    let updated = session.updated.unwrap_or(now);
                    let created = session.created.unwrap_or(updated);
                    match self.store.create_discovered_thread(
                        &tid,
                        title,
                        &session.cwd,
                        harness_name.as_deref(),
                        &sid,
                        &default_mode,
                        created,
                        updated,
                    ) {
                        Ok(()) => {
                            settled = true;
                            broadcasts.push(Broadcast {
                                thread_id: tid.clone(),
                                message: json!({
                                    "event": "thread-updated",
                                    "thread": tid,
                                    "title": title,
                                }),
                            });
                        }
                        Err(err) => {
                            store_failed(log, &err);
                            // Listed again, so it is not left out, later each
                            // time while it cannot be written.
                            create_failed = true;
                        }
                    }
                }
                Err(err) => {
                    store_failed(log, &err);
                    create_failed = true;
                }
            }
        }
        Applied {
            effects: Effects {
                broadcasts,
                replies: Vec::new(),
            },
            settled,
            write_failed: create_failed,
            read_failed: false,
        }
    }

    pub(super) fn ensure_history_load(
        &mut self,
        thread_id: &str,
        session_id: &str,
        cwd: &str,
        log: &mut Log,
    ) -> Result<(), (&'static str, String)> {
        if self.harness_config.is_none() {
            return Err(("no-harness", "No harness is available.".to_string()));
        }
        // Its session may be gone by the time a load would answer.
        if self.turns.deleting(thread_id) {
            return Err(("working", "This thread is being deleted.".to_string()));
        }
        if self.harness_env_stale {
            return Err((
                "working",
                "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.".to_string(),
            ));
        }
        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            return Err(("harness-error", err));
        }
        self.idle_since = None;
        self.discovery
            .loading_history
            .entry(thread_id.to_string())
            .or_insert_with(|| (session_id.to_string(), cwd.to_string()));
        if self.harness.as_ref().is_some_and(|h| h.is_initialized())
            && !self.discovery.dispatched_history.contains(thread_id)
        {
            let mcp_servers = self.mcp_servers_for_thread(thread_id);
            let loaded = self
                .harness
                .as_mut()
                .is_some_and(|h| h.load_session_history(thread_id, session_id, cwd, &mcp_servers));
            if !loaded {
                self.discovery.loading_history.remove(thread_id);
                return Err((
                    "harness-error",
                    "The harness does not support loading session history.".to_string(),
                ));
            }
            self.discovery.dispatched_history.insert(thread_id.to_string());
        }
        Ok(())
    }

    pub(super) fn dispatch_history_loads(&mut self) -> Effects {
        let mut effects = Effects::default();
        let pending: Vec<(String, String, String)> = self
            .discovery
            .loading_history
            .iter()
            .filter(|(tid, _)| !self.discovery.dispatched_history.contains(*tid))
            .map(|(tid, (sid, cwd))| (tid.clone(), sid.clone(), cwd.clone()))
            .collect();
        for (thread_id, session_id, cwd) in pending {
            let mcp_servers = self.mcp_servers_for_thread(&thread_id);
            let loaded = self
                .harness
                .as_mut()
                .is_some_and(|h| h.load_session_history(&thread_id, &session_id, &cwd, &mcp_servers));
            if loaded {
                self.discovery.dispatched_history.insert(thread_id);
            } else {
                effects.extend(
                    self.fail_history_load(&thread_id, "The harness does not support loading session history."),
                );
            }
        }
        effects
    }

    pub(super) fn finish_history_loaded(
        &mut self,
        thread_id: &str,
        history: SessionHistory,
        catalog: Option<ModelCatalog>,
        log: &mut Log,
    ) -> Effects {
        self.discovery.loading_history.remove(thread_id);
        self.discovery.dispatched_history.remove(thread_id);
        let mut effects = Effects::default();
        if let Some(cat) = catalog {
            self.apply_model_catalog(cat, false, log, &mut effects);
        }
        let fallback_title = history
            .turns
            .first()
            .map(|t| initial_title(&t.message))
            .filter(|s| !s.is_empty());
        // An effort the person chose before opening it is theirs, and the
        // session's own does not replace it.
        let chosen_effort = self
            .store
            .thread_meta(thread_id)
            .ok()
            .flatten()
            .and_then(|meta| meta.effort);
        if let Err(err) = self
            .store
            .import_thread_history(thread_id, &history, fallback_title.as_deref(), unix_now())
        {
            store_failed(log, &err);
            // Not imported, so not yet the thread's: the next open loads it again.
            if let Some(h) = self.harness.as_mut() {
                h.forget_session(thread_id);
            }
            effects.extend(self.fail_history_waiters(thread_id, "storage-error", &err));
            return effects;
        }
        if chosen_effort.is_none() {
            effects.broadcasts.extend(self.mirror_effort(thread_id, log));
        } else {
            self.note_session_efforts(thread_id);
        }

        let resumed_outcome = self.resume_from_store(thread_id);
        let resumed_reply = resumed_outcome
            .reply
            .unwrap_or_else(|| error_json("storage-error", "Could not read imported thread."));
        let is_resumed = resumed_reply.get("reply").and_then(Value::as_str) == Some("resumed");

        // Broadcast `thread-updated` so thread list watchers update turn count,
        // state, title, and model immediately.
        if is_resumed {
            let mut upd = json!({
                "event": "thread-updated",
                "thread": thread_id,
                "title": resumed_reply["title"],
            });
            if let Some(model) = resumed_reply.get("model").and_then(Value::as_str) {
                upd["model"] = json!(model);
            }
            effects.broadcasts.push(Broadcast {
                thread_id: thread_id.to_string(),
                message: upd,
            });
        }

        let mut remaining_resumes = Vec::new();
        for (peer_id, tid) in std::mem::take(&mut self.discovery.pending_resumes) {
            if tid == thread_id {
                effects.replies.push(PeerReply {
                    peer_id,
                    message: resumed_reply.clone(),
                    subscribe: is_resumed.then(|| thread_id.to_string()),
                });
            } else {
                remaining_resumes.push((peer_id, tid));
            }
        }
        self.discovery.pending_resumes = remaining_resumes;

        // Replay any `send` requests that arrived for this unopened thread
        // before its history finished loading.
        let mut remaining_sends = Vec::new();
        let mut ready_sends = Vec::new();
        for item in std::mem::take(&mut self.discovery.pending_sends) {
            if item.thread_id == thread_id {
                ready_sends.push(item);
            } else {
                remaining_sends.push(item);
            }
        }
        self.discovery.pending_sends = remaining_sends;
        for item in ready_sends {
            if item.stopped {
                effects.replies.push(PeerReply {
                    peer_id: item.peer_id,
                    message: error_json(
                        "working",
                        "The thread was stopped before this message was sent. Send it again.",
                    ),
                    subscribe: None,
                });
                continue;
            }
            let outcome = self.send_message(item.peer_id, item.is_person, &item.value, None, log);
            effects.broadcasts.extend(outcome.broadcasts);
            if let Some(reply) = outcome.reply {
                effects.replies.push(PeerReply {
                    peer_id: item.peer_id,
                    message: reply,
                    subscribe: outcome.subscribe,
                });
            }
        }

        effects
    }

    pub(super) fn fail_history_load(&mut self, thread_id: &str, message: &str) -> Effects {
        self.discovery.loading_history.remove(thread_id);
        self.discovery.dispatched_history.remove(thread_id);
        self.fail_history_waiters(thread_id, "harness-error", message)
    }

    fn fail_history_waiters(&mut self, thread_id: &str, code: &str, message: &str) -> Effects {
        let mut replies = Vec::new();
        let mut remaining_resumes = Vec::new();
        for (peer_id, tid) in std::mem::take(&mut self.discovery.pending_resumes) {
            if tid == thread_id {
                replies.push(PeerReply {
                    peer_id,
                    message: error_json(code, message),
                    subscribe: None,
                });
            } else {
                remaining_resumes.push((peer_id, tid));
            }
        }
        self.discovery.pending_resumes = remaining_resumes;
        let mut remaining_sends = Vec::new();
        for item in std::mem::take(&mut self.discovery.pending_sends) {
            if item.thread_id == thread_id {
                replies.push(PeerReply {
                    peer_id: item.peer_id,
                    message: error_json(code, message),
                    subscribe: None,
                });
            } else {
                remaining_sends.push(item);
            }
        }
        self.discovery.pending_sends = remaining_sends;
        Effects {
            broadcasts: Vec::new(),
            replies,
        }
    }
}
