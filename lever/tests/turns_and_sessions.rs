//! Turn streaming, concurrent threads, session recovery, redaction, DB write-failure retry, and README contract.

mod common;

#[allow(unused_imports)]
use common::*;
#[allow(unused_imports)]
use serde_json::{Value, json};
#[allow(unused_imports)]
use std::io::{self, Read, Write};
#[allow(unused_imports)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[allow(unused_imports)]
use std::os::unix::net::UnixStream;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::process::{Child, Command, Stdio};
#[allow(unused_imports)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn thread_streams_answer_and_acts_and_persists_across_restart() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [{
                "steps": [
                    { "kind": "text", "text": "Checking " },
                    { "kind": "act", "id": "acp-call-1", "tool_kind": "execute", "title": "Run ls", "status": "in_progress" },
                    { "kind": "act_update", "id": "acp-call-1", "status": "completed", "content": "Cargo.toml\nsrc" },
                    { "kind": "text", "text": "done." },
                    { "kind": "title", "title": "Inspect workspace" }
                ]
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    // Before any thread runs, harness is configured but not started.
    let mut peer = handshake(&lever.person);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut peer).unwrap();
    assert_eq!(status["reply"], "status");
    assert_eq!(status["harness_started"], false);
    assert_eq!(status["harnesses"][0]["name"], "Stub");

    // Start a thread with a message.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "List the files", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    assert_eq!(sent["reply"], "sent");
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    assert_eq!(sent["turn"], 1);

    let mut events = Vec::new();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        let is_done = ev["event"] == "done";
        events.push(ev);
        if is_done {
            break;
        }
    }
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "message" && e["text"] == "List the files")
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "act" && e["title"] == "Run ls" && e["state"] == "done")
    );
    let combined: String = events
        .iter()
        .filter(|e| e["event"] == "text")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert_eq!(combined, "Checking done.");

    // Harness is now started.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], true);

    // List shows the thread with its updated title.
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut peer).unwrap();
    assert_eq!(listed["reply"], "list");
    assert_eq!(listed["threads"].as_array().unwrap().len(), 1);
    assert_eq!(listed["threads"][0]["thread"], thread_id);
    assert_eq!(listed["threads"][0]["title"], "Inspect workspace");
    assert_eq!(listed["threads"][0]["state"], "done");
    assert_eq!(listed["threads"][0]["working"], false);

    // Stop Lever cleanly, restart, and read the thread from the database with no harness running.
    drop(peer);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let mut restarted = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&restarted.person);
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["reply"], "resumed");
    assert_eq!(resumed["thread"], thread_id);
    assert_eq!(resumed["title"], "Inspect workspace");
    let turn = &resumed["turns"][0];
    assert_eq!(turn["message"], "List the files");
    assert_eq!(turn["answer"], "Checking done.");
    assert_eq!(turn["state"], "done");
    assert_eq!(turn["acts"][0]["title"], "Run ls");
    assert_eq!(turn["acts"][0]["state"], "done");
    assert_eq!(turn["acts"][0]["detail"], "Cargo.toml\nsrc");

    // Reading the record did not start the harness.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    restarted.signal(libc::SIGTERM);
    assert_eq!(restarted.wait().code(), Some(0));
}

