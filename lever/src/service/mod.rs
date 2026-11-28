//! Lever's core service controller: owns the SQLite thread store, the on-demand
//! harness lifecycle (including the idle timer), active turns across concurrent
//! threads, discovery, catalog caching, and Lever's wire messages.

mod catalog;
mod discovery;
mod events;
mod journal;
mod mcp_tools;
mod requests;
mod turns;
mod wire;

pub use discovery::DEFAULT_DISCOVERY_DEBOUNCE;
pub use wire::{Broadcast, Effects, PeerReply, RequestOutcome, THREAD_LIST_SUBSCRIPTION, unix_now};

use catalog::CatalogState;
use discovery::DiscoveryCoordinator;
use journal::WriteBehindJournal;
use mcp_tools::McpTools;
use turns::{TurnDispatch, TurnTracker, WorkingSubagent};
use wire::{store_failed, subagent_event_json};

use crate::acp::{self, AcpHarnessFactory};
use crate::discovery::SessionDiscoverer;
use crate::harness::{Harness, HarnessConfig, HarnessEvent, HarnessFactory, HarnessLaunchOptions};
use crate::log::{Event, Log};
use crate::store::{Store, SubagentRecord};
use crate::sys;
use serde_json::json;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Default time an idle harness stays running after the last working turn ends.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Default time a harness has to end a turn after Lever asks it to stop.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the harness says nothing in a turn it began itself before Lever
/// takes that turn as done. ACP gives such a turn no end of its own.
pub const DEFAULT_WAKE_QUIET: Duration = Duration::from_secs(10);

pub struct Service {
    store: Store,
    harness_config: Option<HarnessConfig>,
    fulcrum_socket: Option<PathBuf>,
    harness: Option<Box<dyn Harness>>,
    harness_factory: Box<dyn HarnessFactory>,
    harness_gen: u64,
    idle_timeout: Duration,
    stop_timeout: Duration,
    wake_quiet: Duration,
    idle_since: Option<Instant>,
    /// True when `free_hosted_models` changed while the harness was busy, so
    /// it should be stopped as soon as all active work finishes.
    harness_env_stale: bool,
    /// Why the harness is to be stopped once the event in hand is applied.
    kill_wanted: Option<&'static str>,
    /// Lever is applying a batch of what the harness sent.
    in_harness_batch: bool,
    turns: TurnTracker,
    discovery: DiscoveryCoordinator,
    catalog: CatalogState,
    journal: WriteBehindJournal,
    mcp: McpTools,
}

