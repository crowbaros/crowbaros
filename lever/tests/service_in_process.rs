//! Fast in-process `Service` integration tests exercising Lever's core logic
//! directly against `FakeHarness` and real SQLite (`Store`) in milliseconds.

mod common;

use common::TempDir;
use common::fake_harness::{FakeHarnessHandle, FakeSessionDiscoverer, RecordedCall};
use crowbar_lever::discovery::SessionDiscoverer;
use crowbar_lever::harness::{
    EffortOption, EffortSelector, ExternalSession, HarnessCapabilities, HarnessCommand, HarnessConfig, HarnessDialect,
    HarnessEvent, ImportedTurn, ModelCatalog, ModelOption, SessionHistory, TurnOutcome,
};
use crowbar_lever::log::Log;
use crowbar_lever::service::{
    DEFAULT_DISCOVERY_DEBOUNCE, DEFAULT_IDLE_TIMEOUT, DEFAULT_STOP_TIMEOUT, DEFAULT_WAKE_QUIET, Service, unix_now,
};
use crowbar_lever::store::Store;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

fn fake_config() -> HarnessConfig {
    HarnessConfig {
        id: "fake".into(),
        name: "Fake".into(),
        program: "fake-harness".into(),
        args: Vec::new(),
        env: Vec::new(),
        watch_paths: Vec::new(),
        dialect: HarnessDialect::GenericAcp,
    }
}

fn full_capabilities() -> HarnessCapabilities {
    HarnessCapabilities {
        supports_modes: true,
        supports_steering: true,
        supports_list_sessions: true,
        supports_load_session: true,
        supports_delete_session: true,
        holds_prompt_during_subagent_request: true,
    }
}

fn build_service(
    dir: &TempDir,
    caps: HarnessCapabilities,
    discoverer: Option<Box<dyn SessionDiscoverer>>,
) -> (Service, FakeHarnessHandle, Log, PathBuf) {
    let db_path = dir.0.join("lever.db");
    let log_path = dir.0.join("lever.log");
    let tool_sock = dir.0.join("tool.sock");
    let store = Store::open(&db_path, unix_now()).expect("open real sqlite store");
    let log = Log::open(log_path).expect("open log");
    let handle = FakeHarnessHandle::new(caps);
    let svc = Service::new(
        store,
        Some(fake_config()),
        Box::new(handle.factory()),
        discoverer,
        None,
        tool_sock,
        DEFAULT_IDLE_TIMEOUT,
        DEFAULT_STOP_TIMEOUT,
        DEFAULT_WAKE_QUIET,
        DEFAULT_DISCOVERY_DEBOUNCE,
    );
    (svc, handle, log, db_path)
}

fn drive_init_and_session(
    svc: &mut Service,
    handle: &FakeHarnessHandle,
    log: &mut Log,
    thread_id: &str,
    session_id: &str,
    mode: &str,
) {
    handle.set_session_mode(session_id, mode);
    handle.push_event(HarnessEvent::Initialized);
    svc.on_harness_readable(log);
    handle.push_event(HarnessEvent::SessionReady {
        thread_id: thread_id.into(),
        session_id: session_id.into(),
        afresh: false,
        catalog: None,
        current_model: None,
    });
    svc.on_harness_readable(log);
}