#[test]
fn unknown_update_is_kept_as_json_and_shown_plainly_and_post_turn_updates_are_recorded() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let during_unknown = json!({
        "sessionUpdate": "vendor_metric",
        "summary": "Index cache warmed",
        "count": 42
    });
    let after_unknown = json!({
        "sessionUpdate": "post_turn_audit",
        "summary": "Background check finished"
    });
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [{
                "steps": [
                    { "kind": "act", "id": "acp-late-call", "tool_kind": "read", "title": "Read file", "status": "in_progress" },
                    { "kind": "unknown", "update": during_unknown },
                    { "kind": "text", "text": "Primary answer." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 30 },
                    { "kind": "unknown", "update": after_unknown },
                    { "kind": "act_update", "id": "acp-late-call", "status": "completed", "content": "finished after turn" }
                ]
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "Check cache", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    let thread_id = sent["thread"].as_str().unwrap().to_string();

    // Collect live events through `done` and the two post-turn updates.
    let mut saw_during_note = false;
    let mut saw_done = false;
    let mut saw_after_note = false;
    let mut saw_after_act = false;
    while !(saw_done && saw_after_note && saw_after_act) {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "note" && ev["text"] == "Index cache warmed" {
            saw_during_note = true;
        } else if ev["event"] == "done" {
            saw_done = true;
        } else if ev["event"] == "note" && ev["text"] == "Background check finished" {
            assert!(saw_done, "post-turn note arrived before done");
            saw_after_note = true;
        } else if ev["event"] == "act" && ev["detail"] == "finished after turn" && ev["state"] == "done" {
            assert!(saw_done, "post-turn act update arrived before done");
            saw_after_act = true;
        }
    }
    assert!(saw_during_note);

    // Resume returns both notes and the post-turn act update.
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    let turn = &resumed["turns"][0];
    assert_eq!(turn["notes"].as_array().unwrap().len(), 2);
    assert_eq!(turn["notes"][0]["text"], "Index cache warmed");
    assert_eq!(turn["notes"][1]["text"], "Background check finished");
    assert_eq!(turn["acts"][0]["state"], "done");
    assert_eq!(turn["acts"][0]["detail"], "finished after turn");

    // Verify the database kept the JSON of each unknown update.
    let conn = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let mut stmt = conn
        .prepare("SELECT raw_json, plain_text, after_turn FROM notes WHERE thread_id = ?1 ORDER BY seq ASC")
        .unwrap();
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([&thread_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(serde_json::from_str::<Value>(&rows[0].0).unwrap(), during_unknown);
    assert_eq!(rows[0].1, "Index cache warmed");
    assert_eq!(rows[0].2, 0);
    assert_eq!(serde_json::from_str::<Value>(&rows[1].0).unwrap(), after_unknown);
    assert_eq!(rows[1].1, "Background check finished");
    assert_eq!(rows[1].2, 1);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn two_threads_work_concurrently_on_one_harness_and_stop_cancels_the_turn() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [
                {
                    "match": "slow task",
                    "steps": [{ "kind": "text", "text": "Working slowly..." }],
                    "stall": true
                },
                {
                    "match": "fast task",
                    "steps": [{ "kind": "text", "text": "Fast result." }]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
    let mut peer_a = handshake(&lever.person);
    let mut peer_b = handshake(&lever.person);

    // Thread A starts and stalls mid-turn.
    write_frame(
        &mut peer_a,
        &json!({ "op": "send", "text": "Run slow task", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent_a = read_frame(&mut peer_a).unwrap();
    let thread_a = sent_a["thread"].as_str().unwrap().to_string();
    loop {
        let ev = read_frame(&mut peer_a).unwrap();
        if ev["event"] == "text" && ev["text"] == "Working slowly..." {
            break;
        }
    }

    // While Thread A is still working, Thread B starts and finishes independently.
    write_frame(
        &mut peer_b,
        &json!({ "op": "send", "text": "Run fast task", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent_b = read_frame(&mut peer_b).unwrap();
    let thread_b = sent_b["thread"].as_str().unwrap().to_string();
    assert_ne!(thread_a, thread_b);

    let mut b_text = String::new();
    loop {
        let ev = read_frame(&mut peer_b).unwrap();
        if ev["event"] == "text" {
            b_text.push_str(ev["text"].as_str().unwrap());
        }
        if ev["event"] == "done" {
            break;
        }
    }
    assert_eq!(b_text, "Fast result.");

    // List shows Thread A working and Thread B done.
    write_frame(&mut peer_b, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut peer_b).unwrap();
    let threads = listed["threads"].as_array().unwrap();
    let row_a = threads.iter().find(|t| t["thread"] == thread_a).unwrap();
    let row_b = threads.iter().find(|t| t["thread"] == thread_b).unwrap();
    assert_eq!(row_a["working"], true);
    assert_eq!(row_b["working"], false);

    // Stop Thread A and verify it finishes as `stopped` and sent `session/cancel` to the harness.
    write_frame(&mut peer_a, &json!({ "op": "stop", "thread": thread_a, "turn": 1 })).unwrap();
    let mut saw_ok = false;
    let mut saw_stopped = false;
    while !(saw_ok && saw_stopped) {
        let frame = read_frame(&mut peer_a).unwrap();
        if frame["reply"] == "ok" {
            saw_ok = true;
        } else if frame["event"] == "stopped" && frame["thread"] == thread_a {
            saw_stopped = true;
        }
    }

    // Verify one harness process served both threads and received `session/cancel`.
    let rpc_lines: Vec<Value> = std::fs::read_to_string(&rpc_log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rpc_lines.iter().filter(|m| m["method"] == "initialize").count(),
        1,
        "one harness process must serve every thread"
    );
    assert_eq!(rpc_lines.iter().filter(|m| m["method"] == "session/new").count(), 2);
    assert_eq!(rpc_lines.iter().filter(|m| m["method"] == "session/prompt").count(), 2);
    assert_eq!(
        rpc_lines.iter().filter(|m| m["method"] == "session/cancel").count(),
        1,
        "stop must send session/cancel to the harness"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn no_harness_id_or_message_type_appears_on_levers_sockets() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let forbidden = [
        "acp-secret-session-",
        "acp-secret-tool-99",
        "session/update",
        "session/new",
        "session/prompt",
        "session/cancel",
        "sessionUpdate",
        "sessionId",
        "toolCallId",
        "agent_message_chunk",
        "tool_call",
        "tool_call_update",
        "stopReason",
        "end_turn",
        "in_progress",
        "vendor_secret_tag",
        "acp-secret-tool-99-777",
        "-777",
    ];
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "session_prefix": "acp-secret-session-",
            "turns": [
                {
                    "steps": [
                        { "kind": "act", "id": "acp-secret-tool-99", "tool_kind": "execute", "title": "Run check", "status": "in_progress" },
                        { "kind": "act_update", "id": "acp-secret-tool-99", "status": "completed", "content": "ok" },
                        // An act with nothing to show but the harness's own raw value.
                        { "kind": "act", "id": "acp-secret-tool-100", "tool_kind": "other", "title": "Probe", "status": "completed",
                          "raw_output": { "sessionId": "acp-secret-session-1", "toolCallId": "acp-secret-tool-100", "exit": 0 } },
                        { "kind": "unknown", "update": { "sessionUpdate": "vendor_secret_tag", "summary": "Plain note" } },
                        // An update that names a tool call Lever has seen no act for.
                        // Its ID begins with one Lever does know, and all of it is taken out.
                        { "kind": "unknown", "update": { "sessionUpdate": "vendor_secret_tag", "toolCallId": "acp-secret-tool-99-777",
                          "message": "Waiting on acp-secret-tool-99-777" } },
                        { "kind": "text", "text": "All good." }
                    ]
                },
                {
                    "error": { "code": -32000, "message": "failed for acp-secret-session-1 and acp-secret-tool-99" }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut seen_frames: Vec<String> = Vec::new();
    let mut record = |frame: Value| -> Value {
        seen_frames.push(frame.to_string());
        frame
    };

    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    record(read_frame(&mut peer).unwrap());

    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "First", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = record(read_frame(&mut peer).unwrap());
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    loop {
        let ev = record(read_frame(&mut peer).unwrap());
        if ev["event"] == "done" {
            break;
        }
    }

    // Trigger an error whose raw message mentions harness IDs.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Second" }),
    )
    .unwrap();
    record(read_frame(&mut peer).unwrap());
    loop {
        let ev = record(read_frame(&mut peer).unwrap());
        if ev["event"] == "failed" {
            break;
        }
    }

    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    record(read_frame(&mut peer).unwrap());

    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    record(read_frame(&mut peer).unwrap());

    assert!(!seen_frames.is_empty());
    assert!(
        seen_frames
            .iter()
            .any(|frame| frame.contains("\"text\":\"Waiting on [redacted]\"")),
        "{seen_frames:?}"
    );
    for frame in &seen_frames {
        for bad in &forbidden {
            assert!(
                !frame.contains(bad),
                "harness token {bad:?} leaked into Lever socket frame: {frame}"
            );
        }
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn harness_starts_on_demand_stops_when_idle_and_restarts_for_next_turn() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            // What the harness replays of a session as it loads it. Lever has
            // all of it in its own record already.
            "load_replay": [
                { "kind": "text", "text": "REPLAYED ANSWER" },
                { "kind": "act", "id": "acp-replayed-call", "title": "Replayed act", "status": "completed" },
                { "kind": "unknown", "update": { "sessionUpdate": "replayed_note", "summary": "Replayed note" } }
            ],
            "turns": [
                { "steps": [{ "kind": "text", "text": "First turn." }] },
                { "steps": [{ "kind": "text", "text": "Second turn." }] }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), Some(200));
    let mut peer = handshake(&lever.person);

    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    // Turn 1 starts the harness.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "SUPER_SECRET_PERSON_TEXT", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    while read_frame(&mut peer).unwrap()["event"] != "done" {}

    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], true);

    // Wait for the idle timeout to stop the harness.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
        if read_frame(&mut peer).unwrap()["harness_started"] == false {
            break;
        }
        assert!(Instant::now() < deadline, "harness did not stop after idle timeout");
        std::thread::sleep(Duration::from_millis(30));
    }

    // Turn 2 on the same thread starts a fresh harness and loads the session.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Again" }),
    )
    .unwrap();
    let sent2 = read_frame(&mut peer).unwrap();
    assert_eq!(sent2["turn"], 2);
    while read_frame(&mut peer).unwrap()["event"] != "done" {}

    let rpc_text = std::fs::read_to_string(&rpc_log).unwrap();
    assert_eq!(
        rpc_text.matches("\"method\":\"initialize\"").count(),
        2,
        "harness must have started twice: {rpc_text}"
    );
    assert!(
        rpc_text.contains("\"method\":\"session/load\""),
        "resuming on a restarted harness should call session/load: {rpc_text}"
    );

    // The session was picked up again, so nothing says it began afresh, and
    // what the harness replayed as it loaded is in the record once.
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    // A stub that starts again begins its script again, so both turns got
    // the script's first answer.
    for turn in resumed["turns"].as_array().unwrap() {
        assert_eq!(turn["answer"], "First turn.", "{resumed}");
        assert_eq!(turn["acts"], json!([]), "{resumed}");
        assert_eq!(turn["notes"], json!([]), "{resumed}");
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let log = lever.log_text();
    assert!(log.contains("after idle timeout"), "expected idle stop in log: {log}");
    assert!(
        !log.contains("SUPER_SECRET_PERSON_TEXT"),
        "person text leaked to log: {log}"
    );
}

#[test]
fn harness_crash_and_lever_kill_record_interrupted_turns() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [
                {
                    "match": "crash now",
                    "steps": [{ "kind": "text", "text": "Half" }],
                    "crash": true
                },
                {
                    "match": "stall for kill",
                    "steps": [{ "kind": "text", "text": "Waiting" }],
                    "stall": true
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);

    // Case 1: harness crashes mid-turn.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "Please crash now", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "interrupted" && ev["thread"] == thread_id && ev["turn"] == 1 {
            break;
        }
    }
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    // Case 2: Lever itself is killed mid-turn.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Please stall for kill" }),
    )
    .unwrap();
    let sent2 = read_frame(&mut peer).unwrap();
    assert_eq!(sent2["turn"], 2);
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "text" && ev["text"] == "Waiting" {
            break;
        }
    }
    drop(peer);
    lever.kill_and_wait();

    // Restart Lever: turn 2 is recorded as interrupted, and turn 1 remains interrupted.
    let mut restarted = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&restarted.person);
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["turns"][0]["state"], "interrupted");
    assert_eq!(resumed["turns"][1]["state"], "interrupted");
    assert_eq!(resumed["working"], false);

    restarted.signal(libc::SIGTERM);
    assert_eq!(restarted.wait().code(), Some(0));
}

/// Extracts every fenced ```json block from `lever/README.md`.
fn readme_json_blocks() -> Vec<Value> {
    let readme =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).expect("read lever/README.md");
    let mut blocks = Vec::new();
    let mut in_json = false;
    let mut buf = String::new();
    for line in readme.lines() {
        let trimmed = line.trim();
        if !in_json && trimmed == "```json" {
            in_json = true;
            buf.clear();
        } else if in_json && trimmed == "```" {
            in_json = false;
            blocks.push(
                serde_json::from_str::<Value>(&buf)
                    .unwrap_or_else(|e| panic!("invalid JSON block in lever/README.md: {e}\n{buf}")),
            );
        } else if in_json {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    blocks
}

#[test]
fn lever_speaks_every_message_documented_in_readme() {
    use std::collections::BTreeSet;

    let blocks = readme_json_blocks();
    let doc_ops: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("op").and_then(Value::as_str).map(str::to_string))
        .collect();
    let doc_replies: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("reply").and_then(Value::as_str).map(str::to_string))
        .collect();
    let doc_events: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("event").and_then(Value::as_str).map(str::to_string))
        .collect();

    assert!(
        blocks.contains(&json!({ "version": 1 })),
        "README must document version 1"
    );
    assert_eq!(
        doc_ops,
        BTreeSet::from(
            [
                "answer",
                "archive",
                "delete",
                "commands",
                "efforts",
                "list",
                "make-project",
                "models",
                "resume",
                "send",
                "set-default-mode",
                "set-effort",
                "set-free-models",
                "set-mode",
                "set-model",
                "status",
                "stop",
                "unqueue",
                "watch",
            ]
            .map(str::to_string)
        )
    );
    assert_eq!(
        doc_replies,
        BTreeSet::from(
            [
                "activity",
                "commands",
                "default-mode",
                "archived",
                "deleted",
                "effort-set",
                "efforts",
                "error",
                "free-models",
                "list",
                "mode-set",
                "model-set",
                "models",
                "ok",
                "project-made",
                "queued",
                "resumed",
                "sent",
                "status",
                "steered",
            ]
            .map(str::to_string)
        )
    );
    assert_eq!(
        doc_events,
        BTreeSet::from(
            [
                "archived",
                "deleted",
                "threads",
                "act",
                "activity",
                "commands",
                "done",
                "failed",
                "free-models",
                "interrupted",
                "message",
                "models",
                "note",
                "queued",
                "request",
                "steered",
                "stopped",
                "subagent",
                "subagent-act",
                "subagent-text",
                "text",
                "thread-updated"
            ]
            .map(str::to_string)
        )
    );

    // Exercise Lever against the stub harness and verify it produces every
    // documented reply and event.
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [
                { "id": "stub/stub-1", "name": "Stub One", "efforts": ["low", "high"] },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "modes": true,
            "steering": true,
            "steer_refused_for": "Then run the tests",
            "commands": [{ "name": "review", "description": "review changes", "hint": "commit, branch, or pr" }],
            "turns": [
                {
                    "match": "Ask",
                    "steps": [
                        { "kind": "ask", "id": "c9", "title": "echo hi", "raw_input": { "command": "echo hi" } },
                        { "kind": "text", "text": "Asked." }
                    ]
                },
                {
                    "steps": [
                        { "kind": "act", "id": "c1", "tool_kind": "execute", "title": "Run ls", "status": "completed", "content": "Cargo.toml" },
                        { "kind": "subagent_start", "id": "ses_readme_sub", "title": "Check workspace", "prompt": "Inspect files" },
                        { "kind": "subagent_act", "subagent": "ses_readme_sub", "id": "c2", "tool_kind": "read", "title": "Read README.md", "status": "completed" },
                        { "kind": "subagent_text", "subagent": "ses_readme_sub", "text": "Found two files." },
                        { "kind": "subagent_end", "subagent": "ses_readme_sub", "status": "completed" },
                        { "kind": "unknown", "update": { "sessionUpdate": "extra", "summary": "Cache warm" } },
                        { "kind": "title", "title": "Inspect workspace" },
                        { "kind": "text", "text": "Here are the files." }
                    ]
                },
                { "steps": [{ "kind": "text", "text": "Wait" }], "stall": true },
                { "error": { "code": -32000, "message": "boom" } },
                { "steps": [{ "kind": "text", "text": "Crash" }], "crash": true }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut seen_replies: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut seen_events: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let record = |v: Value,
                  replies: &mut std::collections::BTreeMap<String, Value>,
                  events: &mut std::collections::BTreeMap<String, Value>|
     -> Value {
        // The first of each is kept. Of `thread-updated` and `commands`, it
        // is the first that says what the README's says: the title, and a
        // thread's commands.
        if let Some(r) = v.get("reply").and_then(Value::as_str) {
            replies.entry(r.to_string()).or_insert_with(|| v.clone());
        }
        if let Some(e) = v.get("event").and_then(Value::as_str) {
            let as_documented = match e {
                "thread-updated" => v.get("title").is_some(),
                "commands" => v.get("thread").is_some(),
                _ => true,
            };
            if as_documented {
                events.entry(e.to_string()).or_insert_with(|| v.clone());
            }
        }
        v
    };

    // status + error + set-free-models + models
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "unsupported-op" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    // efforts + commands, asked until the harness has named its commands, so
    // none comes as an event of its own once this peer watches.
    write_frame(&mut peer, &json!({ "op": "efforts", "model": "stub/stub-1" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    wait_until("the harness to name its commands", || {
        write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
        read_frame(&mut peer).unwrap()["known"] == true
    });
    write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "watch" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "set-free-models", "enabled": false })).unwrap();
    while !seen_events.contains_key("free-models") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "models", "refresh": true })).unwrap();
    while !seen_events.contains_key("models") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 1: send -> message, commands, act, note, thread-updated, text, done
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "List the files", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = loop {
        let frame = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if frame["reply"] == "sent" {
            break frame;
        }
    };
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "done" {
            break;
        }
    }

    // A turn that asks: request, answer -> ok, done
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Ask first" }),
    )
    .unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    let request_id = loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "request" {
            break ev["request"].as_str().unwrap().to_string();
        }
    };
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread_id, "request": request_id, "answer": "once" }),
    )
    .unwrap();
    loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "done" {
            break;
        }
    }

    // set-effort + set-model + set-mode + set-default-mode + list + resume
    write_frame(
        &mut peer,
        &json!({ "op": "set-effort", "thread": thread_id, "effort": "high" }),
    )
    .unwrap();
    while !seen_replies.contains_key("effort-set") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(
        &mut peer,
        &json!({ "op": "set-model", "thread": thread_id, "model": "stub/stub-2" }),
    )
    .unwrap();
    while !seen_replies.contains_key("model-set") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(
        &mut peer,
        &json!({ "op": "set-mode", "thread": thread_id, "mode": "full-access" }),
    )
    .unwrap();
    while !seen_replies.contains_key("mode-set") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "set-default-mode", "mode": "ask-first" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    while !seen_replies.contains_key("list") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    while !seen_replies.contains_key("resumed") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 3: stop -> ok + stopped
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Stall" }),
    )
    .unwrap();
    loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "text" {
            break;
        }
    }
    // A message sent while it works is steered into it: steered + steered.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Also check the docs" }),
    )
    .unwrap();
    while !seen_replies.contains_key("steered") || !seen_events.contains_key("steered") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    // One the harness does not take waits behind the turn: queued + queued,
    // and the person takes it back: unqueue -> ok.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Then run the tests" }),
    )
    .unwrap();
    while !seen_replies.contains_key("queued") || !seen_events.contains_key("queued") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    let queued_id = seen_replies["queued"]["queued"].clone();
    write_frame(
        &mut peer,
        &json!({ "op": "unqueue", "thread": thread_id, "queued": queued_id }),
    )
    .unwrap();
    loop {
        let v = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if v["event"] == "queued" && v["state"] == "removed" {
            break;
        }
    }
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread_id, "turn": 3 })).unwrap();
    while !seen_replies.contains_key("ok") || !seen_events.contains_key("stopped") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 3: failed
    write_frame(&mut peer, &json!({ "op": "send", "thread": thread_id, "text": "Fail" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    while !seen_events.contains_key("failed") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 4: interrupted
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Crash" }),
    )
    .unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    while !seen_events.contains_key("interrupted") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    write_frame(&mut peer, &json!({"op":"list","watch":true})).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(
        &mut peer,
        &json!({ "op": "make-project", "thread": thread_id, "name": "readme-check" }),
    )
    .unwrap();
    while !seen_replies.contains_key("project-made") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    for (op, reply) in [("archive", "archived"), ("delete", "deleted")] {
        write_frame(&mut peer, &json!({"op":op,"thread":thread_id})).unwrap();
        loop {
            let v = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
            if v["event"] == "threads" && seen_replies.contains_key(reply) && seen_events.contains_key(reply) {
                break;
            }
        }
    }

    let seen_reply_names: BTreeSet<String> = seen_replies.keys().cloned().collect();
    let seen_event_names: BTreeSet<String> = seen_events.keys().cloned().collect();
    assert_eq!(seen_reply_names, doc_replies);
    assert_eq!(seen_event_names, doc_events);

    // Every documented reply and event block's top-level keys must match what Lever emits.
    for block in &blocks {
        if let Some(name) = block.get("reply").and_then(Value::as_str) {
            let actual = &seen_replies[name];
            let doc_keys: BTreeSet<&str> = block.as_object().unwrap().keys().map(String::as_str).collect();
            let actual_keys: BTreeSet<&str> = actual
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .filter(|k| !(matches!(*k, "model" | "effort") && name == "resumed"))
                .collect();
            assert_eq!(actual_keys, doc_keys, "reply {name:?} keys drifted from README");
        }
        if let Some(name) = block.get("event").and_then(Value::as_str) {
            let actual = &seen_events[name];
            let doc_keys: BTreeSet<&str> = block.as_object().unwrap().keys().map(String::as_str).collect();
            let actual_keys: BTreeSet<&str> = actual.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(actual_keys, doc_keys, "event {name:?} keys drifted from README");
        }
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn reused_tool_call_id_short_id_in_answer_and_error_on_cancel_are_handled_correctly() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [
                {
                    "steps": [
                        { "kind": "act", "id": "c1", "tool_kind": "execute", "title": "First act", "status": "completed", "content": "one" },
                        { "kind": "text", "text": "Use vec1 here." }
                    ]
                },
                {
                    "steps": [
                        { "kind": "act", "id": "c1", "tool_kind": "read", "title": "Second act", "status": "in_progress" },
                        { "kind": "act_update", "id": "c1", "status": "completed", "content": "two" },
                        { "kind": "text", "text": "Turn two." }
                    ]
                },
                {
                    "steps": [{ "kind": "text", "text": "Cancel me" }],
                    "stall": true,
                    "cancel_with_error": true
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);

    // Turn 1: short tool ID "c1" must not corrupt "vec1" in the answer.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "One", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent1 = read_frame(&mut peer).unwrap();
    let thread_id = sent1["thread"].as_str().unwrap().to_string();
    let mut t1_text = String::new();
    let mut t1_act_id = String::new();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "text" {
            t1_text.push_str(ev["text"].as_str().unwrap());
        } else if ev["event"] == "act" {
            t1_act_id = ev["act"].as_str().unwrap().to_string();
        } else if ev["event"] == "done" {
            break;
        }
    }
    assert_eq!(t1_text, "Use vec1 here.");

    // Turn 2: reusing harness tool call ID "c1" creates a new act on turn 2 without overwriting turn 1's act.
    write_frame(&mut peer, &json!({ "op": "send", "thread": thread_id, "text": "Two" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["turn"], 2);
    let mut t2_act_id = String::new();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "act" {
            assert_eq!(ev["turn"], 2, "turn 2 act must broadcast under turn 2: {ev}");
            t2_act_id = ev["act"].as_str().unwrap().to_string();
        } else if ev["event"] == "done" {
            break;
        }
    }
    assert_ne!(t1_act_id, t2_act_id, "turn 2 must get its own Lever act ID");

    // Turn 3: harness answering `session/cancel` with a JSON-RPC error still finishes as `stopped`.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Three" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["turn"], 3);
    loop {
        if read_frame(&mut peer).unwrap()["event"] == "text" {
            break;
        }
    }
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread_id, "turn": 3 })).unwrap();
    let mut saw_stopped = false;
    while !saw_stopped {
        let frame = read_frame(&mut peer).unwrap();
        assert_ne!(
            frame["event"], "failed",
            "cancelled turn must not end as failed: {frame}"
        );
        if frame["event"] == "stopped" && frame["turn"] == 3 {
            saw_stopped = true;
        }
    }

    // Verify all three turns and both acts are intact in the resumed record.
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["turns"][0]["acts"][0]["title"], "First act");
    assert_eq!(resumed["turns"][0]["acts"][0]["detail"], "one");
    assert_eq!(resumed["turns"][1]["acts"][0]["title"], "Second act");
    assert_eq!(resumed["turns"][1]["acts"][0]["detail"], "two");
    assert_eq!(resumed["turns"][2]["state"], "stopped");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_id_the_harness_gives_twice_is_one_threads_alone() {
    let dir = TempDir::new();
    // A prefix of the script's own has every stub process count its sessions
    // from one, so a harness started again gives out IDs it gave before.
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "A three", "steps": [{ "kind": "text", "text": "A3." }] },
                { "match": "B one", "steps": [{ "kind": "text", "text": "B1." }] },
                { "match": "B two", "steps": [{ "kind": "text", "text": "B2." }] },
                { "match": "C one", "steps": [{ "kind": "text", "text": "C1." }] },
                { "match": "D one", "steps": [{ "kind": "text", "text": "D1." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);

    // The first harness gives thread A session 1.
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);

    // The second gives session 1 to thread B. Thread A cannot load it now:
    // it is B's. A gets a session of its own, and a note that says so.
    let (b, turn) = send(&mut peer, None, "B one");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B1.");
    let (_, turn) = send(&mut peer, Some(&a), "A two");
    let events = turn_events(&mut peer, &a, turn);
    assert_eq!(text_of(&events), "A2.", "{events:?}");
    let note = events.iter().find(|event| event["event"] == "note").expect("a note");
    assert!(
        note["text"].as_str().unwrap().contains("began a new one"),
        "the thread must say its harness began afresh: {note}"
    );

    // Each thread's next answer is its own.
    let (_, turn) = send(&mut peer, Some(&b), "B two");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B2.");
    let record_a = resume(&mut peer, &a);
    let record_b = resume(&mut peer, &b);
    assert_eq!(record_a["turns"].as_array().unwrap().len(), 2, "{record_a}");
    assert_eq!(record_a["turns"][1]["answer"], "A2.", "{record_a}");
    assert_eq!(record_a["turns"][1]["notes"].as_array().unwrap().len(), 1, "{record_a}");
    assert_eq!(record_b["turns"].as_array().unwrap().len(), 2, "{record_b}");
    assert_eq!(record_b["turns"][1]["answer"], "B2.", "{record_b}");
    assert_eq!(record_b["turns"][1]["notes"], json!([]), "{record_b}");
    end_the_harness(&mut peer);

    // The third loads A's own session, 2, and A goes on with no note. Then a
    // new thread is given 1, which is free, and the next is given 2, which is
    // A's: that thread's turn fails, and A keeps its session.
    let (_, turn) = send(&mut peer, Some(&a), "A three");
    let events = turn_events(&mut peer, &a, turn);
    assert_eq!(text_of(&events), "A3.", "{events:?}");
    assert!(events.iter().all(|event| event["event"] != "note"), "{events:?}");
    let (c, turn) = send(&mut peer, None, "C one");
    assert_eq!(text_of(&turn_events(&mut peer, &c, turn)), "C1.");
    let (d, turn) = send(&mut peer, None, "D one");
    let events = turn_events(&mut peer, &d, turn);
    let end = events.last().unwrap();
    assert_eq!(end["event"], "failed", "{events:?}");
    assert!(
        end["message"].as_str().unwrap().contains("another thread is using"),
        "{end}"
    );
    let record_a = resume(&mut peer, &a);
    assert_eq!(record_a["turns"].as_array().unwrap().len(), 3, "{record_a}");
    assert_eq!(record_a["turns"][2]["answer"], "A3.", "{record_a}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_whose_session_cannot_be_loaded_says_its_harness_began_afresh() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "load_session": false,
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "One", "steps": [{ "kind": "text", "text": "First." }] },
                { "match": "Two", "steps": [{ "kind": "text", "text": "Second." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    let (thread, turn) = send(&mut peer, None, "One");
    let events = turn_events(&mut peer, &thread, turn);
    assert!(events.iter().all(|event| event["event"] != "note"), "{events:?}");
    end_the_harness(&mut peer);

    let (_, turn) = send(&mut peer, Some(&thread), "Two");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Second.");
    let said = "The harness could not pick this thread's session up again. \
                It began a new one, which has none of the turns before.";
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "note")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [said],
        "{events:?}"
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["notes"], json!([]), "{record}");
    assert_eq!(record["turns"][1]["notes"][0]["text"], said, "{record}");
    assert!(!std::fs::read_to_string(&rpc_log).unwrap().contains("session/load"));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_that_fails_after_its_turn_was_stopped_fails_the_turn_that_waits_and_no_other() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "new_session_delay_ms": 400,
            "new_session_error": { "code": -32000, "message": "no sessions today" }
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let session_requests = || {
        std::fs::read_to_string(&rpc_log)
            .unwrap_or_default()
            .matches("\"method\":\"session/new\"")
            .count()
    };

    // A turn is stopped while the harness still has its request for a
    // session, and nothing more is sent. When the harness says there is no
    // session, no turn waits for one: the turn stays stopped, and nobody is
    // told it failed.
    let (alone, _) = send(&mut peer, None, "Alone");
    wait_until("the harness to be asked for a session", || session_requests() == 1);
    write_frame(&mut peer, &json!({ "op": "stop", "thread": alone, "turn": 1 })).unwrap();
    read_until(&mut peer, |frame| frame["event"] == "stopped" && frame["turn"] == 1);
    std::thread::sleep(Duration::from_millis(700));
    write_frame(&mut peer, &json!({ "op": "resume", "thread": alone })).unwrap();
    let record = read_until(&mut peer, |frame| {
        assert_ne!(frame["event"], "failed", "a stopped turn was failed: {frame}");
        frame.get("reply").is_some()
    });
    assert_eq!(record["turns"][0]["state"], "stopped", "{record}");

    // Turn 1 of another thread is stopped in the same way.
    let (thread, _) = send(&mut peer, None, "One");
    wait_until("the harness to be asked for a session", || session_requests() == 2);
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": 1 })).unwrap();
    read_until(&mut peer, |frame| frame["event"] == "stopped" && frame["turn"] == 1);

    // Turn 2 waits for that same session. When the harness says there is
    // none, turn 2 fails, and turn 1 stays as the person left it.
    let (_, turn) = send(&mut peer, Some(&thread), "Two");
    assert_eq!(turn, 2);
    let failed = read_until(&mut peer, |frame| frame["event"] == "failed");
    assert_eq!(failed["turn"], 2, "{failed}");
    assert_eq!(failed["message"], "no sessions today");
    assert_eq!(session_requests(), 2, "one thread asks for one session at a time");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["state"], "stopped", "{record}");
    assert_eq!(record["turns"][1]["state"], "failed", "{record}");
    assert_eq!(record["working"], false, "{record}");

    // The thread takes another message: no turn of it was left working.
    let (_, turn) = send(&mut peer, Some(&thread), "Three");
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "failed");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_peer_that_reads_none_of_a_threads_events_is_closed_and_the_thread_goes_on() {
    const PIECE: usize = 64 * 1024;
    const PIECES: usize = 96;
    let dir = TempDir::new();
    let mut steps = vec![json!({ "kind": "pause_ms", "ms": 300 })];
    steps.extend((0..PIECES).map(|_| json!({ "kind": "text", "text": "x".repeat(PIECE) })));
    let script = write_script(&dir.0, &json!({ "turns": [{ "steps": steps }] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);

    // One peer begins the turn and reads nothing more. Another watches the
    // same thread and reads all of it.
    let mut stalled = handshake(&lever.person);
    let (thread, turn) = send(&mut stalled, None, "Say a great deal");
    let mut reader = handshake(&lever.person);
    assert_eq!(resume(&mut reader, &thread)["reply"], "resumed");
    let events = turn_events(&mut reader, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "done");
    assert_eq!(text_of(&events).len(), PIECE * PIECES);

    // Lever did not keep the whole answer in memory for the peer that read
    // nothing: it closed it, and said why.
    wait_until("the stalled peer to be closed", || {
        lever
            .log_text()
            .contains("connection: closed, the person left more than 4 MiB of events unread")
    });
    let mut got = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match stalled.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(err) if err.kind() == io::ErrorKind::ConnectionReset => break,
            Err(err) => panic!("Lever kept a peer that read nothing, after {got} bytes: {err}"),
        }
    }
    assert!(
        got < PIECE * PIECES,
        "the stalled peer was sent everything: {got} bytes"
    );

    // The record has what that peer missed, for when it comes back.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let (state, saved): (String, i64) = db
        .query_row("SELECT state, length(answer) FROM turns", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!((state.as_str(), saved as usize), ("done", PIECE * PIECES));
    drop(db);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_database_did_not_take_fails_and_its_end_is_saved_when_the_database_takes_it() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "steps": [
                    { "kind": "text", "text": "Kept. " },
                    { "kind": "pause_ms", "ms": 600 },
                    { "kind": "text", "text": "Lost." }
                ] },
                { "steps": [{ "kind": "text", "text": "Again." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Say two things");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // The database stops taking writes to its turns partway through the answer.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE turns RENAME TO turns_away").unwrap();

    // The rest of the answer still reaches who is watching, and the turn
    // ends as failed, because the record does not have all of it.
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Lost.", "{events:?}");
    let end = events.last().unwrap();
    assert_eq!(end["event"], "failed", "{events:?}");
    assert_eq!(end["message"], "Lever could not save all of this turn.");
    wait_until("the storage failure to be logged", || {
        lever.log_text().contains(" error: storage: ")
    });

    // When the database takes writes again, Lever saves how the turn ended,
    // and the thread is not left working.
    db.execute_batch("ALTER TABLE turns_away RENAME TO turns").unwrap();
    wait_until("the turn's end to be saved", || {
        resume(&mut peer, &thread)["turns"][0]["state"] == "failed"
    });
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["answer"], "Kept. ", "{record}");
    assert_eq!(record["turns"][0]["error"], "Lever could not save all of this turn.");
    assert_eq!(record["working"], false, "{record}");
    let (_, turn) = send(&mut peer, Some(&thread), "Say it again");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let log = lever.log_text();
    assert!(
        !log.contains("Kept.") && !log.contains("Lost."),
        "the answer is in the log: {log}"
    );
}

#[test]
fn a_turn_working_when_lever_is_told_to_stop_is_interrupted_in_the_record_it_leaves() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "steps": [{ "kind": "text", "text": "Waiting" }], "stall": true }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "Wait for me");
    read_until(&mut peer, |frame| frame["event"] == "text");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Read with no Lever running: the database it closed says how the turn ended.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let (state, ended): (String, Option<i64>) = db
        .query_row("SELECT state, ended_at FROM turns", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(state, "interrupted");
    assert!(ended.is_some());
}