impl Service {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        harness_config: Option<HarnessConfig>,
        fulcrum_socket: Option<PathBuf>,
        tool_socket: PathBuf,
        idle_timeout: Duration,
        stop_timeout: Duration,
        wake_quiet: Duration,
        discovery_debounce: Duration,
    ) -> Self {
        let discoverer = harness_config.as_ref().and_then(acp::discoverer_for_harness);
        Self::with_harness(
            store,
            harness_config,
            Box::new(AcpHarnessFactory),
            discoverer,
            fulcrum_socket,
            tool_socket,
            idle_timeout,
            stop_timeout,
            wake_quiet,
            discovery_debounce,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_harness(
        store: Store,
        harness_config: Option<HarnessConfig>,
        harness_factory: Box<dyn HarnessFactory>,
        discoverer: Option<Box<dyn SessionDiscoverer>>,
        fulcrum_socket: Option<PathBuf>,
        tool_socket: PathBuf,
        idle_timeout: Duration,
        stop_timeout: Duration,
        wake_quiet: Duration,
        discovery_debounce: Duration,
    ) -> Self {
        Self {
            store,
            harness_config,
            fulcrum_socket,
            harness: None,
            harness_factory,
            harness_gen: 0,
            idle_timeout,
            stop_timeout,
            wake_quiet,
            idle_since: None,
            harness_env_stale: false,
            kill_wanted: None,
            in_harness_batch: false,
            turns: TurnTracker::default(),
            discovery: DiscoveryCoordinator::new(discoverer, discovery_debounce),
            catalog: CatalogState::default(),
            journal: WriteBehindJournal::default(),
            mcp: McpTools::new(tool_socket),
        }
    }

    pub fn harness_stdout_fd(&self) -> Option<RawFd> {
        self.harness.as_ref().and_then(|h| h.stdout_fd())
    }

    pub fn harness_stdin_fd(&self) -> Option<RawFd> {
        self.harness
            .as_ref()
            .filter(|h| h.wants_write())
            .and_then(|h| h.stdin_fd())
    }

    fn holds_prompt_during_subagent_request(&self) -> bool {
        self.harness
            .as_ref()
            .is_some_and(|h| h.capabilities().holds_prompt_during_subagent_request)
    }

    fn subagent_request_holds_prompt(&self, thread_id: &str) -> bool {
        self.holds_prompt_during_subagent_request() && self.turns.has_waiting_subagent_request(thread_id)
    }

    fn has_active_work(&self) -> bool {
        self.turns.has_active_work()
            || self.catalog.has_active_work()
            || self.discovery.has_active_work()
            || self.harness.as_ref().is_some_and(|h| h.probing_models())
    }

    /// Timeout in milliseconds for `poll(2)`, or `-1` when Lever waits for
    /// nothing but its peers and its harness.
    pub fn poll_timeout_ms(&self) -> libc::c_int {
        let retry = self.journal.retry_wait();
        let stop = self
            .turns
            .working_turns
            .values()
            .filter_map(|t| t.stop_asked)
            .chain(self.turns.working_subagents.values().filter_map(|s| s.stop_asked))
            .map(|asked| self.stop_timeout.saturating_sub(asked.elapsed()))
            .chain(self.catalog.min_mode_wait(self.stop_timeout))
            .min();
        let quiet = self
            .turns
            .working_turns
            .iter()
            .filter_map(|(tid, active)| self.turns.harness_turn_quiet_in(tid, active, self.wake_quiet))
            .min();
        let discovery = self.discovery.next_wait(Instant::now());
        match [self.idle_remaining(), retry, stop, quiet, discovery]
            .into_iter()
            .flatten()
            .min()
        {
            None => -1,
            // Rounded up: the last part of a millisecond is waited for, not
            // spun through.
            Some(wait) => wait.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as libc::c_int,
        }
    }

    /// How long the harness may still sit idle, when it is idle.
    fn idle_remaining(&self) -> Option<Duration> {
        let since = self.idle_since?;
        if self.harness.is_none() || self.has_active_work() {
            return None;
        }
        Some(self.idle_timeout.saturating_sub(since.elapsed()))
    }

    /// What Lever does each time it wakes: saves the ends of turns the
    /// database did not take before, carries out a Stop the harness did not,
    /// stops a harness whose environment changed once it is idle, and stops
    /// the harness if no thread has worked for `idle_timeout`.
    pub fn on_wake(&mut self, log: &mut Log) -> Effects {
        sys::reap_orphans(self.harness.as_ref().and_then(|h| h.pid()));
        self.give_up_unanswered_live_list(log);
        if self.discovery.take_due(Instant::now()) {
            self.trigger_discovery(log);
        }
        self.save_endings();
        self.send_held_prompts();
        let mut effects = Effects::default();
        // A turn the harness began that it has said nothing in since the
        // person stopped it, with nothing of it still working, has stopped:
        // the harness need not be.
        let mut stopped_quiet: Vec<(String, u64)> = self
            .turns
            .working_turns
            .iter()
            .filter(|(tid, t)| {
                let TurnDispatch::HarnessStarted {
                    last_heard, open_acts, ..
                } = &t.dispatch
                else {
                    return false;
                };
                // What an older turn still does does not hold this one: a
                // subagent of it stopped on its own is given its own time.
                t.stop_asked
                    .is_some_and(|asked| asked.elapsed() >= self.stop_timeout && *last_heard <= asked)
                    && open_acts.is_empty()
                    && !self
                        .turns
                        .waiting
                        .values()
                        .any(|w| &w.thread_id == *tid && w.turn == t.turn)
                    && !self
                        .turns
                        .working_subagents
                        .values()
                        .any(|s| &s.thread_id == *tid && s.turn == t.turn)
            })
            .map(|(tid, t)| (tid.clone(), t.turn))
            .collect();
        stopped_quiet.sort();
        for (thread_id, turn) in stopped_quiet {
            self.turns.working_turns.remove(&thread_id);
            self.end_turn(&thread_id, turn, "stopped", None, log);
            effects.broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: json!({ "event": "stopped", "thread": thread_id, "turn": turn }),
            });
        }
        let overdue = self
            .turns
            .working_turns
            .iter()
            .find(|(_, t)| t.stop_asked.is_some_and(|asked| asked.elapsed() >= self.stop_timeout))
            .map(|(tid, t)| (tid.clone(), t.turn));
        if let Some((thread_id, turn)) = overdue {
            effects.extend(self.force_stop(&thread_id, turn, log));
        }
        // A turn the harness began that has gone quiet is done.
        let mut quiet: Vec<(String, u64)> = self
            .turns
            .working_turns
            .iter()
            .filter(|(tid, active)| {
                self.turns
                    .harness_turn_quiet_in(tid, active, self.wake_quiet)
                    .is_some_and(|left| left.is_zero())
            })
            .map(|(tid, active)| (tid.clone(), active.turn))
            .collect();
        quiet.sort();
        for (thread_id, turn) in quiet {
            effects.broadcasts.push(self.end_harness_turn(&thread_id, turn, log));
        }
        self.refresh_idle_timer();
        let overdue_subagent = self
            .turns
            .working_subagents
            .values()
            .any(|s| s.stop_asked.is_some_and(|asked| asked.elapsed() >= self.stop_timeout));
        if overdue_subagent {
            effects.extend(self.kill_harness("it did not stop a subagent when asked", log));
        }
        // A harness that has not answered a mode change in that time, a
        // stopped turn's or one set between turns, is ended too: until it
        // answers, the thread is held, and its session's mode is not known.
        if self.catalog.has_overdue_mode(self.stop_timeout) && self.harness.is_some() {
            effects.extend(self.kill_harness("it did not answer a change of mode", log));
        }
        if self.harness_env_stale && !self.has_active_work() {
            self.stop_harness("after free hosted Models setting changed", log);
        } else if self.catalog.models_refresh_pending && !self.has_active_work() {
            self.catalog.models_refresh_pending = false;
            self.stop_harness("to refresh Models after sign-in", log);
            self.read_what_the_harness_offers(log);
        } else if self.idle_remaining().is_some_and(|left| left.is_zero()) {
            self.stop_harness("after idle timeout", log);
        }
        effects.broadcasts.extend(self.drain_queues(log));
        effects.broadcasts.extend(self.turns.activity_changed());
        effects
    }

    /// Drops any deferred replies waiting for a peer that has disconnected.
    /// An in-flight `session/set_config_option` RPC keeps its thread locked
    /// until the harness answers, with `peer_id` cleared so no reply is sent.
    pub fn on_peer_gone(&mut self, peer_id: u64) {
        self.catalog.on_peer_gone(peer_id);
        self.turns.on_peer_gone(peer_id);
        self.discovery.on_peer_gone(peer_id);
        self.refresh_idle_timer();
    }

    pub fn on_harness_writable(&mut self, log: &mut Log) -> Effects {
        let ok = self.harness.as_mut().is_none_or(|h| h.on_writable());
        if !ok {
            let mut effects = self.on_harness_exited(log);
            effects.broadcasts.extend(self.drain_queues(log));
            effects.broadcasts.extend(self.turns.activity_changed());
            return effects;
        }
        Effects::default()
    }

    pub fn on_harness_readable(&mut self, log: &mut Log) -> Effects {
        let events = match self.harness.as_mut() {
            Some(h) => h.on_readable(),
            None => return Effects::default(),
        };
        // A session Lever opened only to read what the harness offers is
        // never a thread, after this harness is gone too: it is remembered
        // before anything in the batch can end the harness.
        let probes = self
            .harness
            .as_mut()
            .map(|h| h.take_new_probe_sessions())
            .unwrap_or_default();
        for probe in probes {
            if let Err(err) = self.store.remember_ignored_session(&probe, "probe", unix_now()) {
                store_failed(log, &err);
            }
        }
        let mut effects = Effects::default();
        let batch_gen = self.harness_gen;
        self.in_harness_batch = true;
        for ev in events {
            match ev {
                HarnessEvent::Exited => {
                    effects.extend(self.on_harness_exited(log));
                    break;
                }
                other => effects.extend(self.apply_harness_event(other, log)),
            }
            if let Some(why) = self.kill_wanted.take() {
                effects.extend(self.kill_harness(why, log));
            }
            // What came after in the same read was the stopped harness's:
            // it says nothing of the work Lever has ended.
            if self.harness_gen != batch_gen || self.harness.is_none() {
                break;
            }
        }
        self.in_harness_batch = false;
        // A request the harness withdrew, or a subagent's end, can free a
        // prompt that waited on it, as can the end of the batch.
        self.send_held_prompts();
        if self.harness_env_stale && !self.has_active_work() {
            self.stop_harness("after free hosted Models setting changed", log);
        } else if self.catalog.models_refresh_pending && !self.has_active_work() {
            self.catalog.models_refresh_pending = false;
            self.stop_harness("to refresh Models after sign-in", log);
            self.read_what_the_harness_offers(log);
        }
        self.refresh_idle_timer();
        effects.broadcasts.extend(self.drain_queues(log));
        effects.broadcasts.extend(self.turns.activity_changed());
        effects
    }

    pub fn shutdown(mut self, log: &mut Log) -> Result<(), String> {
        if self.harness.is_some() {
            self.stop_harness("on Lever stop", log);
        }
        // How requests and turns ended goes in first, if the database takes
        // it now. What was working ends here, and the record says so now,
        // not at the next start.
        self.save_endings();
        if let Err(err) = self.store.interrupt_all_working(unix_now()) {
            store_failed(log, &err);
        }
        if let Err(err) = self.store.withdraw_all_waiting() {
            store_failed(log, &err);
        }
        self.save_endings();
        self.store.close()
    }

    fn start_harness(&mut self, log: &mut Log) -> Result<(), String> {
        let cfg = self
            .harness_config
            .as_ref()
            .ok_or_else(|| "No harness is available.".to_string())?
            .clone();
        let free_hosted_models = self.free_hosted_models();
        let harness = self
            .harness_factory
            .spawn(&cfg, HarnessLaunchOptions { free_hosted_models })?;
        let started_msg = match harness.pid() {
            Some(pid) => format!("started {} (process {pid})", cfg.name),
            None => format!("started {}", cfg.name),
        };
        let _ = log.write(Event::Harness, &started_msg);
        self.harness = Some(harness);
        self.harness_gen = self.harness_gen.wrapping_add(1);
        self.mcp.reset_for_harness();
        self.turns.pending_session_resets.clear();
        self.catalog.reset_on_harness_start();
        self.harness_env_stale = false;
        Ok(())
    }

    fn stop_harness(&mut self, reason: &str, log: &mut Log) {
        if let Some(harness) = self.harness.take() {
            self.harness_gen = self.harness_gen.wrapping_add(1);
            let pid = harness.pid();
            harness.stop();
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let msg = match pid {
                Some(pid) => format!("stopped {name} (process {pid}) {reason}"),
                None => format!("stopped {name} {reason}"),
            };
            let _ = log.write(Event::Harness, &msg);
        }
        self.mcp.clear();
        self.turns.clear_harness_ephemeral();
        self.catalog.clear_on_harness_stop();
        self.harness_env_stale = false;
        self.lose_live_list();
        self.idle_since = None;
        self.kill_wanted = None;
        self.discovery.clear_history_loads();
    }

    /// Stops the harness and every process under it, at once, for `why`, and
    /// ends what it was working on.
    fn kill_harness(&mut self, why: &str, log: &mut Log) -> Effects {
        if let Some(harness) = self.harness.take() {
            self.harness_gen = self.harness_gen.wrapping_add(1);
            let pid = harness.pid();
            harness.kill_tree();
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let msg = match pid {
                Some(pid) => format!("stopped {name} (process {pid}) and every process under it: {why}"),
                None => format!("stopped {name} and every process under it: {why}"),
            };
            let _ = log.write(Event::Harness, &msg);
        }
        self.after_harness_gone("The harness was stopped.", log)
    }

    fn on_harness_exited(&mut self, log: &mut Log) -> Effects {
        if let Some(harness) = self.harness.take() {
            self.harness_gen = self.harness_gen.wrapping_add(1);
            let pid = harness.pid();
            let fault = harness.fault();
            // A turn or subagent the person asked to stop ends with everything
            // it started, though the harness ended before it said so.
            if self.turns.working_turns.values().any(|t| t.stop_asked.is_some())
                || self.turns.working_subagents.values().any(|s| s.stop_asked.is_some())
            {
                harness.kill_tree();
            } else {
                harness.stop();
            }
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let who = match pid {
                Some(pid) => format!("{name} (process {pid})"),
                None => name.to_string(),
            };
            let what = match fault {
                Some(fault) => format!("{who} was stopped: it {fault}"),
                None => format!("{who} exited unexpectedly"),
            };
            let _ = log.write(Event::Harness, &what);
        }
        self.after_harness_gone("The harness exited unexpectedly.", log)
    }

    /// What follows a harness that is gone: what waited on it fails, every
    /// request it asked is withdrawn, and every turn or subagent it was
    /// working on is interrupted.
    fn after_harness_gone(&mut self, why: &str, log: &mut Log) -> Effects {
        self.mcp.clear();
        self.turns.clear_harness_ephemeral();
        self.catalog.clear_on_harness_stop();
        self.harness_env_stale = false;
        self.kill_wanted = None;
        self.lose_live_list();
        self.idle_since = None;

        let mut replies = self.fail_pending_peer_requests(why);
        self.catalog.modes_in_flight.clear();
        let mut broadcasts = self.withdraw_requests(|_| true, log);
        // What the harness had yet to say it took into a turn waits behind it.
        for pending in std::mem::take(&mut self.turns.pending_steers) {
            let effects = self.queue_untaken(pending, log);
            replies.extend(effects.replies);
            broadcasts.extend(effects.broadcasts);
        }

        // Every working subagent is interrupted, or stopped when the person
        // had asked to stop it.
        let now = unix_now();
        let mut ended_subs: Vec<WorkingSubagent> = self.turns.working_subagents.drain().map(|(_, s)| s).collect();
        ended_subs.sort_by(|a, b| a.thread_id.cmp(&b.thread_id).then(a.subagent_id.cmp(&b.subagent_id)));
        for sub in ended_subs {
            let state = if sub.stop_asked.is_some() {
                "stopped"
            } else {
                "interrupted"
            };
            self.end_subagent(&sub.thread_id, &sub.subagent_id, state, None, log);
            let record = SubagentRecord {
                id: sub.subagent_id,
                title: sub.title,
                state: state.to_string(),
                error: None,
                started: sub.started,
                ended: Some(now),
                messages: Vec::new(),
                acts: Vec::new(),
            };
            broadcasts.push(Broadcast {
                thread_id: sub.thread_id.clone(),
                message: subagent_event_json(&sub.thread_id, sub.turn, &record),
            });
        }

        // Every turn that was working is interrupted, or stopped when the
        // person had asked to stop it, and each end goes the way every other
        // end does, so one the database does not take now is tried again.
        let mut ended: Vec<(String, u64, &'static str)> = self
            .turns
            .working_turns
            .drain()
            .map(|(thread_id, active)| {
                let state = if active.stop_asked.is_some() {
                    "stopped"
                } else {
                    "interrupted"
                };
                (thread_id, active.turn, state)
            })
            .collect();
        ended.sort();
        for (thread_id, turn, state) in ended {
            self.end_turn(&thread_id, turn, state, None, log);
            broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: json!({
                    "event": state,
                    "thread": thread_id,
                    "turn": turn,
                }),
            });
        }
        Effects { broadcasts, replies }
    }

    fn fail_pending_peer_requests(&mut self, message: &str) -> Vec<PeerReply> {
        let mut replies = Vec::new();
        self.turns.fail_pending_deletes(message, &mut replies);
        self.catalog.fail_pending(message, &mut replies);
        self.discovery.fail_pending(message, &mut replies);
        replies
    }

    fn refresh_idle_timer(&mut self) {
        if self.harness.is_some() && !self.has_active_work() {
            if self.idle_since.is_none() {
                self.idle_since = Some(Instant::now());
            }
        } else {
            self.idle_since = None;
        }
    }
}