#[test]
fn in_process_turn_lifecycle_persists_to_sqlite_and_streams_events() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    let out = svc.handle_request(
        1,
        true,
        false,
        &json!({ "op": "send", "text": "hello world" }),
        &mut log,
    );
    let reply = out.reply.unwrap();
    assert_eq!(reply["reply"], "sent");
    let thread_id = reply["thread"].as_str().unwrap().to_string();
    assert_eq!(reply["turn"], 1);
    assert_eq!(handle.spawns(), 1);

    drive_init_and_session(&mut svc, &handle, &mut log, &thread_id, "s-1", "ask-first");

    let calls = handle.take_calls();
    assert!(calls.iter().any(|c| matches!(
        c,
        RecordedCall::SendPrompt { thread_id: tid, turn: 1, session_id, text, .. }
            if tid == &thread_id && session_id == "s-1" && text == "hello world"
    )));

    handle.push_events([
        HarnessEvent::TextChunk {
            thread_id: thread_id.clone(),
            text: "hi ".into(),
        },
        HarnessEvent::Act {
            thread_id: thread_id.clone(),
            harness_call_id: "call-1".into(),
            is_new: true,
            kind: Some("read".into()),
            title: Some("Read README".into()),
            state: Some("done".into()),
            detail: None,
        },
        HarnessEvent::TextChunk {
            thread_id: thread_id.clone(),
            text: "there".into(),
        },
        HarnessEvent::PromptFinished {
            thread_id: thread_id.clone(),
            turn: 1,
            outcome: TurnOutcome::Done,
        },
    ]);
    let effects = svc.on_harness_readable(&mut log);
    assert!(effects.broadcasts.iter().any(|b| b.message["event"] == "done"));

    let resumed = svc
        .handle_request(
            2,
            true,
            false,
            &json!({ "op": "resume", "thread": thread_id }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(resumed["reply"], "resumed");
    assert_eq!(resumed["working"], false);
    assert_eq!(resumed["turns"][0]["answer"], "hi there");
    assert_eq!(resumed["turns"][0]["state"], "done");
    assert_eq!(resumed["turns"][0]["acts"][0]["title"], "Read README");
}

#[test]
fn in_process_steering_accepted_vs_declined_and_fifo_queue_drain() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "first" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");
    handle.take_calls();

    // Steer #0: accepted by harness.
    let steer1 = svc.handle_request(
        2,
        true,
        false,
        &json!({ "op": "send", "thread": tid, "text": "keep it brief" }),
        &mut log,
    );
    assert!(steer1.reply.is_none(), "steering reply waits on harness");
    let steer_id = handle
        .take_calls()
        .into_iter()
        .find_map(|c| match c {
            RecordedCall::Steer { steer, .. } => Some(steer),
            _ => None,
        })
        .expect("harness asked to steer");

    handle.push_event(HarnessEvent::SteerAnswered {
        steer: steer_id,
        taken: Ok(()),
    });
    let effects = svc.on_harness_readable(&mut log);
    assert!(
        effects
            .replies
            .iter()
            .any(|r| r.peer_id == 2 && r.message["reply"] == "steered")
    );

    // Steer #1: declined by harness -> falls back to ADR 0037 queued message.
    let steer2 = svc.handle_request(
        3,
        true,
        false,
        &json!({ "op": "send", "thread": tid, "text": "second turn queued" }),
        &mut log,
    );
    assert!(steer2.reply.is_none());
    let steer_id2 = handle
        .take_calls()
        .into_iter()
        .find_map(|c| match c {
            RecordedCall::Steer { steer, .. } => Some(steer),
            _ => None,
        })
        .unwrap();
    handle.push_event(HarnessEvent::SteerAnswered {
        steer: steer_id2,
        taken: Err("busy".into()),
    });
    let effects = svc.on_harness_readable(&mut log);
    assert!(
        effects
            .replies
            .iter()
            .any(|r| r.peer_id == 3 && r.message["reply"] == "queued")
    );

    // A third message sent while a message is already queued queues behind it without steering.
    let queued3 = svc
        .handle_request(
            4,
            true,
            false,
            &json!({ "op": "send", "thread": tid, "text": "third turn queued" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(queued3["reply"], "queued");
    let q3_id = queued3["queued"].as_str().unwrap().to_string();
    assert!(
        !handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::Steer { .. }))
    );

    // Unqueue the third message before turn 1 ends.
    let unq = svc
        .handle_request(
            4,
            true,
            false,
            &json!({ "op": "unqueue", "thread": tid, "queued": q3_id }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(unq["reply"], "ok");

    // Finishing turn 1 drains "second turn queued" automatically as turn 2.
    handle.push_event(HarnessEvent::PromptFinished {
        thread_id: tid.clone(),
        turn: 1,
        outcome: TurnOutcome::Done,
    });
    svc.on_harness_readable(&mut log);
    let calls = handle.take_calls();
    assert!(calls.iter().any(|c| matches!(
        c,
        RecordedCall::SendPrompt { turn: 2, text, .. } if text == "second turn queued"
    )));
}

#[test]
fn capability_supports_steering_false_queues_immediately_without_calling_steer() {
    let dir = TempDir::new();
    let mut caps = full_capabilities();
    caps.supports_steering = false;
    let (mut svc, handle, mut log, _) = build_service(&dir, caps, None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "first" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");
    handle.take_calls();

    let queued = svc
        .handle_request(
            2,
            true,
            false,
            &json!({ "op": "send", "thread": tid, "text": "next" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(queued["reply"], "queued");
    assert!(
        !handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::Steer { .. })),
        "harness with supports_steering=false must never receive steer()"
    );
}

#[test]
fn capability_holds_prompt_during_subagent_request_controls_subagent_prompt_blocking() {
    // 1. Harness with holds_prompt_during_subagent_request = true (OpenCode 2.4.1):
    //    refuses new send while a subagent permission request waits.
    let dir1 = TempDir::new();
    let mut caps_hold = full_capabilities();
    caps_hold.holds_prompt_during_subagent_request = true;
    let (mut svc1, h1, mut log1, _) = build_service(&dir1, caps_hold, None);

    let tid1 = svc1
        .handle_request(1, true, false, &json!({ "op": "send", "text": "spawn" }), &mut log1)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc1, &h1, &mut log1, &tid1, "s-1", "ask-first");
    h1.push_events([
        HarnessEvent::SubagentUpdated {
            thread_id: tid1.clone(),
            harness_subagent_id: "sub-h-1".into(),
            title: Some("Worker".into()),
            state: Some("working".into()),
            error: None,
            from_launcher: false,
        },
        HarnessEvent::PromptFinished {
            thread_id: tid1.clone(),
            turn: 1,
            outcome: TurnOutcome::Done,
        },
        HarnessEvent::PermissionRequested {
            thread_id: tid1.clone(),
            harness_subagent_id: Some("sub-h-1".into()),
            subagent_title: Some("Worker".into()),
            handle: 10,
            harness_call_id: "call-sub".into(),
            kind: Some("command".into()),
            title: "Run cmd".into(),
            detail: None,
            path: None,
            answers: vec![("once".into(), "Allow once".into())],
        },
    ]);
    svc1.on_harness_readable(&mut log1);
    let refused = svc1
        .handle_request(
            2,
            true,
            false,
            &json!({ "op": "send", "thread": tid1, "text": "hello" }),
            &mut log1,
        )
        .reply
        .unwrap();
    assert_eq!(refused["reply"], "error");
    assert_eq!(refused["code"], "subagent-asks");

    // 2. Harness with holds_prompt_during_subagent_request = false (Codex / Claude Code):
    //    allows a new prompt to proceed even while a subagent permission request waits.
    let dir2 = TempDir::new();
    let mut caps_free = full_capabilities();
    caps_free.holds_prompt_during_subagent_request = false;
    let (mut svc2, h2, mut log2, _) = build_service(&dir2, caps_free, None);

    let tid2 = svc2
        .handle_request(1, true, false, &json!({ "op": "send", "text": "spawn" }), &mut log2)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc2, &h2, &mut log2, &tid2, "s-2", "ask-first");
    h2.push_events([
        HarnessEvent::SubagentUpdated {
            thread_id: tid2.clone(),
            harness_subagent_id: "sub-h-2".into(),
            title: Some("Worker".into()),
            state: Some("working".into()),
            error: None,
            from_launcher: false,
        },
        HarnessEvent::PromptFinished {
            thread_id: tid2.clone(),
            turn: 1,
            outcome: TurnOutcome::Done,
        },
        HarnessEvent::PermissionRequested {
            thread_id: tid2.clone(),
            harness_subagent_id: Some("sub-h-2".into()),
            subagent_title: Some("Worker".into()),
            handle: 20,
            harness_call_id: "call-sub".into(),
            kind: Some("command".into()),
            title: "Run cmd".into(),
            detail: None,
            path: None,
            answers: vec![("once".into(), "Allow once".into())],
        },
    ]);
    svc2.on_harness_readable(&mut log2);
    h2.take_calls();

    let allowed = svc2
        .handle_request(
            2,
            true,
            false,
            &json!({ "op": "send", "thread": tid2, "text": "proceed alongside subagent" }),
            &mut log2,
        )
        .reply
        .unwrap();
    assert_eq!(allowed["reply"], "sent");
    assert_eq!(allowed["turn"], 2);
    assert!(h2.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::SendPrompt { turn: 2, text, .. } if text == "proceed alongside subagent"
    )));
}

#[test]
fn in_process_permission_answer_and_withdraw_lifecycle() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "run" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");

    handle.push_event(HarnessEvent::PermissionRequested {
        thread_id: tid.clone(),
        harness_subagent_id: None,
        subagent_title: None,
        handle: 42,
        harness_call_id: "call-perm".into(),
        kind: Some("command".into()),
        title: "Run tests".into(),
        detail: Some("cargo test".into()),
        path: None,
        answers: vec![("once".into(), "Allow once".into()), ("no".into(), "Deny".into())],
    });
    let effects = svc.on_harness_readable(&mut log);
    let req_id = effects
        .broadcasts
        .iter()
        .find_map(|b| (b.message["event"] == "request").then(|| b.message["request"].as_str().unwrap().to_string()))
        .expect("request event broadcast");

    // Activity reports waiting thread.
    let watch = svc
        .handle_request(2, true, false, &json!({ "op": "watch" }), &mut log)
        .reply
        .unwrap();
    assert_eq!(watch["waiting"], json!([tid]));

    // Invalid answer is rejected cleanly.
    let bad = svc
        .handle_request(
            1,
            true,
            false,
            &json!({ "op": "answer", "thread": tid, "request": req_id, "answer": "bogus" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(bad["code"], "invalid-answer");

    // Valid answer forwards to harness and clears waiting.
    let ok = svc
        .handle_request(
            1,
            true,
            false,
            &json!({ "op": "answer", "thread": tid, "request": req_id, "answer": "once" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(ok["reply"], "ok");
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::AnswerPermission { handle: 42, answer: Some(a) } if a == "once"
    )));
}

#[test]
fn in_process_models_efforts_modes_and_commands_catalog_flow() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    // Ask for models -> spawns harness and probes.
    let pending = svc.handle_request(1, true, false, &json!({ "op": "models" }), &mut log);
    assert!(pending.reply.is_none());
    handle.push_event(HarnessEvent::Initialized);
    svc.on_harness_readable(&mut log);
    assert!(
        handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::ProbeModels { .. }))
    );

    let catalog = ModelCatalog {
        supported: true,
        models: vec![
            ModelOption {
                id: "acme/fast".into(),
                name: "Fast".into(),
                provider: Some(("acme".into(), "Acme".into())),
            },
            ModelOption {
                id: "acme/deep".into(),
                name: "Deep".into(),
                provider: Some(("acme".into(), "Acme".into())),
            },
        ],
        default_model: Some("acme/fast".into()),
        modes: vec!["ask-first".into(), "full-access".into(), "plan".into()],
    };
    handle.push_events([
        HarnessEvent::Commands {
            thread_id: None,
            commands: vec![HarnessCommand {
                name: "compact".into(),
                description: "Compact thread".into(),
                hint: None,
            }],
        },
        HarnessEvent::ModelsProbed(Ok(catalog)),
    ]);
    let effects = svc.on_harness_readable(&mut log);
    let reply = effects.replies.iter().find(|r| r.peer_id == 1).unwrap();
    assert_eq!(reply.message["reply"], "models");
    assert_eq!(reply.message["models"].as_array().unwrap().len(), 2);
    assert_eq!(reply.message["steering"], true);

    // Cached commands return immediately.
    let cmds = svc
        .handle_request(2, true, false, &json!({ "op": "commands" }), &mut log)
        .reply
        .unwrap();
    assert_eq!(cmds["known"], true);
    assert_eq!(cmds["commands"][0]["name"], "compact");

    // Efforts probe + cache.
    let eff_req = svc.handle_request(
        3,
        true,
        false,
        &json!({ "op": "efforts", "model": "acme/deep" }),
        &mut log,
    );
    assert!(eff_req.reply.is_none());
    handle.push_event(HarnessEvent::EffortsProbed {
        model: "acme/deep".into(),
        result: Ok(vec![
            EffortOption {
                id: "low".into(),
                name: "Low".into(),
            },
            EffortOption {
                id: "high".into(),
                name: "High".into(),
            },
        ]),
    });
    let eff_effects = svc.on_harness_readable(&mut log);
    let eff_reply = eff_effects.replies.iter().find(|r| r.peer_id == 3).unwrap();
    assert_eq!(eff_reply.message["efforts"][1]["id"], "high");

    // Start thread with model + effort + mode.
    let tid = svc
        .handle_request(
            4,
            true,
            false,
            &json!({ "op": "send", "text": "think", "model": "acme/deep", "effort": "high", "mode": "plan" }),
            &mut log,
        )
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    handle.set_session_effort(
        "s-deep",
        EffortSelector {
            current: "low".into(),
            options: vec![
                EffortOption {
                    id: "low".into(),
                    name: "Low".into(),
                },
                EffortOption {
                    id: "high".into(),
                    name: "High".into(),
                },
            ],
        },
    );
    handle.set_session_mode("s-deep", "ask-first");
    handle.push_event(HarnessEvent::SessionReady {
        thread_id: tid.clone(),
        session_id: "s-deep".into(),
        afresh: false,
        catalog: None,
        current_model: Some("acme/deep".into()),
    });
    svc.on_harness_readable(&mut log);
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::SetSessionEffort { effort, .. } if effort == "high"
    )));
    handle.push_event(HarnessEvent::EffortSet {
        thread_id: tid.clone(),
        turn: 1,
    });
    svc.on_harness_readable(&mut log);
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::SetSessionMode { mode, .. } if mode == "plan"
    )));
    handle.push_event(HarnessEvent::ModeSet {
        thread_id: tid.clone(),
        turn: Some(1),
    });
    svc.on_harness_readable(&mut log);
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::SendPrompt { turn: 1, text, .. } if text == "think"
    )));
}