#[test]
fn a_harness_failure_is_a_failure_though_the_person_had_asked_to_stop() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [{ "kind": "text", "text": "Working" }],
                "stall": true,
                "error": { "code": -32000, "message": "The provider went away." }
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Work");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // The harness answers the stop with an error that is not a cancellation.
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "The provider went away.");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["state"], "failed", "{record}");
    assert_eq!(record["turns"][0]["error"], "The provider went away.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_result_with_a_null_error_is_a_result_and_an_update_with_a_null_id_is_an_update() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "explicit_nulls": true, "turns": [{ "steps": [{ "kind": "text", "text": "Fine." }] }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Hello");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");
    assert_eq!(text_of(&events), "Fine.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_id_the_record_has_for_two_threads_is_loaded_for_neither() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "B one", "steps": [{ "kind": "text", "text": "B1." }] },
                { "match": "B two", "steps": [{ "kind": "text", "text": "B2." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    // One harness gives thread A session 1, and the next gives thread B the
    // same ID. A sends nothing while that harness runs, so nothing in memory
    // says the ID was A's once.
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);
    let (b, turn) = send(&mut peer, None, "B one");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B1.");
    end_the_harness(&mut peer);

    // What a harness holds under that ID now is B's, if it is anyone's. A
    // does not load it: A is given a new session, and a note that says so.
    // That new session has the ID again, so B has lost it in turn.
    for (thread, message, answer) in [(&a, "A two", "A2."), (&b, "B two", "B2.")] {
        let (_, turn) = send(&mut peer, Some(thread), message);
        let events = turn_events(&mut peer, thread, turn);
        assert_eq!(text_of(&events), answer, "{events:?}");
        assert_eq!(
            events.iter().filter(|event| event["event"] == "note").count(),
            1,
            "the thread must say its harness began afresh: {events:?}"
        );
    }
    let rpc_text = std::fs::read_to_string(&rpc_log).unwrap();
    assert!(
        !rpc_text.contains("session/load"),
        "a shared session was loaded: {rpc_text}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_id_one_thread_is_loading_is_not_given_to_another() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "load_delay_ms": 500,
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "B one", "steps": [{ "kind": "text", "text": "B1." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);

    // The new harness is slow to load A's session, 1, and meanwhile gives
    // the same ID to a new thread. That thread's turn fails: the session is
    // A's, and while it loads Lever takes none of its updates.
    let (_, turn_a) = send(&mut peer, Some(&a), "A two");
    wait_until("the harness to be asked to load A's session", || {
        std::fs::read_to_string(&rpc_log)
            .unwrap_or_default()
            .contains("session/load")
    });
    let (b, turn_b) = send(&mut peer, None, "B one");
    let mut ends = std::collections::HashMap::new();
    let mut a_text = String::new();
    while ends.len() < 2 {
        let frame = read_frame(&mut peer).unwrap();
        let thread = frame["thread"].as_str().unwrap_or_default().to_string();
        if frame["event"] == "text" && thread == a {
            a_text.push_str(frame["text"].as_str().unwrap());
        }
        if ["done", "failed", "stopped", "interrupted"]
            .iter()
            .any(|end| frame["event"] == *end)
        {
            ends.insert(thread, frame);
        }
    }
    assert_eq!(ends[&b]["event"], "failed", "{ends:?}");
    assert_eq!(ends[&b]["turn"], turn_b);
    assert!(
        ends[&b]["message"]
            .as_str()
            .unwrap()
            .contains("another thread is using"),
        "{ends:?}"
    );
    assert_eq!(ends[&a]["event"], "done", "{ends:?}");
    assert_eq!(ends[&a]["turn"], turn_a);
    assert_eq!(a_text, "A2.");
    let record = resume(&mut peer, &a);
    assert_eq!(
        record["turns"][1]["notes"],
        json!([]),
        "A loaded its own session: {record}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_ends_while_the_database_takes_no_writes_still_interrupts_its_turns() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "crash",
                    "steps": [{ "kind": "text", "text": "Half" }, { "kind": "pause_ms", "ms": 600 }],
                    "crash": true
                },
                { "steps": [{ "kind": "text", "text": "Back." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Now crash");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // No turn's state can be written when the harness ends.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF state ON turns");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "interrupted", "who is watching is told all the same");

    // When the database takes writes again, the record gets the turn's end,
    // and the thread takes another message.
    db.execute_batch("DROP TRIGGER refused").unwrap();
    wait_until("the turn's end to be saved", || {
        resume(&mut peer, &thread)["turns"][0]["state"] == "interrupted"
    });
    let (_, turn) = send(&mut peer, Some(&thread), "Go on");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "Back.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_the_database_takes_only_part_of_begins_no_turn() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "turns": [] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "First");
    turn_events(&mut peer, &thread, turn);

    // The database takes a new turn and a new thread, and refuses the
    // timestamp that is written after them.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF updated_at ON threads");
    for request in [
        json!({ "op": "send", "thread": thread, "text": "Second" }),
        json!({ "op": "send", "text": "Another thread", "cwd": "/tmp" }),
    ] {
        write_frame(&mut peer, &request).unwrap();
        let reply = read_until(&mut peer, |frame| frame.get("reply").is_some());
        assert_eq!(reply["code"], "storage-error", "{reply}");
    }
    db.execute_batch("DROP TRIGGER refused").unwrap();

    // Neither left anything behind: no second thread, and no turn that is
    // working in the record with nothing working on it.
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    let listed = read_until(&mut peer, |frame| frame.get("reply").is_some());
    assert_eq!(listed["threads"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["threads"][0]["turns"], 1, "{listed}");
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    let (_, turn) = send(&mut peer, Some(&thread), "Second, again");
    assert_eq!(turn, 2);
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "done");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(lever.log_text().contains(" error: storage: "), "{}", lever.log_text());
}

/// The memory a process holds, in KiB, as the kernel counts it.
fn resident_kib(pid: u32) -> usize {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse().ok())
        .expect("VmRSS")
}

#[test]
fn many_requests_for_a_long_record_from_a_peer_that_reads_nothing_are_answered_as_it_reads() {
    const PIECE: usize = 64 * 1024;
    const PIECES: usize = 24;
    const REQUESTS: usize = 64;
    let dir = TempDir::new();
    let steps: Vec<Value> = (0..PIECES)
        .map(|_| json!({ "kind": "text", "text": "x".repeat(PIECE) }))
        .collect();
    let script = write_script(&dir.0, &json!({ "turns": [{ "steps": steps }] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut first = handshake(&lever.person);
    let (thread, turn) = send(&mut first, None, "Say a great deal");
    assert_eq!(turn_events(&mut first, &thread, turn).last().unwrap()["event"], "done");
    drop(first);

    // A peer asks for the 1.5 MiB record many times over in one write, says
    // it will send no more, and reads nothing. The replies come to 96 MiB.
    let mut peer = handshake(&lever.person);
    let request = encode_frame(&json!({ "op": "resume", "thread": thread }));
    peer.write_all(&request.repeat(REQUESTS)).unwrap();
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    std::thread::sleep(Duration::from_millis(500));

    // Lever has answered only as far as the mark, and holds the rest of the
    // requests, not their replies.
    let held = resident_kib(lever.pid());
    assert!(held < 40 * 1024, "Lever holds {held} KiB for a peer that reads nothing");

    // Every request is answered as the peer reads, and then Lever closes.
    for n in 0..REQUESTS {
        let reply = read_frame(&mut peer).unwrap_or_else(|err| panic!("reply {n} was lost: {err}"));
        assert_eq!(reply["reply"], "resumed", "reply {n}");
        assert_eq!(reply["turns"][0]["answer"].as_str().unwrap().len(), PIECE * PIECES);
    }
    assert_closed_without_reply(peer);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_writes_a_line_with_no_end_is_stopped_and_its_turn_interrupted() {
    const MAX_LINE: usize = 16 * 1024 * 1024;
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "Flood",
                    "steps": [
                        { "kind": "text", "text": "Before." },
                        { "kind": "flood", "bytes": MAX_LINE + 1024 * 1024 }
                    ],
                    "stall": true
                },
                {
                    // As long a line as Lever takes with no end, and then,
                    // once Lever has read that, an update that ends it.
                    "match": "To the brim",
                    "steps": [
                        { "kind": "flood", "bytes": MAX_LINE },
                        { "kind": "pause_ms", "ms": 400 },
                        { "kind": "text", "text": "After." }
                    ],
                    "stall": true
                }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let stopped_for_it = |lever: &RunningLever| {
        lever
            .log_text()
            .matches("was stopped: it wrote a line longer than 16 MiB")
            .count()
    };

    let (thread, turn) = send(&mut peer, None, "Flood");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Before.");
    assert_eq!(events.last().unwrap()["event"], "interrupted", "{events:?}");
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);
    assert!(resident_kib(lever.pid()) < 60 * 1024, "Lever kept the line");
    assert_eq!(stopped_for_it(&lever), 1, "{}", lever.log_text());

    // A line that is too long is too long wherever its end falls in what
    // Lever reads.
    let (thread, turn) = send(&mut peer, None, "To the brim");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "interrupted", "{events:?}");
    assert_eq!(stopped_for_it(&lever), 2, "{}", lever.log_text());

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn one_event_longer_than_the_backlog_reaches_a_peer_that_reads() {
    const LONG: usize = 5 * 1024 * 1024;
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "steps": [{ "kind": "text", "text": "x".repeat(LONG) }] }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Say it all at once");

    // The peer reads as fast as it is sent to. It is behind on nothing.
    let mut got = 0;
    loop {
        let frame = read_frame_up_to(&mut peer, 2 * LONG)
            .unwrap_or_else(|err| panic!("Lever closed a peer that was reading: {err}"));
        if frame["event"] == "text" {
            got += frame["text"].as_str().unwrap().len();
        }
        if frame["event"] == "done" {
            assert_eq!(
                (frame["thread"].as_str(), frame["turn"].as_u64()),
                (Some(&*thread), Some(turn))
            );
            break;
        }
    }
    assert_eq!(got, LONG);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(!lever.log_text().contains("events unread"), "{}", lever.log_text());
}

#[test]
fn a_session_the_record_did_not_take_is_not_used() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "B unsaved", "steps": [{ "kind": "text", "text": "Never." }] },
                { "match": "B saved", "steps": [{ "kind": "text", "text": "B1." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);

    // The next harness gives thread B the ID the record has for A, and the
    // database does not take the write that makes it B's. B's turn fails,
    // and its message never reaches the harness: with the ID still A's in
    // the record, A would load at a later start what B had said.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF harness_session_id ON threads");
    let (b, turn) = send(&mut peer, None, "B unsaved");
    let end = turn_events(&mut peer, &b, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "Lever could not save this thread's session.");
    db.execute_batch("DROP TRIGGER refused").unwrap();
    assert!(!std::fs::read_to_string(&rpc_log).unwrap().contains("B unsaved"));

    // B's next turn is given a session the record takes, and A loads its own.
    let (_, turn) = send(&mut peer, Some(&b), "B saved");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B1.");
    end_the_harness(&mut peer);
    let (_, turn) = send(&mut peer, Some(&a), "A two");
    let events = turn_events(&mut peer, &a, turn);
    assert_eq!(text_of(&events), "A2.", "{events:?}");
    assert!(events.iter().all(|event| event["event"] != "note"), "{events:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_whose_end_waits_to_be_saved_has_ended_for_whoever_asks() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "steps": [{ "kind": "text", "text": "Waiting" }], "stall": true }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Wait");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // The turn is stopped while the database takes no turn's state.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF state ON turns");
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    assert_eq!(turn_events(&mut peer, &thread, turn).pop().unwrap()["event"], "stopped");

    // A peer that comes now was not there for that event, and none will say
    // it again: the record it is given, and the list, say the turn stopped.
    let mut late = handshake(&lever.person);
    let record = resume(&mut late, &thread);
    assert_eq!(record["turns"][0]["state"], "stopped", "{record}");
    assert_eq!(record["working"], false, "{record}");
    write_frame(&mut late, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut late).unwrap();
    assert_eq!(listed["threads"][0]["state"], "stopped", "{listed}");
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");

    // The thread is not working, and its next message is not taken either
    // until the record has how the turn ended. Lever says which it is.
    write_frame(&mut late, &json!({ "op": "send", "thread": thread, "text": "Go on" })).unwrap();
    let refused = read_frame(&mut late).unwrap();
    assert_eq!(refused["code"], "storage-error", "{refused}");
    assert_eq!(
        refused["message"],
        "Lever is still saving how this thread's last turn ended."
    );

    // Once the database takes writes, the next message is taken at once,
    // with no wait for Lever to try the save again on its own.
    db.execute_batch("DROP TRIGGER refused").unwrap();
    let (_, turn) = send(&mut late, Some(&thread), "Go on");
    assert_eq!(turn, 2);
    assert_eq!(turn_events(&mut late, &thread, turn).pop().unwrap()["event"], "done");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_peer_that_sends_and_resumes_at_once_is_told_of_the_new_turn_once() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "turns": [] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "One");
    turn_events(&mut peer, &thread, turn);

    // A message and a request for the record, in one write.
    let mut both = encode_frame(&json!({ "op": "send", "thread": thread, "text": "Two" }));
    both.extend(encode_frame(&json!({ "op": "resume", "thread": thread })));
    peer.write_all(&both).unwrap();
    let mut frames = Vec::new();
    while !(frames.iter().any(|frame: &Value| frame["event"] == "done")
        && frames.iter().any(|frame| frame["reply"] == "resumed"))
    {
        frames.push(read_frame(&mut peer).unwrap());
    }

    // The new turn's message comes as an event before the record that has
    // it, and not again after.
    let at = |found: &dyn Fn(&Value) -> bool| frames.iter().position(found);
    let messages = frames.iter().filter(|frame| frame["event"] == "message").count();
    let message = at(&|frame| frame["event"] == "message").expect("the message");
    let record = at(&|frame| frame["reply"] == "resumed").expect("the record");
    assert_eq!(messages, 1, "{frames:?}");
    assert!(message < record, "the record came before the event it has: {frames:?}");
    assert_eq!(frames[record]["turns"][1]["message"], "Two", "{frames:?}");

    // So with the answer, however much of it the harness had given by then:
    // what the record has and what comes after it are the answer, once.
    let after: String = text_of(&frames[record..]);
    let in_record = frames[record]["turns"][1]["answer"].as_str().unwrap();
    assert_eq!(format!("{in_record}{after}"), "stub echo: Two", "{frames:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_harness_says_when_it_cannot_start_is_the_persons_and_not_the_logs() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "initialize_error": { "code": -32000, "message": "HARNESS_DIAGNOSTIC\n2026 fake: line" } }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Hello");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "HARNESS_DIAGNOSTIC\n2026 fake: line");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let log = lever.log_text();
    assert!(log.contains(" error: the harness could not be initialized"), "{log}");
    assert!(
        !log.contains("HARNESS_DIAGNOSTIC") && !log.contains("fake: line"),
        "{log}"
    );
}

#[test]
fn a_session_with_no_id_is_no_session() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "session_id": "" }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Hello");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "The harness could not start a thread.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_option_given_twice_is_refused() {
    let given_twice = Command::new(BIN)
        .args([
            "--state",
            "/nonexistent",
            "--surface-socket",
            "/nonexistent/s",
            "--shell-uid",
            "1",
        ])
        .args(["--person-socket", "/nonexistent/p", "--person-uid", "1"])
        .args(["--idle-timeout-ms", "1000", "--idle-timeout-ms", "2000"])
        .output()
        .unwrap();
    assert_eq!(given_twice.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&given_twice.stderr).starts_with("usage: crowbar-lever"));
}

#[test]
fn a_long_event_a_peer_has_read_gives_it_no_more_room_to_fall_behind() {
    const MIB: usize = 1024 * 1024;
    let dir = TempDir::new();
    let short = || json!({ "kind": "text", "text": "y".repeat(MIB) });
    let mut steps = vec![json!({ "kind": "text", "text": "x".repeat(6 * MIB) })];
    steps.extend((0..3).map(|_| short()));
    steps.push(json!({ "kind": "pause_ms", "ms": 1500 }));
    steps.extend((0..4).map(|_| short()));
    let script = write_script(&dir.0, &json!({ "turns": [{ "steps": steps }] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let closed = |lever: &RunningLever| {
        lever
            .log_text()
            .contains("connection: closed, the person left more than 4 MiB of events unread")
    };

    // A peer lets a 6 MiB event and three of 1 MiB wait for it. That is
    // within 4 MiB over the longest of them, so it is kept.
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "Say a great deal");
    std::thread::sleep(Duration::from_millis(700));
    assert!(!closed(&lever), "{}", lever.log_text());

    // It reads the long event, and then nothing. Events were waiting behind
    // that one all along, so it never had none waiting.
    loop {
        let frame = read_frame_up_to(&mut peer, 8 * MIB).unwrap();
        if frame["event"] == "text" {
            assert_eq!(frame["text"].as_str().unwrap().len(), 6 * MIB);
            break;
        }
    }

    // Four more events of 1 MiB come. With the three it left, that is more
    // than 4 MiB over the longest still waiting, which is 1 MiB: the event
    // it has taken no longer counts for it.
    wait_until("the peer that stopped reading to be closed", || closed(&lever));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_peer_sent_before_it_went_is_carried_out() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "turns": [] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);

    // A peer states its version, sends two messages, and is gone, all
    // before Lever has read a byte of it.
    // The second is longer than Lever takes in one read, so part of it is
    // still to be read when Lever finds the peer gone.
    let second = format!("second {}", "x".repeat(12 * 1024));
    let mut all = encode_frame(&json!({ "version": 1 }));
    all.extend(encode_frame(&json!({ "op": "send", "text": "first", "cwd": "/tmp" })));
    all.extend(encode_frame(&json!({ "op": "send", "text": second, "cwd": "/tmp" })));
    let mut gone = connect(&lever.person);
    gone.write_all(&all).unwrap();
    drop(gone);

    // Both messages began their threads, and both were answered.
    let mut peer = handshake(&lever.person);
    wait_until("both threads to be done", || {
        write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
        let listed = read_frame(&mut peer).unwrap();
        let threads = listed["threads"].as_array().unwrap();
        threads.len() == 2 && threads.iter().all(|thread| thread["state"] == "done")
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_started_again_has_the_ids_of_earlier_acts_taken_out_of_what_it_says() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "restart the harness", "crash": true },
                {
                    "match": "Act",
                    "steps": [
                        { "kind": "act", "id": "acp-earlier-call-7", "title": "Look", "status": "completed" },
                        { "kind": "text", "text": "Looked." }
                    ]
                },
                { "match": "Fail", "error": { "code": -32000, "message": "Could not go on from acp-earlier-call-7." } }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Act");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "Looked.");
    end_the_harness(&mut peer);

    // The new harness has shown Lever no act, and names the old one.
    let (_, turn) = send(&mut peer, Some(&thread), "Fail");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "Could not go on from [redacted].");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["error"], "Could not go on from [redacted].");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn harness_env_disables_autoupdate_and_free_models_until_chosen_and_selects_models() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "no_models_when_free_disabled": true,
            "models": [
                { "id": "stub/stub-1", "name": "Stub One" },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "turns": [
                { "steps": [{ "kind": "text", "text": "first on {model}" }] },
                { "steps": [{ "kind": "text", "text": "second on {model}" }] },
                { "steps": [{ "kind": "text", "text": "restarted on {model}" }] }
            ]
        }),
    );
    let env_out = dir.0.join("env.json");
    let stub_bin = env!("CARGO_BIN_EXE_crowbar-stub-harness");
    let extra = vec![
        "--harness".into(),
        stub_bin.into(),
        "--harness-name".into(),
        "OpenCode".into(),
        "--harness-arg".into(),
        "--script".into(),
        "--harness-arg".into(),
        script.to_str().unwrap().into(),
        "--harness-arg".into(),
        "--env-out".into(),
        "--harness-arg".into(),
        env_out.to_str().unwrap().into(),
        "--harness-env".into(),
        "OPENCODE_DISABLE_MODELS_FETCH=1".into(),
    ];

    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    // Idle Lever starts no harness and has free_hosted_models = false.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut peer).unwrap();
    assert_eq!(status["harness_started"], false);
    assert_eq!(status["free_hosted_models"], false);
    assert!(!env_out.exists(), "idle Lever must not start the harness");

    // Before the person chooses, models is empty and OPENCODE_DISABLE_FREE_MODELS=1 + update:disable are passed.
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut peer).unwrap();
    assert_eq!(models["reply"], "models");
    assert_eq!(models["supported"], true);
    assert_eq!(models["models"], json!([]));
    assert_eq!(models["free_hosted_models"], false);

    let env1: Value = serde_json::from_str(&std::fs::read_to_string(&env_out).unwrap()).unwrap();
    assert_eq!(env1["OPENCODE_DISABLE_FREE_MODELS"], "1");
    assert_eq!(env1["OPENCODE_DISABLE_MODELS_FETCH"], "1");
    let cfg1: Value = serde_json::from_str(env1["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert_eq!(cfg1["update"], "disable");

    // Sending a message with no Models available is refused with `no-model` and the reason.
    write_frame(&mut peer, &json!({ "op": "send", "text": "Hello" })).unwrap();
    let refused = read_frame(&mut peer).unwrap();
    assert_eq!(refused["reply"], "error");
    assert_eq!(refused["code"], "no-model");
    assert!(refused["message"].as_str().unwrap().contains("No Model is available"));

    // Enabling free hosted Models stops the idle harness and restarts without OPENCODE_DISABLE_FREE_MODELS.
    write_frame(&mut peer, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    let toggled = read_frame(&mut peer).unwrap();
    assert_eq!(
        toggled,
        json!({ "reply": "free-models", "enabled": true, "models_chosen": true })
    );

    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut peer).unwrap();
    assert_eq!(models["free_hosted_models"], true);
    assert_eq!(models["default"], "stub/stub-1");
    assert_eq!(models["models"].as_array().unwrap().len(), 2);

    let env2: Value = serde_json::from_str(&std::fs::read_to_string(&env_out).unwrap()).unwrap();
    assert!(env2["OPENCODE_DISABLE_FREE_MODELS"].is_null());
    let cfg2: Value = serde_json::from_str(env2["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert_eq!(cfg2["update"], "disable");

    // Turn 1 uses default model stub/stub-1.
    let (tid, turn1) = send(&mut peer, None, "one");
    let ev1 = turn_events(&mut peer, &tid, turn1);
    assert_eq!(text_of(&ev1), "first on stub/stub-1");

    // Invalid Model is rejected cleanly.
    write_frame(
        &mut peer,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/missing" }),
    )
    .unwrap();
    let bad = read_frame(&mut peer).unwrap();
    assert_eq!(bad["reply"], "error");
    assert_eq!(bad["code"], "invalid-model");

    // Switch thread to stub/stub-2 and run turn 2.
    write_frame(
        &mut peer,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/stub-2" }),
    )
    .unwrap();
    let set_ok = read_frame(&mut peer).unwrap();
    assert_eq!(
        set_ok,
        json!({ "reply": "model-set", "thread": tid, "model": "stub/stub-2" })
    );
    let (_, turn2) = send(&mut peer, Some(&tid), "two");
    let ev2 = turn_events(&mut peer, &tid, turn2);
    assert_eq!(text_of(&ev2), "second on stub/stub-2");

    // Restart Lever: free_hosted_models and the thread's chosen Model persist and apply on turn 3.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    drop(peer);

    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let rec = resume(&mut peer, &tid);
    assert_eq!(rec["model"], "stub/stub-2");
    let (_, turn3) = send(&mut peer, Some(&tid), "three");
    let ev3 = turn_events(&mut peer, &tid, turn3);
    assert_eq!(text_of(&ev3), "first on stub/stub-2");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}
