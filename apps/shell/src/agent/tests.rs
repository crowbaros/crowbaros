// SPDX-License-Identifier: MIT
use super::state::*;
use super::transport::{
    INIT_DEADLINE, load_saved_thread, read_picks, save_active_thread, start, start_with_options,
    start_with_stream_clone, write_picks,
};
use super::types::*;
use super::wire::{MAX_FRAME, encode_frame, parse_lever_message, parse_turn, read_frame};
use crate::compositor;
use crate::events;
use futures::StreamExt;
use std::{
    io::{self, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

    fn short_temp_dir(prefix: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let dir = root.join(format!("{prefix}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_context() -> DesktopContext {
        DesktopContext {
            workspace: 2,
            active: Some(DesktopWindow {
                id: "0x2".into(),
                app_id: "foot".into(),
                title: "work".into(),
                workspace: 2,
            }),
            locked: false,
            windows: vec![
                DesktopWindow {
                    id: "0x1".into(),
                    app_id: "com.crowbaros.StudioSettings".into(),
                    title: "Settings".into(),
                    workspace: 1,
                },
                DesktopWindow {
                    id: "0x2".into(),
                    app_id: "foot".into(),
                    title: "work".into(),
                    workspace: 2,
                },
            ],
            displays: vec![DesktopDisplay {
                name: "Virtual-1".into(),
                width: 1280,
                height: 960,
                x: 0,
                y: 0,
                scale: 120,
            }],
        }
    }

    fn sample_status(harnesses: Vec<&str>) -> StatusInfo {
        StatusInfo {
            lever_version: "0.1.0".into(),
            harness_started: false,
            harnesses: harnesses.into_iter().map(str::to_string).collect(),
            free_hosted_models: false,
            models_chosen: true,
            default_mode: Some("ask-first".into()),
        }
    }

    #[test]
    fn parses_every_lever_surface_message() {
        // status
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": true,
                "harnesses": [{ "id": "harness-1", "name": "OpenCode" }],
                "free_hosted_models": true,
                "models_chosen": true,
                "default_mode": "full-access"
            })),
            Some(LeverMessage::Status(StatusInfo {
                lever_version: "0.1.0".into(),
                harness_started: true,
                harnesses: vec!["OpenCode".into()],
                free_hosted_models: true,
                models_chosen: true,
                default_mode: Some("full-access".into()),
            }))
        );

        // list
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "list",
                "threads": [{
                    "thread": "t-123",
                    "title": "Check disk",
                    "cwd": "/home/crowbar",
                    "model": "stub/stub-1",
                    "started": 100,
                    "updated": 105,
                    "turns": 2,
                    "state": "done",
                    "working": false,
                    "waiting": true,
                    "mode": "plan"
                }]
            })),
            Some(LeverMessage::List {
                threads: vec![ThreadSummary {
                    archived: false,
                    thread: "t-123".into(),
                    title: "Check disk".into(),
                    cwd: "/home/crowbar".into(),
                    model: Some("stub/stub-1".into()),
                    started: 100,
                    updated: 105,
                    turns: 2,
                    state: "done".into(),
                    working: false,
                    waiting: true,
                    subagents: 0,
                    mode: Some("plan".into()),
                }]
            })
        );

        // resumed (with context, acts, notes, and failed/interrupted/stopped/working states)
        let resumed = parse_lever_message(&serde_json::json!({
            "reply": "resumed",
            "thread": "t-123",
            "title": "Check disk",
            "cwd": "/home/crowbar",
            "model": "stub/stub-1",
            "started": 100,
            "updated": 105,
            "working": false,
            "turns": [
                {
                    "turn": 1,
                    "message": "ls",
                    "context": {
                        "workspace": 1,
                        "active": { "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 },
                        "locked": false,
                        "windows": [{ "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 }],
                        "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                    },
                    "answer": "Cargo.toml",
                    "state": "done",
                    "acts": [{ "act": "act-1", "kind": "command", "title": "Run ls", "state": "done", "detail": "Cargo.toml" }],
                    "notes": [{ "note": "note-1", "text": "Session restarted" }]
                },
                { "turn": 2, "message": "stop me", "answer": "", "state": "stopped", "acts": [], "notes": [] },
                { "turn": 3, "message": "crash", "answer": "", "state": "interrupted", "acts": [], "notes": [] },
                { "turn": 4, "message": "fail", "answer": "", "state": "failed", "error": "boom", "acts": [], "notes": [] }
            ]
        }));
        match resumed {
            Some(LeverMessage::Resumed(r)) => {
                assert_eq!(r.thread, "t-123");
                assert_eq!(r.model.as_deref(), Some("stub/stub-1"));
                assert_eq!(r.turns.len(), 4);
                assert_eq!(r.turns[0].acts.len(), 1);
                assert_eq!(r.turns[0].acts[0].detail.as_deref(), Some("Cargo.toml"));
                assert_eq!(r.turns[0].notes.len(), 1);
                assert!(r.turns[0].context.is_some());
                assert_eq!(r.turns[1].state, TurnState::Stopped);
                assert_eq!(r.turns[2].state, TurnState::Interrupted);
                assert_eq!(r.turns[3].state, TurnState::Failed("boom".into()));
            }
            other => panic!("expected Resumed, got {other:?}"),
        }

        // sent, ok, error
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "sent", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Sent { thread: "t-1".into(), turn: 1 })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "ok" })),
            Some(LeverMessage::Ok)
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "error", "code": "no-model", "message": "No Model is available."
            })),
            Some(LeverMessage::Error {
                code: "no-model".into(),
                message: "No Model is available.".into()
            })
        );

        // live events: message (without and with desktop context), text, act, note, thread-updated, done, stopped, failed, interrupted
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "message", "thread": "t-1", "turn": 1, "text": "hello"
            })),
            Some(LeverMessage::Message {
                thread: "t-1".into(),
                turn: 1,
                text: "hello".into(),
                context: None,
                from_harness: false,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "message",
                "thread": "t-1",
                "turn": 2,
                "text": "what is focused?",
                "context": {
                    "workspace": 2,
                    "active": { "id": "0x2", "app_id": "foot", "title": "work", "workspace": 2 },
                    "locked": false,
                    "windows": [
                        { "id": "0x1", "app_id": "com.crowbaros.StudioSettings", "title": "Settings", "workspace": 1 },
                        { "id": "0x2", "app_id": "foot", "title": "work", "workspace": 2 }
                    ],
                    "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                }
            })),
            Some(LeverMessage::Message {
                thread: "t-1".into(),
                turn: 2,
                text: "what is focused?".into(),
                context: Some(sample_context()),
                from_harness: false,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "text", "thread": "t-1", "turn": 1, "text": "hi"
            })),
            Some(LeverMessage::Text { thread: "t-1".into(), turn: 1, text: "hi".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "act", "thread": "t-1", "turn": 1,
                "act": "act-a", "kind": "edit", "title": "Edit main.rs", "state": "working", "detail": "src/main.rs"
            })),
            Some(LeverMessage::Act {
                thread: "t-1".into(),
                turn: 1,
                act: Act {
                    id: "act-a".into(),
                    kind: "edit".into(),
                    title: "Edit main.rs".into(),
                    state: ActState::Working,
                    detail: Some("src/main.rs".into()),
                }
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "note", "thread": "t-1", "turn": 1, "note": "note-a", "text": "New session"
            })),
            Some(LeverMessage::Note {
                thread: "t-1".into(),
                turn: 1,
                note: Note { id: "note-a".into(), text: "New session".into() }
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "thread-updated", "thread": "t-1", "title": "Inspect workspace"
            })),
            Some(LeverMessage::ThreadUpdated { model: None, effort: None, cwd: None,
                thread: "t-1".into(),
                title: Some("Inspect workspace".into()),
                mode: None,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "done", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Done { thread: "t-1".into(), turn: 1 })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "stopped", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Stopped { thread: "t-1".into(), turn: 1 })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "failed", "thread": "t-1", "turn": 1, "message": "provider failed"
            })),
            Some(LeverMessage::Failed { thread: "t-1".into(), turn: 1, message: "provider failed".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "interrupted", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Interrupted { thread: "t-1".into(), turn: 1 })
        );
    }

    #[test]
    fn turn_streaming_acts_notes_and_mark_transitions_behind_drawer() {
        let mut state = AgentState::new(true);
        assert_eq!(state.mark, Mark::Idle);

        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            true,
        );
        assert!(state.connected);
        assert!(!state.no_harness);
        assert!(state.can_send());

        state.apply(AgentEvent::Sent { thread: "t-abc".into(), turn: 1 }, true);
        assert_eq!(state.mark, Mark::Working);
        state.apply(
            AgentEvent::Message {
                thread: "t-abc".into(),
                turn: 1,
                text: "What is focused?".into(),
                context: Some(sample_context()),
                from_harness: false,
            },
            true,
        );
        // Act transitions pending -> working -> done in place by act ID.
        state.apply(
            AgentEvent::Act {
                thread: "t-abc".into(),
                turn: 1,
                act: Act {
                    id: "act-1".into(),
                    kind: "command".into(),
                    title: "shell".into(),
                    state: ActState::Pending,
                    detail: None,
                },
            },
            true,
        );
        state.apply(
            AgentEvent::Act {
                thread: "t-abc".into(),
                turn: 1,
                act: Act {
                    id: "act-1".into(),
                    kind: "command".into(),
                    title: "id -un".into(),
                    state: ActState::Done,
                    detail: Some("crowbar".into()),
                },
            },
            true,
        );
        state.apply(
            AgentEvent::Note {
                thread: "t-abc".into(),
                turn: 1,
                note: Note { id: "note-1".into(), text: "Warm cache".into() },
            },
            true,
        );
        state.apply(
            AgentEvent::ThreadUpdated { model: None, effort: None, cwd: None,
                thread: "t-abc".into(),
                title: Some("Focus check".into()),
                mode: None,
            },
            true,
        );
        state.apply(
            AgentEvent::Text { thread: "t-abc".into(), turn: 1, text: "The active ".into() },
            true,
        );

        // Putting the drawer away stops nothing and keeps Working.
        assert!(!state.on_drawer_closed());
        assert_eq!(state.mark, Mark::Working);

        // Answer finishes behind the drawer -> mark shows Done.
        state.apply(
            AgentEvent::Text { thread: "t-abc".into(), turn: 1, text: "window is foot.".into() },
            false,
        );
        state.apply(AgentEvent::Done { thread: "t-abc".into(), turn: 1 }, false);
        assert_eq!(state.mark, Mark::Done);
        assert_eq!(state.title.as_deref(), Some("Focus check"));
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].acts.len(), 1);
        assert_eq!(state.turns[0].acts[0].title, "id -un");
        assert_eq!(state.turns[0].acts[0].state, ActState::Done);
        assert_eq!(state.turns[0].acts[0].detail.as_deref(), Some("crowbar"));
        assert_eq!(state.turns[0].notes.len(), 1);
        assert_eq!(state.turns[0].answer, "The active window is foot.");
        assert_eq!(
            state.turns[0]
                .context
                .as_ref()
                .and_then(|c| c.active.as_ref())
                .map(|w| w.app_id.as_str()),
            Some("foot")
        );

        // Summoning the drawer acknowledges Done -> Idle.
        assert!(state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Idle);
    }

    #[test]
    fn fullscreen_mark_overlay_visibility_and_output_rules() {
        let mut snap = compositor::Snapshot {
            workspace: 1,
            connected: true,
            locked: false,
            clients: vec![compositor::Client {
                address: "0x1".into(),
                class: "foot".into(),
                title: "foot".into(),
                workspace: compositor::Workspace { id: 1 },
                attention: false,
                fullscreen: true,
                output: "Virtual-2".into(),
            }],
            ..Default::default()
        };
        assert_eq!(fullscreen_mark_overlay_output(Mark::Idle, &snap, false), None);
        assert_eq!(
            fullscreen_mark_overlay_output(Mark::Working, &snap, false).as_deref(),
            Some("Virtual-2")
        );
        assert_eq!(
            fullscreen_mark_overlay_output(Mark::Done, &snap, false).as_deref(),
            Some("Virtual-2")
        );
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, true), None);
        snap.locked = true;
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, false), None);
        snap.locked = false;
        snap.workspace = 2;
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, false), None);
    }

    #[test]
    fn rapid_send_guard_stopped_interrupted_failed_and_no_thread_recovery() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            true,
        );
        assert!(state.can_send());
        state.sending = true;
        assert!(!state.can_send());
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        assert!(!state.sending);
        assert!(state.can_send(), "while the turn works, Lever takes a message into it or has it wait");

        // Failure while drawer is put away leaves Mark::Done so the person notices.
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "hi".into(), context: None, from_harness: false },
            false,
        );
        state.apply(
            AgentEvent::Failed {
                thread: "t-1".into(),
                turn: 1,
                message: "TypeError: fetch failed (ECONNREFUSED)".into(),
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
        match &state.turns[0].state {
            TurnState::Failed(msg) => assert!(msg.contains("Could not reach the Model"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }

        // Stopped turn transitions mark to Idle; Stop right after Sent (before Message)
        // targets the new turn rather than the previous finished turn.
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 2 }, true);
        assert_eq!(state.current_turn(), Some(2));
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 2, text: "wait".into(), context: None, from_harness: false },
            true,
        );
        state.apply(AgentEvent::Stopped { thread: "t-1".into(), turn: 2 }, true);
        assert_eq!(state.turns[1].state, TurnState::Stopped);
        assert_eq!(state.current_turn(), None);
        assert_eq!(state.mark, Mark::Idle);

        // Resuming a thread with `model: None` clears any previously held model.
        state.model = Some("stub/stub-1".into());
        state.apply(
            AgentEvent::Resumed(ResumedThread { effort: None,
                thread: "t-2".into(),
                title: "Fresh".into(),
                cwd: "/tmp".into(),
                model: None,
                mode: None,
                working: false,
                turns: Vec::new(),
            }),
            true,
        );
        assert_eq!(state.model, None);
        state.thread = Some("t-1".into());
        state.turns.push(Turn { started: None, ended: None, from_harness: false, steered: Vec::new(), queued: Vec::new(),
            turn: 1,
            message: "prev".into(),
            context: None,
            acts: Vec::new(),
            notes: Vec::new(),
            requests: Vec::new(),
            subagents: Vec::new(),
            mode: None,
            answer: "ok".into(),
            state: TurnState::Done,
        });
        state.turns.push(Turn { started: None, ended: None, from_harness: false, steered: Vec::new(), queued: Vec::new(),
            turn: 2,
            message: "stopped".into(),
            context: None,
            acts: Vec::new(),
            notes: Vec::new(),
            requests: Vec::new(),
            subagents: Vec::new(),
            mode: None,
            answer: String::new(),
            state: TurnState::Stopped,
        });

        // Interrupted turn behind drawer shows Done.
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 3 }, false);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 3, text: "mid".into(), context: None, from_harness: false },
            false,
        );
        state.apply(AgentEvent::Interrupted { thread: "t-1".into(), turn: 3 }, false);
        assert_eq!(state.turns[2].state, TurnState::Interrupted);
        assert_eq!(state.mark, Mark::Done);

        // Lost thread clears stale state and unlocks composer.
        state.thread_mark = Mark::Working;
        state.apply(
            AgentEvent::Error {
                code: "no-thread".into(),
                message: "There is no thread t-1.".into(),
            },
            true,
        );
        assert_eq!(state.thread, None);
        assert!(state.turns.is_empty());
        assert_eq!(state.mark, Mark::Idle);
        assert!(state.can_send());

        // Numbers containing 401/403 must not be misclassified as sign-in errors.
        let token_err = "prompt is too long: 204031 tokens > 200000 maximum (140123 input)";
        assert_eq!(calm_turn_error(token_err), token_err);
        assert_eq!(
            calm_turn_error("Internal error: Stub model declined this turn"),
            "Stub model declined this turn"
        );
        assert!(
            !calm_turn_error("Lever is restarting the harness after settings changed")
                .contains("Lever")
        );
        assert!(
            !calm_turn_error("Lever is still saving how this thread's last turn ended.")
                .contains("Lever")
        );
        for auth_err in [
            "HTTP 401 Unauthorized",
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            "API key not valid. Please pass a valid API key.",
        ] {
            assert!(
                calm_turn_error(auth_err).contains("crowbar-agent login"),
                "expected auth copy for {auth_err}"
            );
        }

        // Stopped behind a put-away drawer shows Done, and Disconnected resets
        // both Mark and stale missing_model.
        state.apply(AgentEvent::Sent { thread: "t-9".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message { thread: "t-9".into(), turn: 1, text: "bg".into(), context: None, from_harness: false },
            false,
        );
        state.apply(AgentEvent::Stopped { thread: "t-9".into(), turn: 1 }, false);
        assert_eq!(state.mark, Mark::Done);
        state.missing_model = Some("old".into());
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);
        assert_eq!(state.missing_model, None);
    }

    #[test]
    fn draft_after_send_clears_exact_strips_typed_ahead_and_keeps_edits() {
        assert_eq!(draft_after_send("hello", "hello"), Some(String::new()));
        assert_eq!(
            draft_after_send("hello next question", "hello"),
            Some("next question".into())
        );
        assert_eq!(draft_after_send("completely new draft", "hello"), None);
    }

    #[test]
    fn context_summary_lists_active_windows_and_displays() {
        let lines = format_context_summary(&sample_context());
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Workspace 2"));
        assert!(lines[0].contains("work (foot)"));
        assert!(lines[1].contains("W1: Settings (com.crowbaros.StudioSettings)"));
        assert!(lines[1].contains("W2: work (foot)"));
        assert!(lines[2].contains("Virtual-1 1280x960"));
    }

    #[test]
    fn surface_client_holds_connected_until_initial_status_and_reconnects_on_timeout() {
        let dir = short_temp_dir("cs-lever-deadline");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: handshake succeeds, status sends a 4-byte length header
            // and stalls the payload past the deadline -> client must NOT emit Connected.
            let (mut slow, _) = listener.accept().unwrap();
            let v = read_frame(&mut slow).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            slow.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();
            let req = read_frame(&mut slow).unwrap();
            assert_eq!(req["op"], "status");
            // Send 4-byte header advertising 32 bytes, then stall the body past 180ms.
            slow.write_all(&32u32.to_be_bytes()).unwrap();
            std::thread::sleep(Duration::from_millis(320));
            drop(slow);

            // Attempt 2: answers status + empty list -> client reports Connected with status already populated.
            let (mut ok, _) = listener.accept().unwrap();
            let v = read_frame(&mut ok).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            ok.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();
            let req = read_frame(&mut ok).unwrap();
            assert_eq!(req["op"], "status");
            ok.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "harness-1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            let list_req = read_frame(&mut ok).unwrap();
            assert_eq!(list_req["op"], "list");
            ok.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": []
            })))
            .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let late = tx.clone();
        let test_deadline = Duration::from_secs(10);
        let _sender = start_with_options(sock.clone(), tx, Duration::from_millis(180), None);
        std::thread::spawn(move || {
            std::thread::sleep(test_deadline);
            let _ = late.send(events::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });

        let mut events_seen = Vec::new();
        let mut state = AgentState::new(true);
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let events::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("timed out waiting for Connected after retry");
                    }
                    events_seen.push(agent_ev.clone());
                    state.apply(agent_ev, true);
                    if state.connected {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(events_seen.len(), 1, "expected only Connected after retry: {events_seen:?}");
        assert!(state.connected);
        assert_eq!(state.lever_version.as_deref(), Some("0.1.0"));
        assert_eq!(state.harnesses, vec!["OpenCode".to_string()]);
        assert!(!state.no_harness);
    }

    #[test]
    fn surface_client_resumes_newest_thread_after_restart_and_streams_acts_and_stop() {
        let dir = short_temp_dir("cs-lever-resume");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let v = read_frame(&mut peer).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();

            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": true,
                "harnesses": [{ "id": "harness-1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();

            // Fresh shell asks `list` and prefers a working thread over a newer idle one.
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": [
                    {
                        "thread": "t-idle-newer",
                        "title": "Idle newer",
                        "cwd": "/home/crowbar",
                        "started": 110,
                        "updated": 120,
                        "turns": 1,
                        "state": "done",
                        "working": false
                    },
                    {
                        "thread": "t-existing",
                        "title": "Existing work",
                        "cwd": "/home/crowbar",
                        "model": "stub/stub-1",
                        "started": 100,
                        "updated": 105,
                        "turns": 1,
                        "state": "working",
                        "working": true
                    }
                ]
            })))
            .unwrap();

            let resume_req = read_frame(&mut peer).unwrap();
            assert_eq!(resume_req, serde_json::json!({ "op": "resume", "thread": "t-existing" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "resumed",
                "thread": "t-existing",
                "title": "Existing work",
                "cwd": "/home/crowbar",
                "model": "stub/stub-1",
                "started": 100,
                "updated": 105,
                "working": false,
                "turns": [{
                    "turn": 1,
                    "message": "check whoami",
                    "context": {
                        "workspace": 1,
                        "active": { "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 },
                        "locked": false,
                        "windows": [{ "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 }],
                        "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                    },
                    "answer": "crowbar",
                    "state": "done",
                    "acts": [{ "act": "act-0", "kind": "command", "title": "id -un", "state": "done", "detail": "crowbar" }],
                    "notes": []
                }]
            })))
            .unwrap();

            // After restoring the thread, the client asks to watch every
            // thread's activity for the mark.
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "watch" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "activity", "working": [], "waiting": []
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"list","watch":true}));
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"list","threads":[]}))).unwrap();

            // Client sends turn 2 and then stops it.
            let send_req = read_frame(&mut peer).unwrap();
            assert_eq!(send_req["op"], "send");
            assert_eq!(send_req["thread"], "t-existing");
            assert_eq!(send_req["text"], "long command");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "sent", "thread": "t-existing", "turn": 2
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "message", "thread": "t-existing", "turn": 2, "text": "long command"
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "act", "thread": "t-existing", "turn": 2,
                "act": "act-2", "kind": "command", "title": "sleep 30", "state": "working"
            })))
            .unwrap();

            let stop_req = read_frame(&mut peer).unwrap();
            assert_eq!(stop_req, serde_json::json!({ "op": "stop", "thread": "t-existing", "turn": 2 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "ok" })))
                .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "stopped", "thread": "t-existing", "turn": 2
            })))
            .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let late = tx.clone();
        let test_deadline = Duration::from_secs(10);
        let sender = start(sock.clone(), tx);
        std::thread::spawn(move || {
            std::thread::sleep(test_deadline);
            let _ = late.send(events::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });

        let mut state = AgentState::new(true);
        let mut sent_turn2 = false;
        let mut stopped_turn2 = false;
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let events::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("timed out waiting for resume/send/stop sequence");
                    }
                    state.apply(agent_ev, true);
                    if state.connected && !sent_turn2 {
                        assert_eq!(state.thread.as_deref(), Some("t-existing"));
                        assert_eq!(state.turns.len(), 1);
                        assert_eq!(state.turns[0].acts[0].detail.as_deref(), Some("crowbar"));
                        sent_turn2 = true;
                        sender.send_message(state.thread.clone(), "long command".into(), None);
                    }
                    if state.turns.len() == 2 && !state.turns[1].acts.is_empty() && !stopped_turn2 {
                        stopped_turn2 = true;
                        sender.stop_turn("t-existing".into(), Some(2));
                    }
                    if state.turns.len() == 2 && state.turns[1].state == TurnState::Stopped {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.turns.len(), 2);
        assert_eq!(state.turns[1].state, TurnState::Stopped);
        assert_eq!(state.mark, Mark::Idle);
    }

    #[test]
    fn surface_client_connects_to_lever_and_reports_no_harness() {
        let dir = short_temp_dir("cs-lever-noharness");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let deadline = Duration::from_secs(10);
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(deadline)).unwrap();
            peer.set_write_timeout(Some(deadline)).unwrap();
            let v = read_frame(&mut peer).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();

            let status_req = read_frame(&mut peer).unwrap();
            assert_eq!(status_req["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [],
                "free_hosted_models": false
            })))
            .unwrap();

            let list_req = read_frame(&mut peer).unwrap();
            assert_eq!(list_req["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": []
            })))
            .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let late = tx.clone();
        let _sender = start(sock.clone(), tx);
        std::thread::spawn(move || {
            std::thread::sleep(deadline);
            let _ = late.send(events::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });

        let mut state = AgentState::new(true);
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let events::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("the shell did not report Lever's status in {deadline:?}");
                    }
                    state.apply(agent_ev, true);
                    if state.connected && state.lever_version.is_some() {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert_eq!(state.lever_version.as_deref(), Some("0.1.0"));
        assert!(!state.harness_started);
        assert!(state.no_harness);
        assert!(!state.can_send());
        assert_eq!(state.mark, Mark::Idle);
    }

    fn run_with_deadline<F>(
        rx: &mut futures::channel::mpsc::UnboundedReceiver<events::Event>,
        tx: &events::Sender,
        deadline: Duration,
        mut on_event: F,
    ) where
        F: FnMut(AgentEvent) -> bool,
    {
        let late = tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(deadline);
            let _ = late.send(events::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let events::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("test timed out after {deadline:?}");
                    }
                    if on_event(agent_ev) {
                        break;
                    }
                }
            }
        });
    }

    #[test]
    fn surface_client_stays_connected_when_resume_returns_storage_error() {
        let dir = short_temp_dir("cs-lever-resume-err");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": [{
                    "thread": "t-corrupt",
                    "title": "Corrupt",
                    "cwd": "/tmp",
                    "started": 1,
                    "updated": 2,
                    "turns": 1,
                    "state": "done",
                    "working": false
                }]
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "resume");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "error",
                "code": "storage-error",
                "message": "database is locked"
            })))
            .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let _sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert!(state.can_send());
        assert_eq!(state.thread, None);
        assert!(
            state
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Could not load the previous thread"),
            "expected calm storage-error copy, got {:?}",
            state.error
        );
    }

    #[test]
    fn surface_client_recovers_when_resume_frame_exceeds_max_frame() {
        let dir = short_temp_dir("cs-lever-oversize");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                if attempt == 0 {
                    assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
                    peer.write_all(&encode_frame(&serde_json::json!({
                        "reply": "list",
                        "threads": [{
                            "thread": "t-huge",
                            "title": "Huge",
                            "cwd": "/tmp",
                            "started": 1,
                            "updated": 2,
                            "turns": 500,
                            "state": "done",
                            "working": false
                        }]
                    })))
                    .unwrap();
                    assert_eq!(read_frame(&mut peer).unwrap()["op"], "resume");
                    // Advertise a frame larger than MAX_FRAME (64 MiB + 1).
                    peer.write_all(&((MAX_FRAME as u32) + 1).to_be_bytes()).unwrap();
                }
            }
        });

        let (tx, mut rx) = events::channel();
        let _sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert!(state.can_send());
        assert_eq!(state.thread, None);
        assert!(
            state.error.as_deref().unwrap_or("").contains("too large"),
            "expected oversized history warning, got {:?}",
            state.error
        );
    }

    #[test]
    fn surface_client_restores_persisted_thread_over_newer_working_thread() {
        let dir = short_temp_dir("cs-lever-persist");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": true,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            // Client must resume its persisted `t-drawer` directly without falling back to `list`.
            let req = read_frame(&mut peer).unwrap();
            assert_eq!(req, serde_json::json!({ "op": "resume", "thread": "t-drawer" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "resumed",
                "thread": "t-drawer",
                "title": "Drawer thread",
                "cwd": "/tmp",
                "started": 10,
                "updated": 11,
                "working": false,
                "turns": [{
                    "turn": 1,
                    "message": "from drawer",
                    "answer": "drawer answer",
                    "state": "done",
                    "acts": [],
                    "notes": []
                }]
            })))
            .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let _sender = start_with_options(sock.clone(), tx.clone(), INIT_DEADLINE, Some(state_path));
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.thread.as_deref(), Some("t-drawer"));
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].answer, "drawer answer");
    }

    #[test]
    fn surface_client_reports_error_when_stop_sent_while_disconnected() {
        let dir = short_temp_dir("cs-lever-stop-offline");
        let missing = dir.join("missing");
        let (tx, mut rx) = events::channel();
        let sender = start(missing, tx.clone());
        sender.stop_turn("t-1".into(), Some(1));

        let mut got = None;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            if let AgentEvent::Error { code, message } = ev {
                got = Some((code, message));
                return true;
            }
            false
        });
        let _ = std::fs::remove_dir_all(&dir);
        let (code, msg) = got.expect("expected disconnected error for Stop");
        assert_eq!(code, "disconnected");
        assert!(msg.contains("Stop"), "{msg}");
    }

    #[test]
    fn surface_client_enforces_monotonic_deadline_on_fragmented_status_reply() {
        let dir = short_temp_dir("cs-lever-drip");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: drip 1 byte every 25ms past the 120ms deadline.
            let (mut slow, _) = listener.accept().unwrap();
            let _ = read_frame(&mut slow);
            let _ = slow.write_all(&encode_frame(&serde_json::json!({ "version": 1 })));
            let _ = read_frame(&mut slow);
            let frame = encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            }));
            for byte in &frame {
                if slow.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            drop(slow);

            // Attempt 2: reply immediately.
            let (mut ok, _) = listener.accept().unwrap();
            let _ = read_frame(&mut ok);
            ok.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            let _ = read_frame(&mut ok);
            ok.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            let _ = read_frame(&mut ok);
            ok.write_all(&encode_frame(&serde_json::json!({ "reply": "list", "threads": [] })))
                .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let _sender = start_with_options(sock.clone(), tx.clone(), Duration::from_millis(120), None);
        let mut connected_count = 0usize;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            if matches!(ev, AgentEvent::Connected { .. }) {
                connected_count += 1;
                return true;
            }
            false
        });
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(connected_count, 1);
    }

    #[test]
    fn surface_client_preserves_saved_thread_across_storage_error_and_retries_it() {
        let dir = short_temp_dir("cs-lever-keep-tid");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: saved thread returns transient storage-error, then disconnects.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-drawer" })
                );
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "error",
                    "code": "storage-error",
                    "message": "database is locked"
                })))
                .unwrap();
                // Wait briefly so client emits Connected before disconnect.
                std::thread::sleep(Duration::from_millis(80));
            }
            // Attempt 2: client must retry `t-drawer` (never falling back to `list`).
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-drawer" })
                );
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "resumed",
                    "thread": "t-drawer",
                    "title": "Drawer",
                    "cwd": "/tmp",
                    "started": 1,
                    "updated": 2,
                    "working": false,
                    "turns": [{
                        "turn": 1,
                        "message": "hi",
                        "answer": "preserved",
                        "state": "done",
                        "acts": [],
                        "notes": []
                    }]
                })))
                .unwrap();
            }
        });

        let (tx, mut rx) = events::channel();
        let _sender =
            start_with_options(sock.clone(), tx.clone(), INIT_DEADLINE, Some(state_path.clone()));
        let mut state = AgentState::new(true);
        let mut first_connect_checked = false;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            let is_conn = matches!(ev, AgentEvent::Connected { .. });
            state.apply(ev, true);
            if is_conn && !first_connect_checked {
                first_connect_checked = true;
                assert_eq!(state.thread.as_deref(), Some("t-drawer"));
                assert!(state.error.is_some());
                assert_eq!(load_saved_thread(Some(&state_path)).as_deref(), Some("t-drawer"));
                return false;
            }
            is_conn && first_connect_checked && state.turns.len() == 1
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.thread.as_deref(), Some("t-drawer"));
        assert_eq!(state.turns[0].answer, "preserved");
        assert_eq!(state.error, None);
    }

    #[test]
    fn surface_client_recovers_when_resume_times_out_preserving_saved_thread() {
        let dir = short_temp_dir("cs-lever-resume-timeout");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: status succeeds, resume stalls past deadline.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let _ = read_frame(&mut peer);
                let _ = peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })));
                let _ = read_frame(&mut peer);
                let _ = peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })));
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-drawer" })
                );
                std::thread::sleep(Duration::from_millis(260));
            }
            // Attempt 2: client skips resume for this reconnect and connects online with `t-drawer` preserved.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let _ = read_frame(&mut peer);
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                let _ = read_frame(&mut peer);
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
            }
        });

        let (tx, mut rx) = events::channel();
        let _sender = start_with_options(
            sock.clone(),
            tx.clone(),
            Duration::from_millis(140),
            Some(state_path.clone()),
        );
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let saved = load_saved_thread(Some(&state_path));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert_eq!(state.thread.as_deref(), Some("t-drawer"));
        assert_eq!(saved.as_deref(), Some("t-drawer"));
        assert!(
            state.error.as_deref().unwrap_or("").contains("Timed out"),
            "expected timeout warning, got {:?}",
            state.error
        );
    }

    #[test]
    fn turn_finishing_during_disconnect_lights_done_on_reconnect() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        state.apply(AgentEvent::Sent { thread: "t-bg".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message {
                thread: "t-bg".into(),
                turn: 1,
                text: "run".into(),
                context: None,
                from_harness: false,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Working);

        // Lever disconnects while turn is working -> mark clears to Idle while offline.
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);

        // Lever reconnects and resumes the thread now finished -> mark lights Done.
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: Some(ResumedThread { effort: None,
                    thread: "t-bg".into(),
                    title: "bg".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    mode: None,
                    working: false,
                    turns: vec![Turn { started: None, ended: None, from_harness: false, steered: Vec::new(), queued: Vec::new(),
                        turn: 1,
                        message: "run".into(),
                        context: None,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: Vec::new(),
                        subagents: Vec::new(),
                        mode: None,
                        answer: "finished".into(),
                        state: TurnState::Done,
                    }],
                }),
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
    }

    #[test]
    fn terminal_turn_never_displays_unfinished_act_as_working() {
        let working_act = Act {
            id: "a1".into(),
            kind: "command".into(),
            title: "sleep 10".into(),
            state: ActState::Working,
            detail: None,
        };
        let pending_act = Act {
            id: "a2".into(),
            kind: "command".into(),
            title: "ls".into(),
            state: ActState::Pending,
            detail: None,
        };
        let stopped_err_act = Act {
            id: "a3".into(),
            kind: "command".into(),
            title: "sleep 45".into(),
            state: ActState::Failed,
            detail: Some("Command interrupted by the user.".into()),
        };
        assert_eq!(
            act_display_status(&TurnState::Working, &working_act),
            ActDisplay { label: "Working…", tone: ActTone::Active }
        );
        assert_eq!(
            act_display_status(&TurnState::Stopped, &working_act),
            ActDisplay { label: "Stopped", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Stopped, &pending_act),
            ActDisplay { label: "Stopped", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Stopped, &stopped_err_act),
            ActDisplay { label: "Stopped", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Interrupted, &working_act),
            ActDisplay { label: "Interrupted", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Interrupted, &pending_act),
            ActDisplay { label: "Interrupted", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Failed("err".into()), &working_act),
            ActDisplay { label: "Interrupted", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Done, &working_act),
            ActDisplay { label: "Done", tone: ActTone::Muted }
        );
    }

    #[test]
    fn preserved_thread_reconnect_unwedges_working_turn_and_keeps_done_memory() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message {
                thread: "t-1".into(),
                turn: 1,
                text: "slow work".into(),
                context: None,
                from_harness: false,
            },
            false,
        );
        assert!(state.working());
        assert!(state.can_send(), "while the turn works, Lever takes a message into it or has it wait");

        // Disconnect while turn 1 is working.
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);

        // Reconnect hits transient storage-error on `resume("t-1")`.
        // Preserves `t-1`, unwedges stale Working turn so `can_send()` is true,
        // and retains working memory for a subsequent resume.
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: Some("t-1".into()),
                resume_error: Some("Could not load the previous thread right now.".into()),
            },
            false,
        );
        assert_eq!(state.thread.as_deref(), Some("t-1"));
        assert!(!state.working());
        assert!(state.can_send());
        assert_eq!(state.turns[0].state, TurnState::Interrupted);

        // Later reconnect succeeds with turn 1 Done -> mark still lights Done!
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: Some(ResumedThread { effort: None,
                    thread: "t-1".into(),
                    title: "slow".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    mode: None,
                    working: false,
                    turns: vec![Turn { started: None, ended: None, from_harness: false, steered: Vec::new(), queued: Vec::new(),
                        turn: 1,
                        message: "slow work".into(),
                        context: None,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: Vec::new(),
                        subagents: Vec::new(),
                        mode: None,
                        answer: "all done".into(),
                        state: TurnState::Done,
                    }],
                }),
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
        assert_eq!(state.turns[0].state, TurnState::Done);
    }

    #[test]
    fn missing_saved_thread_starts_fresh_without_adopting_other_thread() {
        let dir = short_temp_dir("cs-lever-missing-saved");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-deleted"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: saved thread is gone (`no-thread`), then socket drops.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-deleted" })
                );
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "error",
                    "code": "no-thread",
                    "message": "There is no thread t-deleted."
                })))
                .unwrap();
                std::thread::sleep(Duration::from_millis(80));
            }
            // Attempt 2: after reconnect, client STILL must NOT ask `list` to
            // adopt an unrelated thread before the person sends a message.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
            }
        });

        let (tx, mut rx) = events::channel();
        let _sender =
            start_with_options(sock.clone(), tx.clone(), INIT_DEADLINE, Some(state_path.clone()));
        let mut state = AgentState::new(true);
        let mut connect_count = 0usize;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            let is_conn = matches!(ev, AgentEvent::Connected { .. });
            state.apply(ev, true);
            if is_conn {
                connect_count += 1;
                assert_eq!(state.thread, None);
            }
            connect_count >= 2
        });

        server.join().unwrap();
        let saved = load_saved_thread(Some(&state_path));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert!(state.can_send());
        assert_eq!(state.thread, None);
        assert_eq!(saved, None);
    }

    fn sample_request(id: &str, state: RequestState) -> Request {
        Request {
            id: id.into(),
            act: Some("act-1".into()),
            subagent: None,
            kind: "command".into(),
            title: "echo hi".into(),
            detail: "echo hi".into(),
            path: None,
            answers: vec![
                RequestAnswer { answer: "once".into(), name: "Allow once".into() },
                RequestAnswer { answer: "always".into(), name: "Always allow".into() },
                RequestAnswer { answer: "no".into(), name: "Reject".into() },
            ],
            state,
            answer: None,
            sending: None,
        }
    }

    fn connected_state(drawer_up: bool) -> AgentState {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            drawer_up,
        );
        state
    }

    fn activity(working: &[&str], waiting: &[&str]) -> AgentEvent {
        AgentEvent::Activity {
            working: working.iter().map(|s| s.to_string()).collect(),
            waiting: waiting.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parses_requests_modes_and_activity() {
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "request", "thread": "t-1", "turn": 2, "request": "r-1", "act": "act-1",
                "kind": "edit", "title": "Edit notes.txt", "detail": "-old\n+new", "path": "/home/crowbar/notes.txt",
                "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }],
                "state": "waiting"
            })),
            Some(LeverMessage::Request {
                thread: "t-1".into(),
                turn: 2,
                request: Request {
                    id: "r-1".into(),
                    act: Some("act-1".into()),
                    subagent: None,
                    kind: "edit".into(),
                    title: "Edit notes.txt".into(),
                    detail: "-old\n+new".into(),
                    path: Some("/home/crowbar/notes.txt".into()),
                    answers: vec![
                        RequestAnswer { answer: "once".into(), name: "Allow once".into() },
                        RequestAnswer { answer: "no".into(), name: "Reject".into() },
                    ],
                    state: RequestState::Waiting,
                    answer: None,
                    sending: None,
                },
            })
        );
        match parse_lever_message(&serde_json::json!({
            "event": "request", "thread": "t-1", "turn": 2, "request": "r-1", "kind": "command",
            "title": "ls", "detail": "ls", "answers": [], "state": "answered", "answer": "always"
        })) {
            Some(LeverMessage::Request { request, .. }) => {
                assert_eq!(request.state, RequestState::Answered);
                assert_eq!(request.answer.as_deref(), Some("always"));
            }
            other => panic!("expected Request, got {other:?}"),
        }
        match parse_lever_message(&serde_json::json!({
            "event": "request", "thread": "t-1", "turn": 2, "request": "r-1", "kind": "command",
            "title": "ls", "detail": "ls", "answers": [], "state": "withdrawn"
        })) {
            Some(LeverMessage::Request { request, .. }) => {
                assert_eq!(request.state, RequestState::Withdrawn)
            }
            other => panic!("expected Request, got {other:?}"),
        }

        // A resumed thread carries its mode, and each turn its mode and requests.
        match parse_lever_message(&serde_json::json!({
            "reply": "resumed", "thread": "t-1", "title": "", "cwd": "/tmp", "mode": "full-access",
            "working": true,
            "turns": [{
                "turn": 1, "message": "hi", "answer": "", "state": "working", "mode": "ask-first",
                "acts": [], "notes": [],
                "requests": [{ "request": "r-9", "kind": "command", "title": "id", "detail": "id",
                               "answers": [{ "answer": "once", "name": "Allow once" }], "state": "waiting" }]
            }, { "turn": 2, "message": "old", "answer": "", "state": "done", "acts": [], "notes": [] }]
        })) {
            Some(LeverMessage::Resumed(r)) => {
                assert_eq!(r.mode.as_deref(), Some("full-access"));
                assert_eq!(r.turns[0].mode.as_deref(), Some("ask-first"));
                assert_eq!(r.turns[0].requests.len(), 1);
                assert_eq!(r.turns[0].requests[0].id, "r-9");
                assert_eq!(r.turns[1].mode, None);
                assert!(r.turns[1].requests.is_empty());
            }
            other => panic!("expected Resumed, got {other:?}"),
        }

        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "thread-updated", "thread": "t-1", "mode": "plan"
            })),
            Some(LeverMessage::ThreadUpdated { model: None, effort: None, cwd: None, thread: "t-1".into(), title: None, mode: Some("plan".into()) })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "models", "supported": true, "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
                "default": "stub/stub-1", "free_hosted_models": false,
                "modes": [{ "id": "ask-first", "name": "Ask first" }, { "id": "full-access", "name": "Full access" },
                          { "id": "plan", "name": "Plan" }]
            })),
            Some(LeverMessage::Models(Offer {
                supported: true,
                models: vec![ModelInfo {
                    id: "stub/stub-1".into(),
                    name: "Stub One".into(),
                    provider: None,
                    provider_name: None,
                }],
                default_model: Some("stub/stub-1".into()),
                free_hosted_models: false,
                steering: false,
                modes: vec![
                    ModeInfo { id: "ask-first".into(), name: "Ask first".into() },
                    ModeInfo { id: "full-access".into(), name: "Full access".into() },
                    ModeInfo { id: "plan".into(), name: "Plan".into() },
                ],
            }))
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "models", "supported": false, "models": [] })),
            Some(LeverMessage::Models(Offer::default()))
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "mode-set", "thread": "t-1", "mode": "full-access" })),
            Some(LeverMessage::ModeSet { thread: "t-1".into(), mode: "full-access".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "default-mode", "mode": "full-access" })),
            Some(LeverMessage::DefaultMode { mode: "full-access".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "activity", "working": ["t-1"], "waiting": [] })),
            Some(LeverMessage::Activity { working: vec!["t-1".into()], waiting: Vec::new() })
        );
        // The `activity` event names no thread of its own.
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "activity", "working": [], "waiting": ["t-2"] })),
            Some(LeverMessage::Activity { working: Vec::new(), waiting: vec!["t-2".into()] })
        );
    }

    #[test]
    fn a_request_after_its_turn_is_answered_and_stop_withdraws_it() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "run".into(), context: None, from_harness: false },
            true,
        );
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert!(!state.can_stop(), "nothing works and nothing waits");

        // The harness asks after its turn ended, as a subagent's would.
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-1", RequestState::Waiting),
            },
            true,
        );
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Waiting
        );
        assert!(state.waiting());
        assert!(state.can_stop(), "Stop withdraws what waits");
        assert_eq!(state.current_turn(), None, "Stop names no turn: none works");
        assert_eq!(state.begin_answer("r-1", "once").as_deref(), Some("t-1"));
    }

    #[test]
    fn a_request_waits_then_is_answered_or_withdrawn() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "run".into(), context: None, from_harness: false },
            true,
        );
        // A request for a thread the drawer does not hold is not drawn here.
        assert!(!state.apply(
            AgentEvent::Request {
                thread: "t-other".into(),
                turn: 1,
                request: sample_request("r-x", RequestState::Waiting),
            },
            true,
        ));
        assert!(state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-1", RequestState::Waiting),
            },
            true,
        ));
        assert!(state.waiting());
        // Before Lever's `activity` arrives, the drawer's own thread shows waiting.
        assert_eq!(state.mark, Mark::Waiting);
        assert_eq!(request_view(&state.turns[0].state, &state.turns[0].requests[0]), RequestView::Waiting);

        // An answer the request does not offer is not sent.
        assert_eq!(state.begin_answer("r-1", "never"), None);
        // Answering once marks it as sending, and a second click sends nothing.
        assert_eq!(state.begin_answer("r-1", "once").as_deref(), Some("t-1"));
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Sending("Allow once".into())
        );
        assert_eq!(state.begin_answer("r-1", "no"), None);
        // Lever did not take it: the person can answer again.
        state.apply(
            AgentEvent::Error { code: "no-request".into(), message: "No such request.".into() },
            true,
        );
        assert_eq!(state.turns[0].requests[0].sending, None);
        assert_eq!(state.error.as_deref(), Some("That request no longer waits for an answer."));
        assert_eq!(state.begin_answer("r-1", "once").as_deref(), Some("t-1"));

        // The same request ID comes again, answered.
        let mut answered = sample_request("r-1", RequestState::Answered);
        answered.answer = Some("once".into());
        state.apply(AgentEvent::Request { thread: "t-1".into(), turn: 1, request: answered }, true);
        assert_eq!(state.turns[0].requests.len(), 1);
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Answered("Allow once".into())
        );
        assert!(!state.waiting());
        assert_eq!(state.mark, Mark::Working);
        assert_eq!(state.begin_answer("r-1", "once"), None);

        // A second request is withdrawn, as Stop withdraws a turn's requests.
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-2", RequestState::Waiting),
            },
            true,
        );
        assert_eq!(state.mark, Mark::Waiting);
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-2", RequestState::Withdrawn),
            },
            true,
        );
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[1]),
            RequestView::Withdrawn
        );
        assert_eq!(state.mark, Mark::Working);

        // One Lever says waits, waits, whether or not its turn still works:
        // a harness may ask after its turn. Only one in a turn the drawer saw
        // interrupted shows no answer controls.
        let waiting = sample_request("r-3", RequestState::Waiting);
        assert_eq!(request_view(&TurnState::Interrupted, &waiting), RequestView::Withdrawn);
        assert_eq!(request_view(&TurnState::Working, &waiting), RequestView::Waiting);
        assert_eq!(request_view(&TurnState::Done, &waiting), RequestView::Waiting);
        assert_eq!(request_view(&TurnState::Stopped, &waiting), RequestView::Waiting);

        // An unnamed answer gets a plain name.
        assert_eq!(RequestAnswer { answer: "no".into(), name: " ".into() }.label(), "Refuse");
    }

    #[test]
    fn a_request_comes_back_with_its_thread_after_a_restart() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: Some(ResumedThread { effort: None,
                    thread: "t-1".into(),
                    title: "Waits".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    mode: Some("ask-first".into()),
                    working: true,
                    turns: vec![Turn { started: None, ended: None, from_harness: false, steered: Vec::new(), queued: Vec::new(),
                        turn: 1,
                        message: "run".into(),
                        context: None,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: vec![sample_request("r-1", RequestState::Waiting)],
                        subagents: Vec::new(),
                        mode: Some("ask-first".into()),
                        answer: String::new(),
                        state: TurnState::Working,
                    }],
                }),
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        assert!(state.waiting());
        assert_eq!(state.mark, Mark::Waiting);
        assert_eq!(state.mode.as_deref(), Some("ask-first"));
        // Summoning the drawer leaves it waiting.
        assert!(!state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Waiting);
        assert!(state.mode_locked());
    }

    #[test]
    fn the_mark_follows_activity_waiting_over_working_over_done() {
        let mut state = connected_state(false);
        assert_eq!(state.mark, Mark::Idle);

        // Lever's `watch` reply: nothing works.
        state.apply(activity(&[], &[]), false);
        assert_eq!(state.mark, Mark::Idle);

        // A thread the drawer does not hold works (as one `crowbar-agent` runs):
        // the mark shows it, and the drawer's own thread can still send.
        assert!(state.apply(activity(&["t-term"], &[]), false));
        assert_eq!(state.mark, Mark::Working);
        assert!(state.can_send());

        // It asks: waiting outranks working.
        state.apply(activity(&["t-term"], &["t-term"]), false);
        assert_eq!(state.mark, Mark::Waiting);
        // The drawer summoned and put away leaves it waiting.
        assert!(!state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Waiting);
        assert!(!state.on_drawer_closed());
        assert_eq!(state.mark, Mark::Waiting);

        // Answered, it works again; then it ends behind the put-away drawer.
        state.apply(activity(&["t-term"], &[]), false);
        assert_eq!(state.mark, Mark::Working);
        state.apply(activity(&[], &[]), false);
        assert_eq!(state.mark, Mark::Done);
        // Summoning the drawer acknowledges it.
        assert!(state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Idle);

        // Work that ends with the drawer up leaves the mark idle.
        state.apply(activity(&["t-term"], &[]), true);
        assert_eq!(state.mark, Mark::Working);
        state.apply(activity(&[], &[]), true);
        assert_eq!(state.mark, Mark::Idle);

        // New work clears a done no one has seen yet.
        state.apply(activity(&["t-a"], &[]), false);
        state.apply(activity(&[], &[]), false);
        assert_eq!(state.mark, Mark::Done);
        state.apply(activity(&["t-b"], &[]), false);
        assert_eq!(state.mark, Mark::Working);

        // Once Lever watches, the drawer's own waiting request does not light the
        // mark by itself: Lever's word for every thread does.
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "run".into(), context: None, from_harness: false },
            false,
        );
        state.apply(activity(&["t-b", "t-1"], &[]), false);
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-1", RequestState::Waiting),
            },
            false,
        );
        assert_eq!(state.mark, Mark::Working);
        state.apply(activity(&["t-b", "t-1"], &["t-1"]), false);
        assert_eq!(state.mark, Mark::Waiting);

        // A lost connection forgets what Lever said until it says it again.
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);
        assert!(state.working_threads.is_empty() && state.waiting_threads.is_empty());
    }

    #[test]
    fn the_mark_overlay_shows_waiting_over_a_fullscreen_window() {
        let snap = compositor::Snapshot {
            workspace: 1,
            connected: true,
            locked: false,
            clients: vec![compositor::Client {
                address: "0x1".into(),
                class: "foot".into(),
                title: "foot".into(),
                workspace: compositor::Workspace { id: 1 },
                attention: false,
                fullscreen: true,
                output: "Virtual-1".into(),
            }],
            ..Default::default()
        };
        assert_eq!(
            fullscreen_mark_overlay_output(Mark::Waiting, &snap, false).as_deref(),
            Some("Virtual-1")
        );
        assert_eq!(fullscreen_mark_overlay_output(Mark::Waiting, &snap, true), None);
        assert_eq!(Mark::Waiting.label(), "waiting");
        assert_eq!(serde_json::to_value(Mark::Waiting).unwrap(), "waiting");
    }

    #[test]
    fn a_thread_keeps_its_mode_and_the_drawer_picks_one() {
        let mut state = connected_state(true);
        assert_eq!(state.default_mode.as_deref(), Some("ask-first"));
        // The drawer's new thread shows the starting mode.
        assert_eq!(state.thread, None);
        assert_eq!(state.shown_mode().as_deref(), Some("ask-first"));

        // Opening the menu asks for the modes once connected.
        assert!(state.ask_offer());
        assert!(state.modes_loading);
        state.apply(
            AgentEvent::Offer(Offer {
                modes: vec![
                    ModeInfo { id: "ask-first".into(), name: "Ask first".into() },
                    ModeInfo { id: "full-access".into(), name: "Full access".into() },
                    ModeInfo { id: "plan".into(), name: "Plan".into() },
                ],
                ..Default::default()
            }),
            true,
        );
        assert!(!state.modes_loading);
        assert_eq!(state.modes.len(), 3);

        // Picked for the new thread, it is held and goes with the first message.
        assert_eq!(state.pick_mode("plan"), ModePick::Pending);
        assert_eq!(state.shown_mode().as_deref(), Some("plan"));
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        assert_eq!(state.mode.as_deref(), Some("plan"));
        assert_eq!(state.pending_mode, None);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "plan it".into(), context: None, from_harness: false },
            true,
        );
        // While the thread works, its mode does not change.
        assert!(state.mode_locked());
        assert_eq!(state.pick_mode("full-access"), ModePick::Refused);
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert_eq!(
            state.pick_mode("full-access"),
            ModePick::SetThread { thread: "t-1".into(), mode: "full-access".into() }
        );
        // Lever's reply, and its `thread-updated`, set the thread's mode.
        state.apply(AgentEvent::ModeSet { thread: "t-1".into(), mode: "full-access".into() }, true);
        assert_eq!(state.mode.as_deref(), Some("full-access"));
        state.apply(
            AgentEvent::ThreadUpdated { model: None, effort: None, cwd: None, thread: "t-1".into(), title: Some("Plan".into()), mode: None },
            true,
        );
        assert_eq!(state.mode.as_deref(), Some("full-access"));
        assert_eq!(state.title.as_deref(), Some("Plan"));
        state.apply(
            AgentEvent::ThreadUpdated { model: None, effort: None, cwd: None, thread: "t-1".into(), title: None, mode: Some("plan".into()) },
            true,
        );
        assert_eq!(state.mode.as_deref(), Some("plan"));
        assert_eq!(state.title.as_deref(), Some("Plan"));
        // Another thread's mode is not this one's.
        state.apply(AgentEvent::ModeSet { thread: "t-2".into(), mode: "ask-first".into() }, true);
        assert_eq!(state.mode.as_deref(), Some("plan"));

        // The mode new threads start in.
        state.apply(AgentEvent::DefaultMode("full-access".into()), true);
        assert_eq!(state.default_mode.as_deref(), Some("full-access"));
        // A status without one keeps it.
        let mut status = sample_status(vec!["OpenCode"]);
        status.default_mode = None;
        state.apply(AgentEvent::Status(status), true);
        assert_eq!(state.default_mode.as_deref(), Some("full-access"));

        // Resuming another thread takes its mode.
        state.apply(
            AgentEvent::Resumed(ResumedThread { effort: None,
                thread: "t-2".into(),
                title: String::new(),
                cwd: "/tmp".into(),
                model: None,
                mode: Some("ask-first".into()),
                working: false,
                turns: Vec::new(),
            }),
            true,
        );
        assert_eq!(state.mode.as_deref(), Some("ask-first"));

        // A new thread with no mode picked starts in the starting mode.
        state.apply(AgentEvent::Error { code: "no-thread".into(), message: String::new() }, true);
        assert_eq!(state.mode, None);
        assert_eq!(state.shown_mode().as_deref(), Some("full-access"));
        state.apply(AgentEvent::Sent { thread: "t-3".into(), turn: 1 }, true);
        assert_eq!(state.mode.as_deref(), Some("full-access"));

        // `models` failing while the menu loads says so in the menu alone.
        assert!(state.ask_offer());
        state.apply(AgentEvent::Error { code: "harness-error".into(), message: "boom".into() }, true);
        assert!(!state.modes_loading);
        assert_eq!(
            state.modes_error.as_deref(),
            Some("Models and modes are not available right now.")
        );
        assert_eq!(state.error, None);

        assert_eq!(mode_name("full-access", &[]), "Full access");
        assert_eq!(mode_name("plan", &state.modes), "Plan");
    }

    #[test]
    fn request_detail_shows_a_command_as_it_is_and_an_edit_as_a_diff() {
        assert_eq!(
            request_detail_lines("command", "rm -rf build\n+x"),
            vec![
                (DetailLine::Plain, "rm -rf build".to_string()),
                (DetailLine::Plain, "+x".to_string()),
            ]
        );
        assert_eq!(
            request_detail_lines("edit", " keep\n-old line\n+new line\n"),
            vec![
                (DetailLine::Plain, " keep".to_string()),
                (DetailLine::Removed, "-old line".to_string()),
                (DetailLine::Added, "+new line".to_string()),
            ]
        );
        // A change Lever cut, as long as Lever makes one, shows whole: what
        // it takes out and what it puts in.
        let mut cut: Vec<String> = (0..200).map(|i| format!("-old {i}")).collect();
        cut.push("… 250 more lines taken out".into());
        cut.extend((0..200).map(|i| format!("+new {i}")));
        cut.push("… 250 more lines put in".into());
        let lines = request_detail_lines("edit", &cut.join("\n"));
        assert_eq!(lines.len(), 402);
        assert_eq!(lines[201], (DetailLine::Added, "+new 0".to_string()));
        assert_eq!(lines[401], (DetailLine::Plain, "… 250 more lines put in".to_string()));
        // Longer detail is cut, saying how much.
        let long: String = (0..450).map(|i| format!("{i}\n")).collect();
        let lines = request_detail_lines("command", &long);
        assert_eq!(lines.len(), 403);
        assert_eq!(lines[402], (DetailLine::Plain, "… 48 more lines".to_string()));
    }

    #[test]
    fn surface_client_watches_answers_and_sets_modes() {
        let dir = short_temp_dir("cs-lever-requests");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status", "version": "0.1.0", "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }], "free_hosted_models": false,
                "default_mode": "ask-first"
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "list", "threads": [] })))
                .unwrap();
            // The shell asks to watch at connect, and never for `models`.
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "watch" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "activity", "working": [], "waiting": []
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"list","watch":true}));
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"list","threads":[]}))).unwrap();

            // The person opens the mode menu.
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "models" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "models", "supported": true, "models": [], "default": null,
                "free_hosted_models": false,
                "modes": [{ "id": "ask-first", "name": "Ask first" }, { "id": "full-access", "name": "Full access" }]
            })))
            .unwrap();
            // A new thread's first message carries the mode picked for it.
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "send", "thread": null, "text": "run it", "mode": "full-access" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "sent", "thread": "t-1", "turn": 1 })))
                .unwrap();
            for frame in [
                serde_json::json!({ "event": "activity", "working": ["t-1"], "waiting": [] }),
                serde_json::json!({ "event": "message", "thread": "t-1", "turn": 1, "text": "run it" }),
                serde_json::json!({
                    "event": "request", "thread": "t-1", "turn": 1, "request": "r-1", "kind": "command",
                    "title": "echo hi", "detail": "echo hi",
                    "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }],
                    "state": "waiting"
                }),
                serde_json::json!({ "event": "activity", "working": ["t-1"], "waiting": ["t-1"] }),
            ] {
                peer.write_all(&encode_frame(&frame)).unwrap();
            }
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "answer", "thread": "t-1", "request": "r-1", "answer": "once" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "ok" }))).unwrap();
            for frame in [
                serde_json::json!({
                    "event": "request", "thread": "t-1", "turn": 1, "request": "r-1", "kind": "command",
                    "title": "echo hi", "detail": "echo hi",
                    "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }],
                    "state": "answered", "answer": "once"
                }),
                serde_json::json!({ "event": "activity", "working": ["t-1"], "waiting": [] }),
                serde_json::json!({ "event": "done", "thread": "t-1", "turn": 1 }),
                serde_json::json!({ "event": "activity", "working": [], "waiting": [] }),
            ] {
                peer.write_all(&encode_frame(&frame)).unwrap();
            }
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "set-mode", "thread": "t-1", "mode": "ask-first" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "mode-set", "thread": "t-1", "mode": "ask-first"
            })))
            .unwrap();
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "set-default-mode", "mode": "full-access" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "default-mode", "mode": "full-access" })))
                .unwrap();
        });

        let (tx, mut rx) = events::channel();
        let sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        let mut step = 0;
        let mut saw_waiting = false;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(10), |ev| {
            // The drawer is put away throughout.
            state.apply(ev, false);
            if step == 0 && state.connected && state.watching {
                step = 1;
                assert!(state.ask_offer());
                sender.dispatch(AgentCommand::FetchModes { refresh: false });
            } else if step == 1 && !state.modes.is_empty() {
                step = 2;
                assert_eq!(state.pick_mode("full-access"), ModePick::Pending);
                state.sending = true;
                sender.send_message(None, "run it".into(), state.pending_mode.clone());
            } else if step == 2 && state.mark == Mark::Waiting {
                step = 3;
                saw_waiting = true;
                let thread = state.begin_answer("r-1", "once").expect("request waits");
                sender.dispatch(AgentCommand::Answer {
                    thread,
                    request: "r-1".into(),
                    answer: "once".into(),
                });
            } else if step == 3 && state.mark == Mark::Done {
                step = 4;
                match state.pick_mode("ask-first") {
                    ModePick::SetThread { thread, mode } => {
                        sender.dispatch(AgentCommand::SetMode { thread, mode });
                    }
                    other => panic!("expected SetThread, got {other:?}"),
                }
            } else if step == 4 && state.mode.as_deref() == Some("ask-first") {
                step = 5;
                sender.dispatch(AgentCommand::SetDefaultMode {
                    mode: "full-access".into(),
                });
            }
            step == 5 && state.default_mode.as_deref() == Some("full-access")
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(saw_waiting);
        assert_eq!(state.thread.as_deref(), Some("t-1"));
        assert_eq!(state.turns[0].requests[0].state, RequestState::Answered);
        assert_eq!(state.turns[0].requests[0].answer.as_deref(), Some("once"));
        assert_eq!(state.mark, Mark::Done);
    }

    #[test]
    fn a_fresh_shell_restores_the_thread_that_waits_for_the_person() {
        let dir = short_temp_dir("cs-lever-waiting-first");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status", "version": "0.1.0", "harness_started": true,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }], "free_hosted_models": false
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": [
                    { "thread": "t-working", "title": "", "cwd": "/tmp", "started": 3, "updated": 4,
                      "turns": 1, "state": "working", "working": true, "waiting": false, "mode": "full-access" },
                    { "thread": "t-waiting", "title": "", "cwd": "/tmp", "started": 1, "updated": 2,
                      "turns": 1, "state": "working", "working": true, "waiting": true, "mode": "ask-first" }
                ]
            })))
            .unwrap();
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "resume", "thread": "t-waiting" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "resumed", "thread": "t-waiting", "title": "", "cwd": "/tmp", "mode": "ask-first",
                "working": true, "turns": []
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "watch" }));
        });
        let (tx, mut rx) = events::channel();
        let _sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, false);
            state.connected
        });
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.thread.as_deref(), Some("t-waiting"));
        assert_eq!(state.mode.as_deref(), Some("ask-first"));
    }
    fn summary(id: &str, cwd: &str, working: bool) -> ThreadSummary {
        ThreadSummary { archived: false, thread: id.into(), title: id.into(), cwd: cwd.into(), model: None, mode: None, waiting: false, subagents: 0,
            started: 1, updated: 2, turns: 1, state: if working { "working" } else { "done" }.into(), working }
    }

    #[test]
    fn all_threads_drive_the_mark_but_only_the_selected_thread_blocks_the_composer() {
        let mut s = AgentState::new(true);
        s.connected = true;
        s.apply(AgentEvent::Threads { threads: vec![summary("terminal", "/tmp", true)], finished: false }, false);
        assert_eq!(s.mark, Mark::Working);
        assert!(s.can_send(), "terminal work must not prevent a second thread");
        assert!(s.select_thread(None));
        s.apply(AgentEvent::Message { thread: "old-subscription".into(), turn: 1, text: "late".into(), context: None, from_harness: false }, true);
        assert!(s.thread.is_none());
        assert!(s.turns.is_empty());
        s.apply(AgentEvent::Threads { threads: vec![summary("terminal", "/tmp", false)], finished: true }, false);
        assert_eq!(s.mark, Mark::Done);
        s.on_drawer_opened();
        assert_eq!(s.mark, Mark::Idle);
        s.apply(AgentEvent::Threads { threads: vec![summary("a", "/tmp", false), summary("b", "/tmp", true)], finished: true }, false);
        assert_eq!(s.mark, Mark::Working, "one completion must not hide another working thread");
    }

    #[test]
    fn list_groups_newest_folders_and_threads_and_hides_archived_work_without_losing_the_mark() {
        let mut s = AgentState::new(true);
        s.connected = true;
        let mut archived = summary("hidden", "/archive", true); archived.archived = true;
        s.apply(AgentEvent::Threads { threads: vec![summary("new", "/b", false), summary("middle", "/a", false), summary("old", "/b", false), archived], finished: false }, true);
        let groups = s.groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "/b");
        assert_eq!(groups[0].1.iter().map(|t| t.thread.as_str()).collect::<Vec<_>>(), vec!["new", "old"]);
        assert_eq!(s.mark, Mark::Working);
        s.on_drawer_closed();
        assert!(s.list_view);
        s.select_thread(Some("new".into()));
        assert!(!s.select_thread(Some("old".into())), "a pending history read must not land in another thread");
        s.on_drawer_closed(); s.on_drawer_opened();
        assert!(!s.list_view);
    }

    #[test]
    fn a_background_archive_does_not_unlock_a_pending_history_read() {
        let mut s = AgentState::new(true);
        s.connected = true;
        s.select_thread(Some("selected".into()));
        s.apply(AgentEvent::Lifecycle { thread: "background".into(), deleted: false, warning: None }, true);
        assert!(s.loading);
        assert!(!s.select_thread(Some("another".into())));
        assert_eq!(s.thread.as_deref(), Some("selected"));
    }

    #[test]
    fn selecting_new_thread_clears_the_previous_threads_work_and_mode() {
        let mut state = connected_state(false);
        state.thread = Some("working".into());
        state.thread_mark = Mark::Working;
        state.active_turn = Some(1);
        state.mode = Some("full-access".into());
        state.pending_mode = Some("plan".into());
        state.default_mode = Some("ask-first".into());
        state.apply(activity(&["working"], &["waiting"]), false);
        assert!(state.select_thread(None));
        assert!(state.can_send());
        assert!(!state.can_stop());
        assert_eq!(state.shown_mode().as_deref(), Some("ask-first"));
        state.apply(AgentEvent::Threads {
            threads: vec![summary("working", "/tmp", true)], finished: false,
        }, false);
        assert_eq!(state.mark, Mark::Waiting, "list snapshots must not hide a permission request");
    }


    fn offer() -> Offer {
        let model = |id: &str, name: &str, provider: &str, provider_name: &str| ModelInfo {
            id: id.into(),
            name: name.into(),
            provider: Some(provider.into()),
            provider_name: Some(provider_name.into()),
        };
        Offer {
            supported: true,
            models: vec![
                model("acme/small", "Small", "acme", "Acme"),
                model("acme/large", "Large", "acme", "Acme"),
                model("other/tiny", "Tiny", "other", "other"),
            ],
            default_model: Some("acme/small".into()),
            free_hosted_models: false,
            steering: false,
            modes: Vec::new(),
        }
    }

    fn levels(ids: &[&str]) -> Vec<EffortInfo> {
        ids.iter()
            .map(|id| EffortInfo { id: (*id).into(), name: id.to_uppercase() })
            .collect()
    }

    #[test]
    fn parses_models_with_their_providers_efforts_and_commands() {
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "models", "supported": true, "default": "acme/small", "modes": [],
                "models": [
                    { "id": "acme/small", "name": "Small", "provider": "acme", "provider_name": "Acme" },
                    { "id": "plain", "name": "Plain" },
                    { "name": "a Model with no ID" }
                ]
            })),
            Some(LeverMessage::Models(Offer {
                supported: true,
                models: vec![
                    ModelInfo {
                        id: "acme/small".into(),
                        name: "Small".into(),
                        provider: Some("acme".into()),
                        provider_name: Some("Acme".into()),
                    },
                    ModelInfo { id: "plain".into(), name: "Plain".into(), provider: None, provider_name: None },
                ],
                default_model: Some("acme/small".into()),
                free_hosted_models: false,
                steering: false,
                modes: Vec::new(),
            }))
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "efforts", "model": "acme/small",
                "efforts": [{ "id": "low", "name": "Low" }, { "id": "default" }]
            })),
            Some(LeverMessage::Efforts {
                model: "acme/small".into(),
                efforts: vec![
                    EffortInfo { id: "low".into(), name: "Low".into() },
                    EffortInfo { id: "default".into(), name: "default".into() },
                ],
            })
        );
        let review = CommandInfo {
            name: "review".into(),
            description: "review changes".into(),
            hint: Some("commit, branch, or pr".into()),
        };
        let listed = serde_json::json!([
            { "name": "review", "description": "review changes", "hint": "commit, branch, or pr" }
        ]);
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "commands", "known": false, "commands": [] })),
            Some(LeverMessage::Commands { thread: None, commands: Vec::new(), known: false })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "commands", "known": true, "thread": "t-1", "commands": listed })),
            Some(LeverMessage::Commands { thread: Some("t-1".into()), commands: vec![review.clone()], known: true })
        );
        // The event, for a thread and for the harness.
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "commands", "thread": "t-1", "commands": listed })),
            Some(LeverMessage::Commands { thread: Some("t-1".into()), commands: vec![review.clone()], known: true })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "commands", "commands": listed })),
            Some(LeverMessage::Commands { thread: None, commands: vec![review], known: true })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "model-set", "thread": "t-1", "model": "acme/large" })),
            Some(LeverMessage::ModelSet { thread: "t-1".into(), model: "acme/large".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "effort-set", "thread": "t-1", "effort": null })),
            Some(LeverMessage::EffortSet { thread: "t-1".into(), effort: None })
        );
        // `thread-updated` says a Model, cwd, and an effort or that there is none.
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "thread-updated", "thread": "t-1", "model": "acme/large" })),
            Some(LeverMessage::ThreadUpdated {
                thread: "t-1".into(), title: None, cwd: None, mode: None, model: Some("acme/large".into()), effort: None,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "thread-updated", "thread": "t-1", "cwd": "/home/crowbar/Projects/demo" })),
            Some(LeverMessage::ThreadUpdated {
                thread: "t-1".into(), title: None, cwd: Some("/home/crowbar/Projects/demo".into()), mode: None, model: None, effort: None,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "project-made", "thread": "t-1", "cwd": "/home/crowbar/Projects/demo", "created": true })),
            Some(LeverMessage::ProjectMade {
                thread: "t-1".into(), cwd: "/home/crowbar/Projects/demo".into(), created: true,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "dirs",
                "path": "/home/crowbar/Code",
                "home": "/home/crowbar",
                "parent": "/home/crowbar",
                "entries": [{ "name": "demo", "path": "/home/crowbar/Code/demo" }]
            })),
            Some(LeverMessage::Dirs(DirListing {
                path: "/home/crowbar/Code".into(),
                home: "/home/crowbar".into(),
                parent: Some("/home/crowbar".into()),
                entries: vec![DirEntry { name: "demo".into(), path: "/home/crowbar/Code/demo".into() }],
            }))
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "thread-updated", "thread": "t-1", "effort": null })),
            Some(LeverMessage::ThreadUpdated { thread: "t-1".into(), title: None, cwd: None, mode: None, model: None, effort: Some(None) })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "thread-updated", "thread": "t-1", "effort": "high" })),
            Some(LeverMessage::ThreadUpdated {
                thread: "t-1".into(), title: None, cwd: None, mode: None, model: None, effort: Some(Some("high".into())),
            })
        );
        // A record says when each turn began and ended.
        let Some(LeverMessage::Resumed(record)) = parse_lever_message(&serde_json::json!({
            "reply": "resumed", "thread": "t-1", "title": "", "cwd": "/", "working": false,
            "model": "acme/small", "effort": "high",
            "turns": [{ "turn": 1, "message": "hi", "answer": "", "state": "done", "started": 100, "ended": 112 }]
        })) else {
            panic!("a record");
        };
        assert_eq!(record.effort.as_deref(), Some("high"));
        assert_eq!((record.turns[0].started, record.turns[0].ended), (Some(100), Some(112)));
        assert_eq!(record.turns[0].elapsed(500), Some(12));
    }

    #[test]
    fn how_long_a_turn_worked_reads_as_seconds_minutes_and_hours() {
        assert_eq!(
            [0, 8, 59, 60, 65, 3599, 3600, 3725, -3].map(format_elapsed),
            ["0s", "8s", "59s", "1m 00s", "1m 05s", "59m 59s", "1h 00m", "1h 02m", "0s"]
        );
        let mut turn = Turn {
            turn: 1,
            message: String::new(),
            context: None,
            acts: Vec::new(),
            notes: Vec::new(),
            requests: Vec::new(),
            subagents: Vec::new(),
            mode: None,
            answer: String::new(),
            state: TurnState::Working,
            started: Some(100),
            ended: None,
            from_harness: false,
            steered: Vec::new(),
            queued: Vec::new(),
        };
        // A working turn has worked until now, and an ended one until it ended.
        assert_eq!(turn.elapsed(130), Some(30));
        turn.state = TurnState::Done;
        assert_eq!(turn.elapsed(130), None);
        turn.ended = Some(104);
        assert_eq!(turn.elapsed(130), Some(4));
        turn.started = None;
        assert_eq!(turn.elapsed(130), None);
    }

    #[test]
    fn the_rail_has_each_provider_once_in_the_harness_s_order() {
        let mut models = offer().models;
        models.push(ModelInfo { id: "plain".into(), name: "Plain".into(), provider: None, provider_name: None });
        let rail: Vec<(String, String)> = providers(&models).into_iter().map(|p| (p.id, p.name)).collect();
        assert_eq!(
            rail,
            [("acme".into(), "Acme".into()), ("other".into(), "other".into()), (String::new(), "Other".into())]
        );
    }

    #[test]
    fn a_new_thread_starts_with_the_model_and_effort_last_picked() {
        let mut state = connected_state(true);
        state.list_view = false;
        // With nothing picked, the drawer shows the harness's own Model once
        // Lever has said it.
        assert_eq!(state.shown_model(), None);
        state.apply(AgentEvent::Offer(offer()), true);
        assert!(state.offer_known);
        assert_eq!(state.shown_model().as_deref(), Some("acme/small"));
        assert_eq!((state.new_thread_model(), state.shown_effort()), (None, None));
        assert_eq!(state.model_label("acme/large"), "Large");
        assert_eq!(state.model_label("gone/model-x"), "model-x");

        // A pick for the thread that has not begun is held, and remembered.
        assert_eq!(state.pick_model("other/tiny"), SettingPick::Pending);
        assert_eq!(state.shown_model().as_deref(), Some("other/tiny"));
        state.apply(AgentEvent::Efforts { model: "other/tiny".into(), efforts: levels(&["low", "high"]) }, true);
        assert_eq!(state.pick_effort("high"), SettingPick::Pending);
        assert_eq!(state.shown_effort().as_deref(), Some("high"));
        assert_eq!(state.effort_label(Some("other/tiny"), "high"), "HIGH");
        assert_eq!(state.effort_label(Some("acme/small"), "xhigh"), "Xhigh");
        assert_eq!(
            (state.last_model.as_deref(), state.last_effort.as_deref()),
            (Some("other/tiny"), Some("high"))
        );

        // The first message begins the thread with them.
        assert_eq!(
            (state.new_thread_model().as_deref(), state.new_thread_effort().as_deref()),
            (Some("other/tiny"), Some("high"))
        );
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        assert_eq!((state.model.as_deref(), state.effort.as_deref()), (Some("other/tiny"), Some("high")));
        assert_eq!((state.pending_model.clone(), state.pending_effort.clone()), (None, None));

        // The next new thread starts with what was last picked.
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "hi".into(), context: None, from_harness: false },
            true,
        );
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert!(state.select_thread(None));
        assert_eq!(
            (state.shown_model().as_deref(), state.shown_effort().as_deref()),
            (Some("other/tiny"), Some("high"))
        );
        // Another Model begins at its own effort.
        assert_eq!(state.pick_model("acme/large"), SettingPick::Pending);
        assert_eq!((state.shown_effort(), state.last_effort.clone()), (None, None));

        // A Model the harness no longer offers is not asked for.
        state.select_thread(None);
        state.last_model = Some("gone/model".into());
        assert_eq!(state.new_thread_model(), None);
        assert_eq!(state.shown_model().as_deref(), Some("acme/small"));
    }

    #[test]
    fn a_thread_shows_its_model_and_effort_as_soon_as_lever_says_them() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Offer(offer()), true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "hi".into(), context: None, from_harness: false },
            true,
        );
        let told = |model: Option<&str>, effort: Option<Option<&str>>| AgentEvent::ThreadUpdated {
            thread: "t-1".into(),
            title: None,
            cwd: None,
            mode: None,
            model: model.map(str::to_string),
            effort: effort.map(|e| e.map(str::to_string)),
        };
        assert!(state.apply(told(Some("acme/large"), None), true));
        assert_eq!(state.shown_model().as_deref(), Some("acme/large"));
        assert!(state.apply(told(None, Some(Some("high"))), true));
        assert_eq!(state.shown_effort().as_deref(), Some("high"));
        assert!(!state.apply(told(Some("acme/large"), Some(Some("high"))), true), "nothing changed");
        assert!(state.apply(told(None, Some(None)), true));
        assert_eq!(state.shown_effort(), None);
        // What is said of another thread is not this one's.
        let other = AgentEvent::ThreadUpdated {
            thread: "t-2".into(), title: None, cwd: None, mode: None, model: Some("other/tiny".into()), effort: None,
        };
        assert!(!state.apply(other, true));

        // ProjectMade and ThreadUpdated(cwd) move the thread's folder cleanly in both active state and list.
        state.threads.push(summary("t-1", "/home/crowbar", false));
        assert!(state.apply(
            AgentEvent::ProjectMade {
                thread: "t-1".into(),
                cwd: "/home/crowbar/Projects/demo".into(),
                created: true,
            },
            true,
        ));
        assert_eq!(state.cwd.as_deref(), Some("/home/crowbar/Projects/demo"));
        assert_eq!(state.threads[0].cwd, "/home/crowbar/Projects/demo");
        assert!(state.apply(
            AgentEvent::ThreadUpdated {
                thread: "t-1".into(),
                title: None,
                cwd: Some("/home/crowbar/Code/demo".into()),
                mode: None,
                model: None,
                effort: None,
            },
            true,
        ));
        assert_eq!(state.cwd.as_deref(), Some("/home/crowbar/Code/demo"));
        assert_eq!(state.threads[0].cwd, "/home/crowbar/Code/demo");

        // While the thread works its settings rest, and a pick is refused.
        assert_eq!(state.pick_model("other/tiny"), SettingPick::Refused);
        assert_eq!(state.pick_effort("low"), SettingPick::Refused);
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert!(state.turns[0].ended.is_some() && state.turns[0].started.is_some());
        assert_eq!(
            state.pick_model("other/tiny"),
            SettingPick::SetThread { thread: "t-1".into(), value: "other/tiny".into() }
        );
        assert_eq!(state.shown_model().as_deref(), Some("acme/large"), "until Lever says it took it");
        assert!(state.apply(AgentEvent::ModelSet { thread: "t-1".into(), model: "other/tiny".into() }, true));
        assert_eq!(
            state.pick_effort("low"),
            SettingPick::SetThread { thread: "t-1".into(), value: "low".into() }
        );
        assert!(state.apply(AgentEvent::EffortSet { thread: "t-1".into(), effort: Some("low".into()) }, true));
        assert_eq!(state.shown_effort().as_deref(), Some("low"));

        // Lever's refusals are said in the drawer's words.
        state.apply(AgentEvent::Error { code: "invalid-effort".into(), message: "x".into() }, true);
        assert_eq!(state.error.as_deref(), Some("This thread's Model does not offer that effort."));
        state.apply(AgentEvent::Error { code: "invalid-model".into(), message: "x".into() }, true);
        assert_eq!(state.error.as_deref(), Some("The Agent does not offer that Model."));
    }

    #[test]
    fn a_thread_has_the_commands_its_session_named_and_else_the_harness_s() {
        let named = |name: &str| CommandInfo { name: name.into(), description: String::new(), hint: None };
        let mut state = connected_state(true);
        assert!(state.harness_commands().is_empty());
        assert!(!state.apply(
            AgentEvent::Commands { thread: None, commands: Vec::new(), known: false },
            true
        ));
        assert!(state.apply(
            AgentEvent::Commands { thread: None, commands: vec![named("review")], known: true },
            true
        ));
        assert_eq!(state.harness_commands(), [named("review")]);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        // Another thread's commands are not this one's.
        assert!(!state.apply(
            AgentEvent::Commands { thread: Some("t-2".into()), commands: vec![named("deploy")], known: true },
            true
        ));
        assert_eq!(state.harness_commands(), [named("review")]);
        assert!(state.apply(
            AgentEvent::Commands { thread: Some("t-1".into()), commands: vec![named("deploy")], known: true },
            true
        ));
        assert_eq!(state.harness_commands(), [named("deploy")]);
        // A thread left takes its commands with it.
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "hi".into(), context: None, from_harness: false },
            true,
        );
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert!(state.select_thread(None));
        assert_eq!(state.harness_commands(), [named("review")]);
    }

    #[test]
    fn the_surface_client_asks_lever_for_models_efforts_and_commands_and_sets_them() {
        let dir = short_temp_dir("picks");
        let socket = dir.join("surface");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            assert_eq!(read_frame(&mut stream).unwrap(), serde_json::json!({ "version": 1 }));
            stream.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            let mut asked = Vec::new();
            loop {
                let Ok(request) = read_frame(&mut stream) else {
                    return asked;
                };
                let reply = match request["op"].as_str().unwrap() {
                    "status" => serde_json::json!({
                        "reply": "status", "version": "0.1.0", "harness_started": false,
                        "harnesses": [{ "id": "harness-1", "name": "OpenCode" }], "free_hosted_models": false
                    }),
                    "list" => serde_json::json!({ "reply": "list", "threads": [] }),
                    "watch" => serde_json::json!({ "reply": "activity", "working": [], "waiting": [] }),
                    "efforts" => serde_json::json!({ "reply": "efforts", "model": request["model"], "efforts": [] }),
                    "commands" => serde_json::json!({ "reply": "commands", "known": true, "commands": [] }),
                    "set-model" => serde_json::json!({ "reply": "model-set", "thread": "t-1", "model": request["model"] }),
                    "set-effort" => serde_json::json!({ "reply": "effort-set", "thread": "t-1", "effort": request["effort"] }),
                    "send" => {
                        asked.push(request);
                        return asked;
                    }
                    other => panic!("unexpected {other}"),
                };
                if !matches!(request["op"].as_str(), Some("status" | "list" | "watch")) {
                    asked.push(request);
                }
                stream.write_all(&encode_frame(&reply)).unwrap();
            }
        });
        let (tx, mut rx) = events::channel();
        let sender = start_with_options(socket, tx.clone(), Duration::from_secs(5), None);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(10), |event| {
            matches!(event, AgentEvent::Connected { .. })
        });
        sender.dispatch(AgentCommand::FetchEfforts {
            model: "acme/small".into(),
        });
        sender.dispatch(AgentCommand::FetchCommands { thread: None });
        sender.dispatch(AgentCommand::FetchCommands {
            thread: Some("t-1".into()),
        });
        sender.dispatch(AgentCommand::SetModel {
            thread: "t-1".into(),
            model: "acme/large".into(),
        });
        sender.dispatch(AgentCommand::SetEffort {
            thread: "t-1".into(),
            effort: Some("high".into()),
        });
        sender.dispatch(AgentCommand::SetEffort {
            thread: "t-1".into(),
            effort: None,
        });
        sender.dispatch(AgentCommand::Send {
            thread: None,
            text: "hello".into(),
            mode: Some("plan".into()),
            model: Some("acme/large".into()),
            effort: Some("high".into()),
        });
        assert_eq!(
            server.join().unwrap(),
            [
                serde_json::json!({ "op": "efforts", "model": "acme/small" }),
                serde_json::json!({ "op": "commands" }),
                serde_json::json!({ "op": "commands", "thread": "t-1" }),
                serde_json::json!({ "op": "set-model", "thread": "t-1", "model": "acme/large" }),
                serde_json::json!({ "op": "set-effort", "thread": "t-1", "effort": "high" }),
                serde_json::json!({ "op": "set-effort", "thread": "t-1", "effort": null }),
                serde_json::json!({
                    "op": "send", "thread": null, "text": "hello",
                    "mode": "plan", "model": "acme/large", "effort": "high"
                }),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_model_and_effort_last_picked_are_kept_for_the_next_shell() {
        let dir = short_temp_dir("last");
        let path = dir.join("last-picks");
        assert_eq!(read_picks(Some(&path)), (None, None));
        write_picks(Some(&path), Some("acme/large"), Some("high"));
        assert_eq!(read_picks(Some(&path)), (Some("acme/large".into()), Some("high".into())));
        write_picks(Some(&path), Some("acme/small"), None);
        assert_eq!(read_picks(Some(&path)), (Some("acme/small".into()), None));
        // An effort is one Model's: with no Model there is none.
        write_picks(Some(&path), None, Some("high"));
        assert_eq!(read_picks(Some(&path)), (None, None));
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_picks(Some(&path)), (None, None));
        assert_eq!(read_picks(None), (None, None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn threads_snapshot_keeps_optimistic_model_and_status_or_error_bumps_catalog_epoch() {
        let mut state = AgentState::new(true);
        state.connected = true;
        state.last_model = Some("acme/large".into());
        state.apply(
            AgentEvent::Sent {
                thread: "t-new".into(),
                turn: 1,
            },
            true,
        );
        assert_eq!(state.model.as_deref(), Some("acme/large"));

        // A `threads` snapshot arriving before Lever persists the Model must
        // not blank out the optimistic Model.
        state.apply(
            AgentEvent::Threads {
                threads: vec![ThreadSummary {
                    thread: "t-new".into(),
                    title: "Hello".into(),
                    cwd: "/home/person".into(),
                    model: None,
                    mode: Some("ask-first".into()),
                    state: "working".into(),
                    turns: 1,
                    archived: false,
                    working: true,
                    waiting: false,
                    subagents: 0,
                    started: 1,
                    updated: 1,
                }],
                finished: false,
            },
            true,
        );
        assert_eq!(state.model.as_deref(), Some("acme/large"));

        // Toggling free hosted Models invalidates the cached catalog and bumps
        // `catalog_epoch` so the drawer retries lookups.
        state.offer_known = true;
        state.efforts.insert("acme/large".into(), vec![]);
        let before = state.catalog_epoch;
        state.apply(
            AgentEvent::Status(StatusInfo {
                lever_version: "0.1.0".into(),
                harness_started: true,
                harnesses: vec!["OpenCode".into()],
                free_hosted_models: true,
                models_chosen: true,
                default_mode: Some("ask-first".into()),
            }),
            true,
        );
        assert!(!state.offer_known);
        assert!(state.efforts.is_empty());
        assert_ne!(state.catalog_epoch, before);

        // An error while loading the offer does NOT bump `catalog_epoch`
        // (preventing tight retry loops) and sets a neutral error note.
        let before_err = state.catalog_epoch;
        assert!(state.ask_offer());
        state.apply(
            AgentEvent::Error {
                code: "harness-error".into(),
                message: "boom".into(),
            },
            true,
        );
        assert_eq!(state.catalog_epoch, before_err);
        assert_eq!(
            state.modes_error.as_deref(),
            Some("Models and modes are not available right now.")
        );
    }

    #[test]
    fn working_after_a_transient_resume_failure_resubscribes_without_resending() {
        transient_resume_recovery(false);
    }

    #[test]
    fn a_recovery_reply_does_not_reopen_a_thread_the_person_left() {
        transient_resume_recovery(true);
    }

    fn transient_resume_recovery(leave_thread: bool) {
        let dir = short_temp_dir("cs-recover-busy");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"version":1}));
            peer.write_all(&encode_frame(&serde_json::json!({"version":1}))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"status","version":"0.1.0",
                "harness_started":true,"harnesses":[{"name":"OpenCode"}],"free_hosted_models":false}))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "resume");
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"error","code":"storage-error","message":"busy"}))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "watch");
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"activity","working":["t-drawer"],"waiting":[]}))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"list","threads":[]}))).unwrap();
            let request = read_frame(&mut peer).unwrap();
            assert_eq!(request, serde_json::json!({"op":"send","thread":"t-drawer","text":"continue"}));
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"error","code":"working","message":"already working"}))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"resume","thread":"t-drawer"}));
            if leave_thread {
                // Status follows Select(None) on the writer queue, so the
                // selection has changed before this delayed reply is sent.
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            }
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"resumed","thread":"t-drawer","title":"kept","cwd":"/tmp","working":true,
                "model":"stub/kept","effort":"high","mode":"ask-first","turns":[{"turn":1,"message":"first","answer":"still ","state":"working","acts":[],"notes":[]}]}))).unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({"event":"text","thread":"t-drawer","turn":1,"text":"here"}))).unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({"event":"done","thread":"t-drawer","turn":1}))).unwrap();
            if leave_thread {
                peer.write_all(&encode_frame(&serde_json::json!({"reply":"status","version":"recovery-finished",
                    "harness_started":true,"harnesses":[],"free_hosted_models":false}))).unwrap();
            }
        });
        let (tx, mut rx) = events::channel();
        let sender = start_with_options(sock, tx.clone(), INIT_DEADLINE, Some(state_path));
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |event| {
            let connected = matches!(event, AgentEvent::Connected { .. });
            let busy = matches!(&event, AgentEvent::Error { code, .. } if code == "working");
            state.apply(event, true);
            if connected { sender.send_message(state.thread.clone(), "continue".into(), None); }
            if busy && leave_thread {
                assert!(state.select_thread(None));
                sender.select(None);
                sender.dispatch(AgentCommand::RefreshStatus);
            }
            if leave_thread { state.lever_version.as_deref() == Some("recovery-finished") }
            else { state.turns.last().is_some_and(|t| t.answer == "still here" && t.state == TurnState::Done) }
        });
        server.join().unwrap();
        if leave_thread {
            assert_eq!(state.thread, None);
            assert!(state.turns.is_empty());
        } else {
            assert_eq!(state.turns.len(), 1, "a refused message is never resent automatically");
            assert_eq!(state.model.as_deref(), Some("stub/kept"));
            assert_eq!(state.effort.as_deref(), Some("high"));
            assert_eq!(state.mode.as_deref(), Some("ask-first"));
            assert_eq!(state.error, None);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_writer_clone_keeps_first_connection_thread_discovery() {
        let dir = short_temp_dir("cs-clone-retry");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"version":1}));
                peer.write_all(&encode_frame(&serde_json::json!({"version":1}))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({"reply":"status","version":"0.1.0",
                    "harness_started":false,"harnesses":[],"free_hosted_models":false}))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"list"}));
                peer.write_all(&encode_frame(&serde_json::json!({"reply":"list","threads":[{
                    "thread":"kept","title":"Kept","cwd":"/tmp","started":1,"updated":2,
                    "turns":1,"state":"done","working":false}]}))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"resume","thread":"kept"}));
                peer.write_all(&encode_frame(&serde_json::json!({"reply":"resumed","thread":"kept",
                    "cwd":"/tmp","working":false,"turns":[]}))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "watch");
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"list","watch":true}));
            }
        });
        let (tx, mut rx) = events::channel();
        let mut fail_once = true;
        let _sender = start_with_stream_clone(sock, tx.clone(), INIT_DEADLINE, None, move |stream| {
            if std::mem::take(&mut fail_once) { Err(io::Error::from_raw_os_error(24)) }
            else { stream.try_clone() }
        });
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |event| {
            matches!(event, AgentEvent::Connected { resumed: Some(r), .. } if r.thread == "kept")
        });
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn queued_recovery_keeps_the_new_composer_selected() {
        let mut state = connected_state(false);
        state.thread = Some("old".into());
        assert!(state.select_thread(None));
        assert!(!state.apply(AgentEvent::Recovered(ResumedThread {
            thread: "old".into(), title: "Old".into(), cwd: "/tmp".into(),
            model: None, effort: None, mode: None, working: true, turns: vec![],
        }), true));
        assert_eq!(state.thread, None);
        assert!(state.can_send());
    }

    #[test]
    fn first_summon_choice_blocks_sending_until_free_models_or_sign_in_catalog_arrives() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: StatusInfo {
                    lever_version: "0.1.0".into(),
                    harness_started: false,
                    harnesses: vec!["OpenCode".into()],
                    free_hosted_models: false,
                    models_chosen: false,
                    default_mode: Some("ask-first".into()),
                },
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            true,
        );
        assert!(state.needs_model_choice());
        assert!(!state.can_send());

        // Choosing free hosted Models exits the choice screen into a fresh composer.
        state.apply(
            AgentEvent::FreeModels {
                enabled: true,
                models_chosen: Some(true),
            },
            true,
        );
        assert!(!state.needs_model_choice());
        assert!(state.free_hosted_models);
        assert!(!state.list_view);
        assert!(state.can_send());

        // Turning free hosted Models back off with no provider returns to the choice screen.
        state.apply(
            AgentEvent::FreeModels {
                enabled: false,
                models_chosen: Some(false),
            },
            true,
        );
        assert!(state.needs_model_choice());
        assert!(!state.can_send());

        // A live `models` event from Lever after sign-in exits the choice screen
        // with the new Models immediately, with no restart or reconnect.
        let offer_msg = parse_lever_message(&serde_json::json!({
            "event": "models",
            "supported": true,
            "models": [{ "id": "acme/small", "name": "Small", "provider": "acme", "provider_name": "Acme" }],
            "default": "acme/small",
            "free_hosted_models": false,
            "modes": [{ "id": "ask-first", "name": "Ask first" }]
        }))
        .unwrap();
        state.apply(offer_msg.into_agent_event().unwrap(), true);
        assert!(!state.needs_model_choice());
        assert!(!state.list_view);
        assert!(state.can_send());
        assert_eq!(state.models.len(), 1);
        assert_eq!(state.models[0].id, "acme/small");

        // A later live `models` event after signing in to a second provider
        // replaces the non-empty catalog immediately.
        let second_offer = parse_lever_message(&serde_json::json!({
            "event": "models",
            "supported": true,
            "models": [
                { "id": "acme/small", "name": "Small", "provider": "acme", "provider_name": "Acme" },
                { "id": "other/tiny", "name": "Tiny", "provider": "other", "provider_name": "Other" }
            ],
            "default": "acme/small",
            "free_hosted_models": false,
            "modes": [{ "id": "ask-first", "name": "Ask first" }]
        }))
        .unwrap();
        state.apply(second_offer.into_agent_event().unwrap(), true);
        assert_eq!(state.models.len(), 2);
        assert_eq!(state.models[1].id, "other/tiny");

        // Unreachable Model error wording stays calm.
        assert_eq!(
            calm_turn_error("Unable to connect. Is the computer able to access the url?"),
            "Could not reach the Model right now. Check your connection and try again."
        );

        // An empty catalog with `supported: false` does not count as no Models available.
        let unsupported = parse_lever_message(&serde_json::json!({
            "event": "models",
            "supported": false,
            "models": [],
            "default": null,
            "free_hosted_models": false,
            "modes": []
        }))
        .unwrap();
        state.apply(unsupported.into_agent_event().unwrap(), true);
        assert!(state.offer_known);
        assert!(state.offer_unsupported);
        assert!(!state.no_models_available());
    }

    #[test]
    fn subagent_keeps_thread_working_after_turn_ends_and_request_drives_waiting_mark() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message {
                thread: "t-1".into(),
                turn: 1,
                text: "Check workspace".into(),
                context: None,
                from_harness: false,
            },
            false,
        );
        // Subagent starts during turn 1.
        let sub_start = parse_lever_message(&serde_json::json!({
            "event": "subagent",
            "thread": "t-1",
            "turn": 1,
            "subagent": "sub-1",
            "title": "Inspect secrets",
            "state": "working",
            "started": 100
        }))
        .unwrap();
        state.apply(sub_start.into_agent_event().unwrap(), false);
        // Parent turn ends while the subagent is still working.
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, false);
        assert!(state.working(), "thread stays working while subagent works");
        assert!(state.subagent_working());
        assert!(state.can_stop());
        assert!(state.can_send(), "a message is sent while a subagent works");
        assert!(!state.mode_locked(), "and the mode may change");
        assert_eq!(state.mark, Mark::Working);

        // Subagent streams prompt, act, and raises a request after parent turn ended.
        let sub_prompt = parse_lever_message(&serde_json::json!({
            "event": "subagent-text",
            "thread": "t-1",
            "turn": 1,
            "subagent": "sub-1",
            "role": "prompt",
            "text": "Read .env"
        }))
        .unwrap();
        state.apply(sub_prompt.into_agent_event().unwrap(), false);
        let sub_act = parse_lever_message(&serde_json::json!({
            "event": "subagent-act",
            "thread": "t-1",
            "turn": 1,
            "subagent": "sub-1",
            "act": "act-sub-1",
            "kind": "read",
            "title": "Read .env",
            "state": "working"
        }))
        .unwrap();
        state.apply(sub_act.into_agent_event().unwrap(), false);
        let sub_req = parse_lever_message(&serde_json::json!({
            "event": "request",
            "thread": "t-1",
            "turn": 1,
            "request": "r-sub-1",
            "act": "act-sub-1",
            "subagent": "sub-1",
            "kind": "read",
            "title": "Read .env",
            "path": "/tmp/.env",
            "answers": [
                { "answer": "once", "name": "Allow once" },
                { "answer": "no", "name": "Reject" }
            ],
            "state": "waiting"
        }))
        .unwrap();
        state.apply(sub_req.into_agent_event().unwrap(), false);
        assert!(state.waiting());
        assert_eq!(state.mark, Mark::Waiting);
        assert_eq!(state.turns[0].requests[0].subagent.as_deref(), Some("sub-1"));
        assert_eq!(state.begin_answer("r-sub-1", "once").as_deref(), Some("t-1"));

        // Request answered, subagent streams answer and finishes behind put-away drawer.
        let req_done = parse_lever_message(&serde_json::json!({
            "event": "request",
            "thread": "t-1",
            "turn": 1,
            "request": "r-sub-1",
            "act": "act-sub-1",
            "subagent": "sub-1",
            "kind": "read",
            "title": "Read .env",
            "path": "/tmp/.env",
            "answers": [
                { "answer": "once", "name": "Allow once" },
                { "answer": "no", "name": "Reject" }
            ],
            "state": "answered",
            "answer": "once"
        }))
        .unwrap();
        state.apply(req_done.into_agent_event().unwrap(), false);
        assert_eq!(state.mark, Mark::Working);

        for chunk in ["All ", "clear."] {
            let txt = parse_lever_message(&serde_json::json!({
                "event": "subagent-text",
                "thread": "t-1",
                "turn": 1,
                "subagent": "sub-1",
                "role": "answer",
                "text": chunk
            }))
            .unwrap();
            state.apply(txt.into_agent_event().unwrap(), false);
        }
        let sub_done = parse_lever_message(&serde_json::json!({
            "event": "subagent",
            "thread": "t-1",
            "turn": 1,
            "subagent": "sub-1",
            "title": "Inspect secrets",
            "state": "done",
            "started": 100,
            "ended": 105
        }))
        .unwrap();
        state.apply(sub_done.into_agent_event().unwrap(), false);
        assert!(!state.working());
        assert!(!state.subagent_working());
        assert_eq!(state.mark, Mark::Done);
        let sub = &state.turns[0].subagents[0];
        assert_eq!(sub.title, "Inspect secrets");
        assert_eq!(sub.state, SubagentState::Done);
        assert_eq!(sub.messages.len(), 2);
        assert_eq!(sub.messages[0].text, "Read .env");
        assert_eq!(sub.messages[1].text, "All clear.");
        assert_eq!(sub.acts.len(), 1);
    }

    fn lever_event(value: serde_json::Value) -> AgentEvent {
        parse_lever_message(&value).unwrap().into_agent_event().unwrap()
    }

    #[test]
    fn a_turn_the_harness_begins_shows_as_its_own_reopens_and_takes_a_message() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(lever_event(serde_json::json!({ "event": "message", "thread": "t-1", "turn": 1, "text": "Look" })), true);
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);

        // The harness goes on by itself after a subagent reports.
        state.apply(
            lever_event(serde_json::json!({ "event": "message", "thread": "t-1", "turn": 2, "text": "", "origin": "harness" })),
            true,
        );
        assert!(state.turns[1].from_harness);
        assert_eq!(state.turns[1].state, TurnState::Working);
        assert!(state.working(), "the thread shows working");
        assert!(state.can_send(), "the person's message takes the turn over");
        assert!(state.mode_locked(), "Lever changes no setting while the harness's turn works");

        // Lever ends it when quiet, and the harness going on reopens it.
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 2 }, true);
        state.apply(
            lever_event(serde_json::json!({ "event": "message", "thread": "t-1", "turn": 2, "text": "", "origin": "harness" })),
            true,
        );
        assert_eq!(state.turns.len(), 2);
        assert_eq!(state.turns[1].state, TurnState::Working);

        // A turn read from Lever's record keeps where it came from.
        let rec = parse_turn(&serde_json::json!({
            "turn": 2, "message": "", "answer": "Reported.", "state": "done", "origin": "harness"
        }))
        .unwrap();
        assert!(rec.from_harness);
    }

    #[test]
    fn a_reconnect_that_cannot_resume_leaves_no_cached_subagent_working_or_its_request_waiting() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, false);
        state.apply(lever_event(serde_json::json!({ "event": "message", "thread": "t-1", "turn": 1, "text": "Go" })), false);
        state.apply(
            lever_event(serde_json::json!({
                "event": "subagent", "thread": "t-1", "turn": 1, "subagent": "sub-1",
                "title": "Dig", "state": "working", "started": 100
            })),
            false,
        );
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, false);
        state.apply(
            lever_event(serde_json::json!({
                "event": "request", "thread": "t-1", "turn": 1, "request": "r-1", "subagent": "sub-1",
                "kind": "read", "title": "Read .env", "path": "/tmp/.env",
                "answers": [{ "answer": "once", "name": "Allow once" }], "state": "waiting"
            })),
            false,
        );
        assert!(state.subagent_working());

        state.apply(AgentEvent::Disconnected, false);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: Some("t-1".into()),
                resume_error: Some("Could not load the previous thread right now.".into()),
            },
            false,
        );
        assert!(!state.subagent_working());
        assert!(!state.working());
        assert_eq!(state.turns[0].subagents[0].state, SubagentState::Interrupted);
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Withdrawn,
            "no answer controls for a request nobody here follows"
        );
    }

    #[test]
    fn a_message_sent_while_the_turn_works_is_taken_into_it_or_waits_behind_it() {
        let mut state = connected_state(true);
        state.apply(lever_event(serde_json::json!({
            "reply": "models", "supported": true, "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default": "stub/stub-1", "free_hosted_models": false, "modes": [], "steering": false
        })), true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(lever_event(serde_json::json!({ "event": "message", "thread": "t-1", "turn": 1, "text": "Look" })), true);
        state.apply(lever_event(serde_json::json!({ "event": "text", "thread": "t-1", "turn": 1, "text": "Looking. " })), true);
        assert!(state.can_send(), "a message sent while the turn works goes to Lever, whatever the harness");

        // One the harness cannot take waits behind the turn, with a way back.
        state.sending = true;
        state.apply(lever_event(serde_json::json!({ "reply": "queued", "thread": "t-1", "turn": 1, "queued": "q-1" })), true);
        assert!(!state.sending, "the reply ends the sending");
        state.apply(lever_event(serde_json::json!({
            "event": "queued", "thread": "t-1", "turn": 1, "queued": "q-1", "text": "Then this", "state": "queued"
        })), true);
        assert_eq!(state.turns.len(), 1, "a message that waits is no turn yet");
        assert_eq!(
            state.turns[0].queued,
            vec![Queued { id: "q-1".into(), text: "Then this".into(), state: "queued".into() }]
        );
        state.apply(lever_event(serde_json::json!({
            "event": "queued", "thread": "t-1", "turn": 1, "queued": "q-1", "text": "Then this", "state": "removed"
        })), true);
        assert!(state.turns[0].queued.is_empty(), "one taken back is gone");

        // A harness that can steer takes one into the turn.
        state.apply(lever_event(serde_json::json!({
            "reply": "models", "supported": true, "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default": "stub/stub-1", "free_hosted_models": false, "modes": [], "steering": true
        })), true);
        assert!(state.steering);
        state.sending = true;
        state.apply(lever_event(serde_json::json!({ "reply": "steered", "thread": "t-1", "turn": 1 })), true);
        assert!(!state.sending, "the reply ends the sending");
        state.apply(
            lever_event(serde_json::json!({ "event": "steered", "thread": "t-1", "turn": 1, "text": "Also the docs" })),
            true,
        );
        state.apply(lever_event(serde_json::json!({ "event": "text", "thread": "t-1", "turn": 1, "text": "Took it." })), true);
        assert_eq!(state.turns.len(), 1, "the message is the turn's, not a turn of its own");
        assert_eq!(state.turns[0].answer, "Looking. Took it.");
        assert_eq!(
            state.turns[0].steered,
            vec![Steer { text: "Also the docs".into(), at: 9, context: None }],
            "it shows where the answer had come to when it was taken"
        );

        // The record keeps each where it was taken.
        let rec = parse_turn(&serde_json::json!({
            "turn": 1, "message": "Look", "answer": "Looking. Took it.", "state": "stopped",
            "steered": [{ "text": "Also the docs", "at": 9 }],
            "queued": [{ "queued": "q-2", "text": "And that", "state": "dropped" }]
        }))
        .unwrap();
        assert_eq!(rec.steered, vec![Steer { text: "Also the docs".into(), at: 9, context: None }]);
        assert_eq!(rec.queued, vec![Queued { id: "q-2".into(), text: "And that".into(), state: "dropped".into() }]);
        assert_eq!(
            rec.answer_parts(),
            vec![
                AnswerPart::Text("Looking. "),
                AnswerPart::Steered(&rec.steered[0]),
                AnswerPart::Text("Took it."),
            ]
        );
        // A place the answer does not have shows the message after what it has.
        let mut odd = rec.clone();
        odd.steered[0].at = 400;
        assert_eq!(
            odd.answer_parts(),
            vec![AnswerPart::Text("Looking. Took it."), AnswerPart::Steered(&odd.steered[0])]
        );
    }