#[test]
fn in_process_write_behind_journal_recovers_turn_ending_after_sqlite_failure() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, db_path) = build_service(&dir, full_capabilities(), None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "work" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");

    // Block UPDATE on turns via a real SQLite trigger.
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TRIGGER deny_turn_update BEFORE UPDATE ON turns BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();

    handle.push_event(HarnessEvent::PromptFinished {
        thread_id: tid.clone(),
        turn: 1,
        outcome: TurnOutcome::Done,
    });
    svc.on_harness_readable(&mut log);

    // Even while SQLite failed to persist the ending, `list` and `resume` overlay
    // the journal so peers see `working: false, state: "done"`.
    let listed = svc
        .handle_request(2, true, false, &json!({ "op": "list" }), &mut log)
        .reply
        .unwrap();
    assert_eq!(listed["threads"][0]["working"], false);
    assert_eq!(listed["threads"][0]["state"], "done");

    // Poll timeout is bounded by the write-behind retry interval.
    let timeout = svc.poll_timeout_ms();
    assert!(timeout > 0 && timeout <= 250, "expected retry timeout, got {timeout}");

    // Drop the trigger and wake -> journal flushes to SQLite.
    db.execute_batch("DROP TRIGGER deny_turn_update;").unwrap();
    svc.on_wake(&mut log);

    let db_state: String = db
        .query_row(
            "SELECT state FROM turns WHERE thread_id = ?1 AND turn_number = 1",
            [&tid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(db_state, "done");
}

#[test]
fn in_process_project_move_delivers_thread_history_handoff_and_detaches_old_session() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    let tid = svc
        .handle_request(
            1,
            true,
            false,
            &json!({ "op": "send", "text": "plan our app", "cwd": dir.0.to_str().unwrap() }),
            &mut log,
        )
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-old", "ask-first");
    handle.push_events([
        HarnessEvent::TextChunk {
            thread_id: tid.clone(),
            text: "Let's build it.".into(),
        },
        HarnessEvent::PromptFinished {
            thread_id: tid.clone(),
            turn: 1,
            outcome: TurnOutcome::Done,
        },
    ]);
    svc.on_harness_readable(&mut log);
    handle.take_calls();

    // Move the thread to a new project folder.
    let project_path = dir.0.join("my-app");
    let moved = svc
        .handle_request(
            1,
            true,
            false,
            &json!({ "op": "make-project", "thread": tid, "path": project_path.to_str().unwrap() }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(moved["reply"], "project-made");
    assert_eq!(moved["created"], true);
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::DetachSession { thread_id, .. } if thread_id == &tid
    )));

    // Next turn starts a fresh session in the project folder and carries `<thread-history>`.
    let sent2 = svc
        .handle_request(
            1,
            true,
            false,
            &json!({ "op": "send", "thread": tid, "text": "scaffold files" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(sent2["turn"], 2);
    let ensure_calls = handle.take_calls();
    assert!(ensure_calls.iter().any(|c| matches!(
        c,
        RecordedCall::EnsureSession { cwd, existing_session_id: None, .. }
            if cwd == project_path.to_str().unwrap()
    )));

    handle.set_session_mode("s-new", "ask-first");
    handle.push_event(HarnessEvent::SessionReady {
        thread_id: tid.clone(),
        session_id: "s-new".into(),
        afresh: false,
        catalog: None,
        current_model: None,
    });
    svc.on_harness_readable(&mut log);
    let prompt_calls = handle.take_calls();
    let ctx = prompt_calls
        .iter()
        .find_map(|c| match c {
            RecordedCall::SendPrompt {
                turn: 2,
                context: Some(ctx),
                ..
            } => Some(ctx.clone()),
            _ => None,
        })
        .expect("post-move turn must carry <thread-history> handoff context");
    assert!(ctx.contains("<thread-history>"));
    assert!(ctx.contains("plan our app"));
    assert!(ctx.contains("Let's build it."));
}

#[test]
fn in_process_external_session_discovery_and_on_demand_history_import() {
    let dir = TempDir::new();
    let ext = ExternalSession {
        harness_session_id: "ext-42".into(),
        cwd: dir.0.to_str().unwrap().into(),
        title: Some("Terminal chat".into()),
        created: Some(1_700_000_000),
        updated: Some(1_700_000_100),
        is_subagent: false,
        stamp: None,
    };
    let (discoverer, _shared) = FakeSessionDiscoverer::new(vec![ext]);
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), Some(Box::new(discoverer)));

    // Initial wake triggers discovery scan on background thread; wait for result.
    svc.on_wake(&mut log);
    let mut found_thread = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let effects = svc.on_discovery_ready(&mut log);
        if let Some(b) = effects
            .broadcasts
            .iter()
            .find(|b| b.message["event"] == "thread-updated")
        {
            found_thread = Some(b.thread_id.clone());
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let tid = found_thread.expect("discovered thread broadcast");

    // Resuming the unopened thread requests on-demand history load.
    let pending = svc.handle_request(9, true, false, &json!({ "op": "resume", "thread": tid }), &mut log);
    assert!(pending.reply.is_none());

    handle.push_event(HarnessEvent::Initialized);
    svc.on_harness_readable(&mut log);
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::LoadSessionHistory { thread_id, session_id, .. }
            if thread_id == &tid && session_id == "ext-42"
    )));

    handle.push_event(HarnessEvent::HistoryLoaded {
        thread_id: tid.clone(),
        history: SessionHistory {
            turns: vec![ImportedTurn {
                message: "hello from terminal".into(),
                answer: "hello back".into(),
                acts: Vec::new(),
                notes: Vec::new(),
                subagents: Vec::new(),
                origin: None,
            }],
            model: Some("acme/fast".into()),
            effort: None,
            title: Some("Terminal chat".into()),
        },
        catalog: None,
    });
    let effects = svc.on_harness_readable(&mut log);
    let reply = effects.replies.iter().find(|r| r.peer_id == 9).unwrap();
    assert_eq!(reply.message["reply"], "resumed");
    assert_eq!(reply.message["turns"][0]["message"], "hello from terminal");
    assert_eq!(reply.message["turns"][0]["answer"], "hello back");
    assert_eq!(reply.message["model"], "acme/fast");
}

#[test]
fn subagent_waiting_request_queues_active_turn_and_holds_drain_for_every_harness() {
    // ADR 0037: even when a harness can steer and does not strand subagent
    // requests on a fresh prompt (`holds_prompt_during_subagent_request = false`),
    // an outstanding subagent request still queues messages sent during an
    // active turn and holds queue drain until the request is answered.
    let dir = TempDir::new();
    let mut caps = full_capabilities();
    caps.supports_steering = true;
    caps.holds_prompt_during_subagent_request = false;
    let (mut svc, handle, mut log, _) = build_service(&dir, caps, None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "start work" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");
    handle.take_calls();

    // Subagent starts and raises a permission request while Turn 1 is still active.
    handle.push_events([
        HarnessEvent::SubagentUpdated {
            thread_id: tid.clone(),
            harness_subagent_id: "sub-1".into(),
            title: Some("Worker".into()),
            state: Some("working".into()),
            error: None,
            from_launcher: false,
        },
        HarnessEvent::PermissionRequested {
            thread_id: tid.clone(),
            harness_subagent_id: Some("sub-1".into()),
            subagent_title: Some("Worker".into()),
            handle: 77,
            harness_call_id: "call-sub-1".into(),
            kind: Some("command".into()),
            title: "Run check".into(),
            detail: None,
            path: None,
            answers: vec![("once".into(), "Allow once".into())],
        },
    ]);
    let req_effects = svc.on_harness_readable(&mut log);
    let req_id = req_effects
        .broadcasts
        .iter()
        .find_map(|b| (b.message["event"] == "request").then(|| b.message["request"].as_str().unwrap().to_string()))
        .expect("subagent request broadcast");

    // Sending during the active turn must queue (not steer).
    let queued = svc
        .handle_request(
            2,
            true,
            false,
            &json!({ "op": "send", "thread": tid, "text": "follow up" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(queued["reply"], "queued");
    assert!(
        !handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::Steer { .. })),
        "must not steer while a subagent request waits"
    );

    // Finishing Turn 1 while the subagent request still waits must hold the queue.
    handle.push_event(HarnessEvent::PromptFinished {
        thread_id: tid.clone(),
        turn: 1,
        outcome: TurnOutcome::Done,
    });
    svc.on_harness_readable(&mut log);
    assert!(
        !handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::SendPrompt { .. })),
        "queued message must wait while subagent request is unanswered"
    );

    // Answering the subagent request releases the queued message as Turn 2.
    let answered = svc
        .handle_request(
            1,
            true,
            false,
            &json!({ "op": "answer", "thread": tid, "request": req_id, "answer": "once" }),
            &mut log,
        )
        .reply
        .unwrap();
    assert_eq!(answered["reply"], "ok");
    svc.on_wake(&mut log);
    assert!(handle.take_calls().iter().any(|c| matches!(
        c,
        RecordedCall::SendPrompt { turn: 2, text, .. } if text == "follow up"
    )));
}

#[test]
fn in_process_stop_drops_queued_messages_and_overdue_stop_kills_tree() {
    let dir = TempDir::new();
    let db_path = dir.0.join("lever.db");
    let log_path = dir.0.join("lever.log");
    let tool_sock = dir.0.join("tool.sock");
    let store = Store::open(&db_path, unix_now()).unwrap();
    let mut log = Log::open(log_path).unwrap();
    let handle = FakeHarnessHandle::new(full_capabilities());
    // Very short stop timeout so an unanswered Stop becomes overdue quickly.
    let mut svc = Service::new(
        store,
        Some(fake_config()),
        Box::new(handle.factory()),
        None,
        None,
        tool_sock,
        DEFAULT_IDLE_TIMEOUT,
        Duration::from_millis(20),
        DEFAULT_WAKE_QUIET,
        DEFAULT_DISCOVERY_DEBOUNCE,
    );

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "long turn" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");
    handle.take_calls();

    // Queue one message (steer declined -> queued) and send another in-flight steer.
    let _ = svc.handle_request(
        2,
        true,
        false,
        &json!({ "op": "send", "thread": tid, "text": "waiting msg" }),
        &mut log,
    );
    let steer_id = handle
        .take_calls()
        .into_iter()
        .find_map(|c| match c {
            RecordedCall::Steer { steer, .. } => Some(steer),
            _ => None,
        })
        .unwrap();
    handle.push_event(HarnessEvent::SteerAnswered {
        steer: steer_id,
        taken: Err("busy".into()),
    });
    svc.on_harness_readable(&mut log);

    // Stop drops the queued message and asks the harness to cancel.
    let stop_out = svc.handle_request(1, true, false, &json!({ "op": "stop", "thread": tid }), &mut log);
    assert_eq!(stop_out.reply.unwrap()["reply"], "ok");
    assert!(
        stop_out
            .broadcasts
            .iter()
            .any(|b| b.message["event"] == "queued" && b.message["state"] == "dropped")
    );
    assert!(
        handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::CancelSession { session_id } if session_id == "s-1"))
    );

    // When the harness ignores cancel past stop_timeout, on_wake kills the tree.
    std::thread::sleep(Duration::from_millis(35));
    let woke = svc.on_wake(&mut log);
    assert!(woke.broadcasts.iter().any(|b| b.message["event"] == "stopped"));
    assert!(
        handle
            .take_calls()
            .iter()
            .any(|c| matches!(c, RecordedCall::KilledTree))
    );
}

#[test]
fn in_process_unexpected_harness_exit_interrupts_turn_withdraws_request_and_queues_steer() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "work" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    drive_init_and_session(&mut svc, &handle, &mut log, &tid, "s-1", "ask-first");
    handle.take_calls();

    // Open a waiting permission request and an in-flight steer before crash.
    handle.push_event(HarnessEvent::PermissionRequested {
        thread_id: tid.clone(),
        harness_subagent_id: None,
        subagent_title: None,
        handle: 55,
        harness_call_id: "call-crash".into(),
        kind: Some("command".into()),
        title: "Run cmd".into(),
        detail: None,
        path: None,
        answers: vec![("once".into(), "Allow once".into())],
    });
    svc.on_harness_readable(&mut log);

    let steer_out = svc.handle_request(
        2,
        true,
        false,
        &json!({ "op": "send", "thread": tid, "text": "mid-crash steer" }),
        &mut log,
    );
    assert!(steer_out.reply.is_none());

    // Harness crashes unexpectedly.
    handle.push_event(HarnessEvent::Exited);
    let effects = svc.on_harness_readable(&mut log);
    assert!(effects.broadcasts.iter().any(|b| b.message["event"] == "interrupted"));
    assert!(
        effects
            .broadcasts
            .iter()
            .any(|b| b.message["event"] == "request" && b.message["state"] == "withdrawn")
    );
    assert!(
        effects
            .replies
            .iter()
            .any(|r| r.peer_id == 2 && r.message["reply"] == "queued")
    );
}

#[test]
fn in_process_mode_and_model_set_failures_preserve_prior_session_state() {
    let dir = TempDir::new();
    let (mut svc, handle, mut log, _) = build_service(&dir, full_capabilities(), None);

    let tid = svc
        .handle_request(1, true, false, &json!({ "op": "send", "text": "hello" }), &mut log)
        .reply
        .unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    handle.set_session_mode("s-1", "ask-first");
    handle.push_event(HarnessEvent::Initialized);
    svc.on_harness_readable(&mut log);
    handle.push_event(HarnessEvent::SessionReady {
        thread_id: tid.clone(),
        session_id: "s-1".into(),
        afresh: false,
        catalog: Some(ModelCatalog {
            supported: true,
            models: vec![
                ModelOption {
                    id: "acme/a".into(),
                    name: "A".into(),
                    provider: None,
                },
                ModelOption {
                    id: "acme/b".into(),
                    name: "B".into(),
                    provider: None,
                },
            ],
            default_model: Some("acme/a".into()),
            modes: vec!["ask-first".into(), "plan".into()],
        }),
        current_model: Some("acme/a".into()),
    });
    svc.on_harness_readable(&mut log);
    handle.push_event(HarnessEvent::PromptFinished {
        thread_id: tid.clone(),
        turn: 1,
        outcome: TurnOutcome::Done,
    });
    svc.on_harness_readable(&mut log);
    handle.take_calls();

    // Attempt to switch mode to `plan` between turns, then fail the RPC.
    let mode_req = svc.handle_request(
        2,
        true,
        false,
        &json!({ "op": "set-mode", "thread": tid, "mode": "plan" }),
        &mut log,
    );
    assert!(mode_req.reply.is_none());
    // Prior mode must still be `ask-first` before completion.
    assert_eq!(handle.session_current_mode("s-1").as_deref(), Some("ask-first"));

    handle.push_event(HarnessEvent::ModeSetFailed {
        thread_id: tid.clone(),
        turn: None,
        message: "rejected".into(),
    });
    let mode_effects = svc.on_harness_readable(&mut log);
    assert!(
        mode_effects
            .replies
            .iter()
            .any(|r| r.peer_id == 2 && r.message["reply"] == "error")
    );
    assert_eq!(handle.session_current_mode("s-1").as_deref(), Some("ask-first"));

    // Attempt to switch model to `acme/b` between turns, then fail the RPC.
    let model_req = svc.handle_request(
        3,
        true,
        false,
        &json!({ "op": "set-model", "thread": tid, "model": "acme/b" }),
        &mut log,
    );
    assert!(model_req.reply.is_none());
    assert_eq!(handle.session_current_model("s-1").as_deref(), Some("acme/a"));

    handle.push_event(HarnessEvent::ModelSetFailed {
        thread_id: tid.clone(),
        turn: None,
        message: "model unavailable".into(),
    });
    let model_effects = svc.on_harness_readable(&mut log);
    assert!(
        model_effects
            .replies
            .iter()
            .any(|r| r.peer_id == 3 && r.message["reply"] == "error")
    );
    assert_eq!(handle.session_current_model("s-1").as_deref(), Some("acme/a"));
}
