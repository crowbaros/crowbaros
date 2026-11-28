//! External harness session discovery, store settling, classification, history import, delete races, and retry backoff.

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

fn start_lever_with_discovery(dir: &Path, script: &Path, log: Option<&Path>, watch_dir: &Path) -> RunningLever {
    let mut extra: Vec<String> = vec![
        "--harness".into(),
        STUB_BIN.into(),
        "--harness-name".into(),
        "Stub".into(),
        "--harness-arg".into(),
        "--script".into(),
        "--harness-arg".into(),
        script.to_str().unwrap().into(),
        "--harness-watch".into(),
        watch_dir.to_str().unwrap().into(),
        "--discovery-debounce-ms".into(),
        "40".into(),
    ];
    if let Some(log) = log {
        extra.push("--harness-arg".into());
        extra.push("--log".into());
        extra.push("--harness-arg".into());
        extra.push(log.to_str().unwrap().into());
    }
    RunningLever::start_with_extra(dir, my_uid(), my_uid(), &extra)
}

fn write_sessions_file(path: &Path, sessions: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(sessions).unwrap()).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path).map(|t| t.lines().count()).unwrap_or(0)
}

#[test]
fn a_listing_that_writes_to_the_store_it_watches_does_not_start_the_harness_again_and_again() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    let sessions_file = store_dir.join("sessions.json");
    let starts = store_dir.join("starts.log");
    let cwd = dir.0.join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_hand_1",
            "cwd": cwd.to_str().unwrap(),
            "title": "By hand",
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        }]),
    );
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "touch_on_start": starts.to_str().unwrap(),
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    let initial = read_frame(&mut watcher).unwrap();
    if initial["threads"].as_array().is_none_or(Vec::is_empty) {
        read_until(&mut watcher, |f| {
            f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 1)
        });
    }

    // Each listing writes to the store, which must not bring the next.
    std::thread::sleep(Duration::from_millis(1500));
    let settled = count_lines(&starts);
    assert!(settled <= 2, "the harness started {settled} times for one store change");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        count_lines(&starts),
        settled,
        "the harness kept starting on a quiet store"
    );

    // A change of the person's is still seen.
    let mut sessions: Value = serde_json::from_slice(&std::fs::read(&sessions_file).unwrap()).unwrap();
    sessions.as_array_mut().unwrap().push(json!({
        "session_id": "ses_hand_2",
        "cwd": cwd.to_str().unwrap(),
        "title": "Also by hand",
        "history": [{ "kind": "user_text", "text": "again" }, { "kind": "text", "text": "yes" }]
    }));
    write_sessions_file(&sessions_file, &sessions);
    read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 2)
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

fn a_session_written_while_a_follow_up_lists_is_still_found(before_answer: bool) {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    let sessions_file = store_dir.join("sessions.json");
    let starts = store_dir.join("starts.log");
    let session = |id: &str, title: &str| {
        json!({
            "session_id": id,
            "cwd": "/tmp",
            "title": title,
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        })
    };
    write_sessions_file(&sessions_file, &json!([session("ses_first", "First")]));
    // The first listing's own writes bring a follow-up; the person's session
    // is written once that follow-up has listed, and nothing writes after.
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "touch_on_start": starts.to_str().unwrap(),
            "change_after_list": {
                "on_start": 2,
                "before_answer": before_answer,
                // After the second listing's answer, the change only the
                // watch to the pass's end sees; before the first's, the
                // change the second listing has in it.
                "listing": if before_answer { 1 } else { 2 },
                "sessions": [session("ses_first", "First"), session("ses_late", "Late")]
            }
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    let initial = read_frame(&mut watcher).unwrap();
    if !initial["threads"]
        .as_array()
        .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Late"))
    {
        read_until(&mut watcher, |f| {
            f["event"] == "threads"
                && f["threads"]
                    .as_array()
                    .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Late"))
        });
    }
    assert!(count_lines(&starts) >= 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_written_after_a_follow_up_answered_its_listing_is_still_found() {
    a_session_written_while_a_follow_up_lists_is_still_found(false);
}

#[test]
fn a_session_written_after_a_follow_up_read_its_sessions_but_before_it_answered_is_still_found() {
    a_session_written_while_a_follow_up_lists_is_still_found(true);
}

fn a_session_written_while_the_running_harness_lists_is_still_found_at(listing: u64) {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let session = |id: &str, title: &str| {
        json!({
            "session_id": id,
            "cwd": "/tmp",
            "title": title,
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        })
    };
    // Lever's own harness runs, and its turn writes to the store as it
    // lists; the person's session is written after the running harness's
    // follow-up has read its sessions, and nothing writes after.
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "touch_on_start": dir.0.join("starts.log").to_str().unwrap(),
            "touch_on_list": store_dir.join("turn.log").to_str().unwrap(),
            "change_after_list": {
                "on_start": 1,
                "listing": listing,
                "before_answer": true,
                "sessions": [session("ses_first", "First"), session("ses_late", "Late")]
            },
            "turns": [{ "match": "hello", "steps": [{ "kind": "text", "text": "Hi." }] }]
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let (thread, turn) = send(&mut peer, None, "hello");
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "done");

    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");
    write_sessions_file(&sessions_file, &json!([session("ses_first", "First")]));
    read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Late"))
    });
    assert_eq!(count_lines(&dir.0.join("starts.log")), 1, "the running harness listed");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_written_while_the_running_harness_lists_is_still_found() {
    a_session_written_while_the_running_harness_lists_is_still_found_at(2);
}

#[test]
fn a_session_written_while_the_running_harness_lists_its_last_follow_up_is_still_found() {
    // Its fifth listing is the last follow-up the cap allows.
    a_session_written_while_the_running_harness_lists_is_still_found_at(5);
}

#[test]
fn a_new_store_is_not_listed_until_whoever_made_it_has_set_it_up() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({ "list_sessions": true, "sessions_file": sessions_file.to_str().unwrap() }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, Some(&rpc_log), &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    // A harness run for the first time makes its store and sets it up,
    // writing as it does: a listing then would start a second harness that
    // sets it up too.
    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_first_ever",
            "cwd": "/tmp",
            "title": "First ever",
            "history": [{ "kind": "user_text", "text": "go" }]
        }]),
    );
    let part = store_dir.join("setup.log");
    let setting_up = Instant::now();
    while setting_up.elapsed() < Duration::from_millis(1500) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)
            .unwrap();
        std::io::Write::write_all(&mut f, b"setup\n").unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    let set_up = Instant::now();
    let rpc = std::fs::read_to_string(&rpc_log).unwrap_or_default();
    assert!(
        !rpc.contains("\"method\":\"session/list\""),
        "a listing started a harness while the new store was being set up:\n{rpc}"
    );

    // Once it has been quiet a moment, the session is listed.
    read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 1)
    });
    let after = set_up.elapsed();
    assert!(
        after >= Duration::from_millis(800),
        "listed only {after:?} after the store was set up"
    );
    assert!(
        after < Duration::from_secs(3),
        "listed {after:?} after the store was set up"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_appears_while_its_store_keeps_changing() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    // A store the harness has used before.
    std::fs::write(store_dir.join("parts.log"), b"part\n").unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let cwd = dir.0.join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let script = write_script(
        &dir.0,
        &json!({ "list_sessions": true, "sessions_file": sessions_file.to_str().unwrap() }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_streaming",
            "cwd": cwd.to_str().unwrap(),
            "title": "Still answering",
            "history": [{ "kind": "user_text", "text": "go" }]
        }]),
    );
    // The session's turn goes on writing to the store, faster than the
    // debounce, for longer than the longest wait.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let stop = std::sync::Arc::clone(&stop);
        let part = store_dir.join("parts.log");
        std::thread::spawn(move || {
            let started = Instant::now();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) && started.elapsed() < Duration::from_secs(8) {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&part)
                    .unwrap();
                std::io::Write::write_all(&mut f, b"part\n").unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let started = Instant::now();
    read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 1)
    });
    let took = started.elapsed();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    assert!(
        took < Duration::from_secs(4),
        "the session appeared only after {took:?}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn external_sessions_appear_on_store_change_without_keeping_harness_running_and_import_on_resume() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    let term_cwd = dir.0.join("project-a");
    std::fs::create_dir_all(&term_cwd).unwrap();

    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "list_page_size": 1,
            "models": [
                { "id": "stub/stub-1", "name": "Stub 1", "efforts": ["low", "high"] }
            ],
            "turns": [
                {
                    "match": "follow-up from drawer",
                    "steps": [{ "kind": "text", "text": "Continued in the same session." }]
                }
            ]
        }),
    );

    let mut lever = start_lever_with_discovery(&dir.0, &script, Some(&rpc_log), &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    let initial = read_frame(&mut watcher).unwrap();
    assert_eq!(initial["reply"], "list");
    assert_eq!(initial["threads"].as_array().unwrap().len(), 0);

    // Write an external session, a second external session (to exercise pagination),
    // and two subagent sessions that must never become threads.
    write_sessions_file(
        &sessions_file,
        &json!([
            {
                "session_id": "ses_term_1",
                "cwd": term_cwd.to_str().unwrap(),
                "title": "Terminal draft",
                "updated_at": "2026-04-10T12:00:00Z",
                "history": [
                    { "kind": "user_text", "text": "Check status in terminal" },
                    {
                        "kind": "act",
                        "id": "call_term_1",
                        "title": "git status",
                        "tool_kind": "execute",
                        "status": "in_progress"
                    },
                    {
                        "kind": "act_update",
                        "id": "call_term_1",
                        "status": "completed",
                        "content": "clean tree"
                    },
                    { "kind": "text", "text": "The working tree is clean." },
                    { "kind": "user_text", "text": "Inspect with a helper" },
                    {
                        "kind": "act",
                        "id": "call_sub_1",
                        "title": "subagent",
                        "tool_kind": "think",
                        "status": "pending",
                        "raw_input": { "description": "Helper", "prompt": "Look around" }
                    },
                    {
                        "kind": "act_update",
                        "id": "call_sub_1",
                        "status": "completed",
                        "content": "Helper done (sessionID: ses_term_child)"
                    },
                    { "kind": "text", "text": "Helper reported back." }
                ]
            },
            {
                "session_id": "ses_term_child",
                "cwd": term_cwd.to_str().unwrap(),
                "title": "Subagent session",
                "is_subagent": true,
                "history": [
                    { "kind": "user_text", "text": "Look around" },
                    { "kind": "text", "text": "Done." }
                ]
            },
            {
                "session_id": "ses_term_child_2",
                "cwd": term_cwd.to_str().unwrap(),
                "title": "Subagent with parent",
                "parent_session_id": "ses_term_1",
                "history": []
            },
            {
                "session_id": "ses_term_2",
                "cwd": "/tmp",
                "title": "Second terminal session",
                "updated_at": "2026-04-10T11:00:00Z",
                "history": [
                    { "kind": "user_text", "text": "Hello two" },
                    { "kind": "text", "text": "Reply two" }
                ]
            }
        ]),
    );

    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 2)
    });
    let threads = pushed["threads"].as_array().unwrap();
    let pushed_blob = serde_json::to_string(&pushed).unwrap();
    assert!(
        !pushed_blob.contains("ses_term_"),
        "harness session ID leaked: {pushed_blob}"
    );

    let term1 = threads
        .iter()
        .find(|t| t["cwd"] == term_cwd.to_str().unwrap())
        .expect("discovered thread in project-a");
    assert_eq!(term1["title"], "Terminal draft");
    assert_eq!(term1["state"], "idle");
    let term1_id = term1["thread"].as_str().unwrap().to_string();
    assert!(term1_id.starts_with("t-"), "expected Lever thread ID, got {term1_id}");

    // Discovery must not keep the harness process running.
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut peer).unwrap();
    assert_eq!(
        status["harness_started"], false,
        "discovery must not keep harness running: {status}"
    );

    // Updating the external session's title while unopened updates the list.
    let mut updated_sessions: Value = serde_json::from_slice(&std::fs::read(&sessions_file).unwrap()).unwrap();
    updated_sessions[0]["title"] = json!("Terminal renamed");
    updated_sessions[0]["updated_at"] = json!("2026-04-10T12:05:00Z");
    write_sessions_file(&sessions_file, &updated_sessions);
    let renamed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"].as_array().is_some_and(|ts| {
                ts.iter()
                    .any(|t| t["thread"] == term1_id && t["title"] == "Terminal renamed")
            })
    });
    assert!(renamed["threads"].as_array().unwrap().len() == 2);

    // Opening (`resume`) imports the full multi-turn history into Lever's record.
    let record = resume(&mut peer, &term1_id);
    let record_blob = serde_json::to_string(&record).unwrap();
    assert!(
        !record_blob.contains("ses_term_"),
        "harness session ID leaked into resumed record: {record_blob}"
    );
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "expected 2 imported turns: {record}");
    assert_eq!(turns[0]["message"], "Check status in terminal");
    assert_eq!(turns[0]["answer"], "The working tree is clean.");
    assert_eq!(turns[0]["state"], "done");
    assert_eq!(turns[0]["acts"].as_array().unwrap().len(), 1);
    assert_eq!(turns[0]["acts"][0]["title"], "git status");
    assert_eq!(turns[0]["acts"][0]["detail"], "clean tree");
    assert_eq!(turns[1]["message"], "Inspect with a helper");
    assert_eq!(turns[1]["answer"], "Helper reported back.");
    assert_eq!(turns[1]["subagents"].as_array().unwrap().len(), 1);
    assert_eq!(turns[1]["subagents"][0]["title"], "Helper");
    assert_eq!(turns[1]["subagents"][0]["messages"][0]["role"], "prompt");
    assert_eq!(turns[1]["subagents"][0]["messages"][0]["text"], "Look around");

    // Sending from Lever continues the SAME harness session as turn 3.
    let (sent_thread, sent_turn) = send(&mut peer, Some(&term1_id), "follow-up from drawer");
    assert_eq!(sent_thread, term1_id);
    assert_eq!(sent_turn, 3);
    let events = turn_events(&mut peer, &term1_id, sent_turn);
    assert_eq!(text_of(&events), "Continued in the same session.");
    assert_eq!(events.last().unwrap()["event"], "done");

    let rpc = std::fs::read_to_string(&rpc_log).unwrap();
    assert!(
        rpc.contains("\"method\":\"session/load\"") && rpc.contains("\"sessionId\":\"ses_term_1\""),
        "expected session/load on ses_term_1 in rpc log:\n{rpc}"
    );
    assert!(
        rpc.contains("\"method\":\"session/prompt\"") && rpc.contains("\"sessionId\":\"ses_term_1\""),
        "expected session/prompt on ses_term_1 in rpc log:\n{rpc}"
    );

    // After restarting Lever, re-opening the already-imported thread reads from
    // SQLite without starting the harness.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Remove sessions_file so startup doesn't run a scan; re-opening must still
    // serve the imported record from SQLite with harness_started == false.
    std::fs::remove_file(&sessions_file).unwrap();
    let mut lever = start_lever_with_discovery(&dir.0, &script, Some(&rpc_log), &store_dir);
    let mut peer = handshake(&lever.person);
    let reopened = resume(&mut peer, &term1_id);
    assert_eq!(reopened["turns"].as_array().unwrap().len(), 3, "{reopened}");
    assert_eq!(reopened["turns"][0]["message"], "Check status in terminal");
    assert_eq!(reopened["turns"][2]["answer"], "Continued in the same session.");
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut peer).unwrap();
    assert_eq!(
        status["harness_started"], false,
        "resuming imported record must not start harness"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn external_session_sent_directly_before_resume_imports_history_first() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let rpc_log = dir.0.join("rpc.jsonl");

    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "turns": [
                {
                    "match": "second message",
                    "steps": [{ "kind": "text", "text": "Second answer." }]
                }
            ]
        }),
    );

    let mut lever = start_lever_with_discovery(&dir.0, &script, Some(&rpc_log), &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    write_sessions_file(
        &sessions_file,
        &json!([
            {
                "session_id": "ses_direct_send",
                "cwd": "/tmp",
                "title": "Direct send test",
                "updated_at": "2026-04-10T12:00:00Z",
                "history": [
                    { "kind": "user_text", "text": "First message in terminal" },
                    { "kind": "text", "text": "First answer in terminal." }
                ]
            }
        ]),
    );

    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 1)
    });
    let tid = pushed["threads"][0]["thread"].as_str().unwrap().to_string();

    // Send directly without calling `resume` first: history is imported as turn 1
    // and this message runs as turn 2.
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let (sent_thread, sent_turn) = send(&mut peer, Some(&tid), "second message");
    assert_eq!(sent_thread, tid);
    assert_eq!(sent_turn, 2, "imported history should occupy turn 1");
    let events = turn_events(&mut peer, &tid, sent_turn);
    assert_eq!(text_of(&events), "Second answer.");

    let record = resume(&mut peer, &tid);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{record}");
    assert_eq!(turns[0]["message"], "First message in terminal");
    assert_eq!(turns[0]["answer"], "First answer in terminal.");
    assert_eq!(turns[1]["message"], "second message");
    assert_eq!(turns[1]["answer"], "Second answer.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// Lever with discovery, and one hand-started session listed as a thread
/// whose history the harness takes `load_delay_ms` to read. Returns Lever,
/// the thread, and the harness's RPC log.
fn one_discovered_thread(dir: &Path, extra_script: Value, history: Value) -> (RunningLever, String, PathBuf) {
    let store_dir = dir.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let rpc_log = dir.join("rpc.jsonl");
    let mut script = json!({
        "list_sessions": true,
        "sessions_file": sessions_file.to_str().unwrap(),
    });
    script
        .as_object_mut()
        .unwrap()
        .extend(extra_script.as_object().unwrap().clone());
    let script = write_script(dir, &script);
    let lever = start_lever_with_discovery(dir, &script, Some(&rpc_log), &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");
    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_hand_1",
            "cwd": "/tmp",
            "title": "Started by hand",
            "updated_at": "2026-04-10T12:00:00Z",
            "history": history
        }]),
    );
    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 1)
    });
    let tid = pushed["threads"][0]["thread"].as_str().unwrap().to_string();
    (lever, tid, rpc_log)
}

#[test]
fn a_send_to_an_unopened_thread_still_goes_when_its_client_leaves_while_history_is_read() {
    let dir = TempDir::new();
    let (mut lever, tid, _) = one_discovered_thread(
        &dir.0,
        json!({
            "load_delay_ms": 400,
            "turns": [{ "match": "sent then left", "steps": [{ "kind": "text", "text": "Went anyway." }] }]
        }),
        json!([
            { "kind": "user_text", "text": "First in terminal" },
            { "kind": "text", "text": "First answer." }
        ]),
    );

    // The client sends while the history is still being read, and leaves.
    let mut leaving = handshake(&lever.person);
    write_frame(
        &mut leaving,
        &json!({ "op": "send", "thread": tid, "text": "sent then left" }),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    drop(leaving);

    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = loop {
        let record = resume(&mut peer, &tid);
        if record["turns"].as_array().is_some_and(|t| t.len() == 2) && record["turns"][1]["state"] == "done" {
            break record;
        }
        assert!(Instant::now() < deadline, "the message never went: {record}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(record["turns"][0]["message"], "First in terminal");
    assert_eq!(record["turns"][1]["message"], "sent then left");
    assert_eq!(record["turns"][1]["answer"], "Went anyway.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_while_an_unopened_threads_history_is_read_drops_the_message_waiting_on_it() {
    let dir = TempDir::new();
    let (mut lever, tid, rpc_log) = one_discovered_thread(
        &dir.0,
        json!({
            "load_delay_ms": 400,
            "turns": [{ "steps": [{ "kind": "text", "text": "Must not run." }] }]
        }),
        json!([
            { "kind": "user_text", "text": "First in terminal" },
            { "kind": "text", "text": "First answer." }
        ]),
    );

    let mut sender = handshake(&lever.person);
    sender.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(
        &mut sender,
        &json!({ "op": "send", "thread": tid, "text": "then stopped" }),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let mut stopper = handshake(&lever.person);
    stopper.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut stopper, &json!({ "op": "stop", "thread": tid })).unwrap();
    assert_eq!(read_frame(&mut stopper).unwrap()["reply"], "ok");

    // Once the history is read, the message is answered, and not sent.
    let reply = read_until(&mut sender, |f| f.get("reply").is_some());
    assert_eq!(reply["reply"], "error", "{reply}");
    assert_eq!(reply["code"], "working", "{reply}");

    let record = resume(&mut stopper, &tid);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1, "{record}");
    assert_eq!(turns[0]["message"], "First in terminal");
    let rpc = std::fs::read_to_string(&rpc_log).unwrap();
    assert!(
        !rpc.contains("\"method\":\"session/prompt\""),
        "a stopped message reached the harness:\n{rpc}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn opening_a_thread_reads_the_subagents_an_acp_harness_replays() {
    let dir = TempDir::new();
    let (mut lever, tid, _) = one_discovered_thread(
        &dir.0,
        json!({}),
        json!([
            { "kind": "user_text", "text": "Use two helpers" },
            { "kind": "unknown", "update": {
                "sessionUpdate": "subagent_update",
                "sessionId": "ses_hand_child_1",
                "title": "Reader",
                "state": { "state": "running" }
            } },
            { "kind": "unknown", "update": {
                "sessionUpdate": "session_message",
                "messageId": "msg-child-1",
                "senderSessionId": "ses_hand_1",
                "recipientSessionId": "ses_hand_child_1",
                "content": [{ "type": "text", "text": "Read the docs" }]
            } },
            { "kind": "unknown", "update": {
                "sessionUpdate": "subagent_update",
                "sessionId": "ses_hand_child_2",
                "title": "Writer",
                "state": { "state": "running" }
            } },
            { "kind": "unknown", "update": {
                "sessionUpdate": "subagent_update",
                "sessionId": "ses_hand_child_1",
                "state": { "state": "idle", "stopReason": "end_turn" }
            } },
            { "kind": "unknown", "update": {
                "sessionUpdate": "subagent_update",
                "sessionId": "ses_hand_child_2",
                "state": { "state": "idle", "stopReason": "error", "error": { "code": -32000, "message": "ses_hand_child_2 broke" } }
            } },
            { "kind": "text", "text": "Both reported." }
        ]),
    );

    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let record = resume(&mut peer, &tid);
    let blob = serde_json::to_string(&record).unwrap();
    assert!(
        !blob.contains("ses_hand"),
        "a harness session ID crossed Lever's socket: {blob}"
    );
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1, "{record}");
    assert_eq!(turns[0]["answer"], "Both reported.");
    let subagents = turns[0]["subagents"].as_array().unwrap();
    assert_eq!(subagents.len(), 2, "{record}");
    assert_eq!(subagents[0]["title"], "Reader");
    assert_eq!(subagents[0]["state"], "done");
    assert_eq!(subagents[0]["messages"][0]["role"], "prompt");
    assert_eq!(subagents[0]["messages"][0]["text"], "Read the docs");
    assert_eq!(subagents[1]["title"], "Writer");
    assert_eq!(subagents[1]["state"], "failed");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn lever_started_session_and_its_subagent_are_never_duplicated_by_discovery() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let rpc_log = dir.0.join("rpc.jsonl");

    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "turns": [
                {
                    "match": "start from lever",
                    "steps": [
                        {
                            "kind": "subagent_start",
                            "id": "ses_lever_sub_1",
                            "title": "Child",
                            "prompt": "Work"
                        },
                        {
                            "kind": "subagent_end",
                            "subagent": "ses_lever_sub_1",
                            "status": "completed"
                        },
                        { "kind": "text", "text": "Parent finished." }
                    ]
                }
            ]
        }),
    );

    let mut lever = start_lever_with_discovery(&dir.0, &script, Some(&rpc_log), &store_dir);
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    // Start a thread from Lever itself.
    let (lever_tid, turn) = send(&mut peer, None, "start from lever");
    let events = turn_events(&mut peer, &lever_tid, turn);
    assert_eq!(events.last().unwrap()["event"], "done");

    // Find the harness session ID that stub-harness assigned to Lever's thread.
    let lever_sid = std::fs::read_to_string(&rpc_log)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|v| {
            if v["method"] == "session/prompt" {
                v["params"]["sessionId"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .expect("harness session ID for Lever thread");

    // Now have the harness list Lever's own session, Lever's subagent session
    // (even WITHOUT any is_subagent marker!), and one genuinely external session.
    write_sessions_file(
        &sessions_file,
        &json!([
            {
                "session_id": lever_sid,
                "cwd": "/tmp",
                "title": "Duplicate of Lever session",
                "updated_at": "2026-04-10T12:00:00Z"
            },
            {
                "session_id": "ses_lever_sub_1",
                "cwd": "/tmp",
                "title": "Lever subagent without marker",
                "updated_at": "2026-04-10T12:01:00Z"
            },
            {
                "session_id": "ses_genuinely_external",
                "cwd": "/tmp",
                "title": "Started in terminal",
                "updated_at": "2026-04-10T12:02:00Z",
                "history": [
                    { "kind": "user_text", "text": "Hi" },
                    { "kind": "text", "text": "Hello" }
                ]
            }
        ]),
    );

    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Started in terminal"))
    });
    let threads = pushed["threads"].as_array().unwrap();
    assert_eq!(
        threads.len(),
        2,
        "only Lever's original thread and the one external session should be listed: {threads:?}"
    );
    assert!(threads.iter().any(|t| t["thread"] == lever_tid));
    assert!(threads.iter().any(|t| t["title"] == "Started in terminal"));
    assert!(!threads.iter().any(|t| t["title"] == "Lever subagent without marker"));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn external_session_load_failure_is_retryable_and_delete_does_not_resurrect() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");

    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "delete_session": false,
            "load_session_error": { "code": -32000, "message": "Session not found: ses_flaky_load" },
            "turns": []
        }),
    );

    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    write_sessions_file(
        &sessions_file,
        &json!([
            {
                "session_id": "ses_flaky_load",
                "cwd": "/tmp",
                "title": "Flaky load",
                "updated_at": "2026-04-10T12:00:00Z",
                "history": [
                    { "kind": "user_text", "text": "Hello" },
                    { "kind": "text", "text": "World" }
                ]
            }
        ]),
    );

    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 1)
    });
    let tid = pushed["threads"][0]["thread"].as_str().unwrap().to_string();

    // First resume fails cleanly and keeps the thread eligible to retry.
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut peer, &json!({ "op": "resume", "thread": tid })).unwrap();
    let err = read_frame(&mut peer).unwrap();
    assert_eq!(err["reply"], "error", "{err}");
    assert_eq!(err["code"], "harness-error", "{err}");
    assert!(
        !err.to_string().contains("ses_flaky_load"),
        "harness session ID leaked: {err}"
    );

    // Restart Lever with a working script (no load_session_error) and retry resume.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let working_script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "delete_session": false,
            "turns": []
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &working_script, None, &store_dir);
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let record = resume(&mut peer, &tid);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert_eq!(record["turns"][0]["answer"], "World");

    // Deleting when harness does not support delete_session returns a warning
    // and records the session as ignored so later store changes do not resurrect it.
    write_frame(&mut peer, &json!({ "op": "delete", "thread": tid })).unwrap();
    let deleted = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(deleted["reply"], "deleted", "{deleted}");
    assert!(deleted.get("warning").is_some(), "{deleted}");

    // Add a second session to trigger another discovery scan and verify the
    // deleted session stays gone while the new one appears.
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    write_sessions_file(
        &sessions_file,
        &json!([
            {
                "session_id": "ses_flaky_load",
                "cwd": "/tmp",
                "title": "Flaky load",
                "updated_at": "2026-04-10T12:00:00Z"
            },
            {
                "session_id": "ses_after_delete",
                "cwd": "/tmp",
                "title": "Fresh session",
                "updated_at": "2026-04-10T12:05:00Z"
            }
        ]),
    );

    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Fresh session"))
    });
    let threads = pushed["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1, "deleted session must not resurrect: {threads:?}");
    assert_eq!(threads[0]["title"], "Fresh session");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_the_harness_deleted_stays_deleted_and_is_not_opened_while_deleting() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    let sessions_file = store_dir.join("sessions.json");
    let session = |id: &str, title: &str| {
        json!({
            "session_id": id,
            "cwd": "/tmp",
            "title": title,
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        })
    };
    write_sessions_file(
        &sessions_file,
        &json!([session("ses_gone", "Gone"), session("ses_slow", "Slow")]),
    );
    // The harness deletes its copy, but a listing taken before that still
    // shows it: the stub's list keeps every session in its file.
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "delete_session": true,
            "delete_delay_ms": 600,
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    let initial = read_frame(&mut watcher).unwrap();
    let listed = if initial["threads"].as_array().is_some_and(|t| t.len() == 2) {
        initial
    } else {
        read_until(&mut watcher, |f| {
            f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 2)
        })
    };
    let id_of = |title: &str| {
        listed["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == title)
            .unwrap()["thread"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (gone, slow) = (id_of("Gone"), id_of("Slow"));

    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut peer, &json!({ "op": "delete", "thread": gone })).unwrap();
    // While the harness deletes it, the thread is not opened from its session.
    let mut other = handshake(&lever.person);
    other.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut other, &json!({ "op": "resume", "thread": gone })).unwrap();
    let refused = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(refused["code"], "working", "{refused}");
    let deleted = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(deleted["reply"], "deleted", "{deleted}");
    assert!(deleted["warning"].is_null(), "{deleted}");

    // The next listing still has it, and it does not come back.
    let mut sessions: Value = serde_json::from_slice(&std::fs::read(&sessions_file).unwrap()).unwrap();
    sessions.as_array_mut().unwrap().push(session("ses_new", "New"));
    write_sessions_file(&sessions_file, &sessions);
    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "New"))
    });
    let titles: Vec<&str> = pushed["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["title"].as_str())
        .collect();
    assert!(!titles.contains(&"Gone"), "a deleted thread came back: {titles:?}");
    assert!(
        pushed["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["thread"] == slow)
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_lever_opened_to_read_the_models_is_never_a_thread() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "session_prefix": "ses_lever_",
            "models": [{ "id": "stub/stub-1", "name": "Stub 1" }],
        }),
    );
    let mut extra: Vec<String> = [
        "--harness",
        STUB_BIN,
        "--harness-name",
        "Stub",
        "--harness-arg",
        "--script",
        "--harness-arg",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    extra.push(script.to_str().unwrap().into());
    for arg in [
        "--harness-watch",
        store_dir.to_str().unwrap(),
        "--discovery-debounce-ms",
        "40",
        "--idle-timeout-ms",
        "300",
    ] {
        extra.push(arg.into());
    }
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(models["reply"], "models", "{models}");

    // Once the harness that opened it is gone, a listing still shows it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
        let status = read_until(&mut peer, |f| f["reply"] == "status");
        if status["harness_started"] == false {
            break;
        }
        assert!(Instant::now() < deadline, "the harness never idled out: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");
    write_sessions_file(
        &sessions_file,
        &json!([
            { "session_id": "ses_lever_1", "cwd": "/tmp", "title": "New session" },
            {
                "session_id": "ses_hand",
                "cwd": "/tmp",
                "title": "By hand",
                "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
            }
        ]),
    );
    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "By hand"))
    });
    assert_eq!(pushed["threads"].as_array().unwrap().len(), 1, "{pushed}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_opened_hand_started_thread_keeps_its_mode_and_the_persons_chosen_effort() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    let sessions_file = store_dir.join("sessions.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    let session = |id: &str, title: &str| {
        json!({
            "session_id": id,
            "cwd": "/tmp",
            "title": title,
            "history": [{ "kind": "user_text", "text": "plan it" }, { "kind": "text", "text": "A plan." }]
        })
    };
    write_sessions_file(
        &sessions_file,
        &json!([session("ses_plan", "Planned"), session("ses_effort", "Effort")]),
    );
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "modes": true,
            "load_mode": "plan",
            "models": [{ "id": "stub/stub-1", "name": "Stub 1", "efforts": ["low", "high"] }],
            "turns": [{ "match": "go on", "steps": [{ "kind": "text", "text": "Going on." }] }]
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, Some(&rpc_log), &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    let initial = read_frame(&mut watcher).unwrap();
    let listed = if initial["threads"].as_array().is_some_and(|t| t.len() == 2) {
        initial
    } else {
        read_until(&mut watcher, |f| {
            f["event"] == "threads" && f["threads"].as_array().is_some_and(|t| t.len() == 2)
        })
    };
    let id_of = |title: &str| {
        listed["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == title)
            .unwrap()["thread"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (planned, effort) = (id_of("Planned"), id_of("Effort"));
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // Opened, it keeps the mode it was listed with, though the harness
    // reports its own default for the loaded session.
    let listed_mode = listed["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["thread"] == planned)
        .unwrap()["mode"]
        .clone();
    let record = resume(&mut peer, &planned);
    assert_eq!(record["mode"], listed_mode, "{record}");
    let (_, turn) = send(&mut peer, Some(&planned), "go on");
    let events = turn_events(&mut peer, &planned, turn);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");

    // An effort chosen before it is opened outlives the import.
    write_frame(
        &mut peer,
        &json!({ "op": "set-effort", "thread": effort, "effort": "high" }),
    )
    .unwrap();
    let set = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(set["reply"], "effort-set", "{set}");
    let record = resume(&mut peer, &effort);
    assert_eq!(record["effort"], "high", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_running_harness_that_never_answers_a_listing_does_not_stall_discovery() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "withhold_list_after_prompt": true,
            "turns": [{ "match": "hello", "steps": [{ "kind": "text", "text": "Hi." }] }]
        }),
    );
    let mut extra: Vec<String> = [
        "--harness",
        STUB_BIN,
        "--harness-name",
        "Stub",
        "--harness-arg",
        "--script",
        "--harness-arg",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    extra.push(script.to_str().unwrap().into());
    for arg in [
        "--harness-watch",
        store_dir.to_str().unwrap(),
        "--discovery-debounce-ms",
        "40",
        "--discovery-timeout-ms",
        "300",
    ] {
        extra.push(arg.into());
    }
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // The harness runs, and has taken a prompt.
    let (thread, turn) = send(&mut peer, None, "hello");
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "done");

    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");
    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_hand",
            "cwd": "/tmp",
            "title": "By hand",
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        }]),
    );
    // The running harness never answers; a one-shot listing does.
    read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "By hand"))
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_found_empty_is_no_thread_until_it_changes() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let script = write_script(
        &dir.0,
        &json!({ "list_sessions": true, "sessions_file": sessions_file.to_str().unwrap() }),
    );
    // A pass before this one found it empty, as OpenCode's export does.
    let lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut lever = {
        let mut lever = lever;
        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
        let db = rusqlite::Connection::open(dir.0.join("state").join("lever.sqlite")).unwrap();
        db.execute(
            "INSERT INTO quiet_sessions (harness_session_id, stamp) VALUES ('ses_quiet', '2026-04-10T12:00:00.100Z')",
            [],
        )
        .unwrap();
        start_lever_with_discovery(&dir.0, &script, None, &store_dir)
    };
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");
    let session = |id: &str, title: &str, stamp: &str| {
        json!({
            "session_id": id,
            "cwd": "/tmp",
            "title": title,
            "updated_at": stamp,
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        })
    };
    write_sessions_file(
        &sessions_file,
        &json!([
            session("ses_quiet", "Quiet", "2026-04-10T12:00:00.100Z"),
            session("ses_real", "Real", "2026-04-10T12:00:01Z")
        ]),
    );
    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Real"))
    });
    let titles: Vec<&str> = pushed["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["title"].as_str())
        .collect();
    assert_eq!(titles, ["Real"], "a session found empty was made a thread");

    // Once it changes, it is looked at again.
    write_sessions_file(
        &sessions_file,
        &json!([
            session("ses_quiet", "Quiet", "2026-04-10T12:00:00.500Z"),
            session("ses_real", "Real", "2026-04-10T12:00:01Z")
        ]),
    );
    read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Quiet"))
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_that_cannot_be_written_is_listed_again_later_each_time() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let starts = dir.0.join("starts.log");
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "touch_on_start": starts.to_str().unwrap(),
        }),
    );
    // Lever's database reads, but will not take a new thread.
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let db = rusqlite::Connection::open(dir.0.join("state").join("lever.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER refused BEFORE INSERT ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END;")
        .unwrap();
    drop(db);
    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_hand",
            "cwd": "/tmp",
            "title": "By hand",
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        }]),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    std::thread::sleep(Duration::from_millis(6500));
    // Tried at once, then after 1, 2 and 4 s: not every second.
    let tried = count_lines(&starts);
    assert!((2..=4).contains(&tried), "listed {tried} times in 6.5 s");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_that_could_not_be_written_waits_through_a_failed_listing() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("sessions.json");
    let starts = dir.0.join("starts.log");
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "touch_on_start": starts.to_str().unwrap(),
            "fail_list_on_start": 2,
        }),
    );
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let db_path = dir.0.join("state").join("lever.sqlite");
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch("CREATE TRIGGER refused BEFORE INSERT ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END;")
        .unwrap();
    write_sessions_file(
        &sessions_file,
        &json!([{
            "session_id": "ses_hand",
            "cwd": "/tmp",
            "title": "By hand",
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        }]),
    );
    // Its thread cannot be written; the pass that tries again cannot list;
    // then both are fine again, and nothing writes to the store.
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");
    let deadline = Instant::now() + Duration::from_secs(5);
    while count_lines(&starts) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(count_lines(&starts), 2, "it listed again after the write failed");
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute_batch("DROP TRIGGER refused")
        .unwrap();
    read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "By hand"))
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// Runs Lever's OpenCode path against the stub with `export` as the
/// session's export and `refuse` creating a trigger in Lever's database,
/// and returns how often the session was exported in 6.5 s.
fn exports_while_lever_cannot_write(export: &str, refuse: &str) -> usize {
    let dir = TempDir::new();
    // The stub stands in for OpenCode: its store is OpenCode's, it writes to
    // it as it starts, and it answers `session export` as OpenCode does.
    let store_dir = dir.0.join("data").join("opencode");
    std::fs::create_dir_all(&store_dir).unwrap();
    let sessions_file = store_dir.join("opencode.db");
    let wal = store_dir.join("opencode.db-wal");
    let exports = dir.0.join("exports");
    std::fs::create_dir_all(&exports).unwrap();
    std::fs::write(exports.join("export-ses_hand.json"), export).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "touch_on_start": wal.to_str().unwrap(),
        }),
    );
    let extra: Vec<String> = [
        "--harness",
        STUB_BIN,
        "--harness-name",
        "OpenCode",
        "--harness-arg",
        "--script",
        "--harness-arg",
        script.to_str().unwrap(),
        "--harness-env",
        &format!("XDG_DATA_HOME={}", dir.0.join("data").display()),
        "--harness-env",
        &format!("STUB_EXPORT_DIR={}", exports.display()),
        "--harness-env",
        &format!("STUB_TOUCH={}", wal.display()),
        "--discovery-debounce-ms",
        "40",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Lever's database reads, but will not take a new thread.
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    rusqlite::Connection::open(dir.0.join("state").join("lever.sqlite"))
        .unwrap()
        .execute_batch(refuse)
        .unwrap();
    std::fs::write(
        &sessions_file,
        serde_json::to_vec(&json!([{
            "session_id": "ses_hand",
            "cwd": "/tmp",
            "title": "By hand",
            "updated_at": "2026-04-10T12:00:00.100Z",
            "history": [{ "kind": "user_text", "text": "hi" }, { "kind": "text", "text": "hello" }]
        }]))
        .unwrap(),
    )
    .unwrap();
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    std::thread::sleep(Duration::from_millis(6500));
    let exported = count_lines(&exports.join("exports.log"));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    exported
}

#[test]
fn an_opencode_session_that_cannot_be_written_is_not_exported_again_and_again() {
    let exported = exports_while_lever_cannot_write(
        r#"{"info":{"id":"ses_hand","title":"By hand"},"messages":[{}]}"#,
        "CREATE TRIGGER refused BEFORE INSERT ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    );
    assert!((1..=8).contains(&exported), "exported {exported} times in 6.5 s");
}

#[test]
fn an_empty_opencode_session_whose_mark_cannot_be_written_is_not_exported_again_and_again() {
    let exported = exports_while_lever_cannot_write(
        r#"{"info":{"id":"ses_hand"},"messages":[]}"#,
        "CREATE TRIGGER refused BEFORE INSERT ON quiet_sessions BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    );
    assert!((1..=8).contains(&exported), "exported {exported} times in 6.5 s");
}

#[test]
fn a_harness_that_cannot_list_brings_no_more_passes_for_its_own_writes() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("harness-store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let starts = store_dir.join("starts.log");
    // It writes to its store as it starts, and offers no `session/list`.
    let script = write_script(&dir.0, &json!({ "touch_on_start": starts.to_str().unwrap() }));
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    std::fs::write(store_dir.join("changed"), "x").unwrap();
    std::thread::sleep(Duration::from_millis(7000));
    let started = count_lines(&starts);
    assert!((1..=2).contains(&started), "started {started} times for one change");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn missing_store_directory_is_promoted_when_created_later() {
    let dir = TempDir::new();
    let store_dir = dir.0.join("nested").join("store");
    let sessions_file = store_dir.join("sessions.json");

    let script = write_script(
        &dir.0,
        &json!({
            "list_sessions": true,
            "sessions_file": sessions_file.to_str().unwrap(),
            "turns": []
        }),
    );

    // Start Lever BEFORE `nested/store` exists.
    let mut lever = start_lever_with_discovery(&dir.0, &script, None, &store_dir);
    let mut peer = handshake(&lever.person);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    // Create the nested store directory and write a session file.
    write_sessions_file(
        &sessions_file,
        &json!([
            {
                "session_id": "ses_late_dir",
                "cwd": "/tmp",
                "title": "Created later",
                "updated_at": "2026-04-10T12:00:00Z"
            }
        ]),
    );

    let pushed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["title"] == "Created later"))
    });
    assert_eq!(pushed["threads"].as_array().unwrap().len(), 1);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_becomes_a_project_by_control_and_continues_in_the_new_folder_with_history() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "third turn",
                    "steps": [{ "kind": "text", "text": "THIRD_CWD={cwd}\nTHIRD_PROMPT={prompt}" }]
                },
                {
                    "match": "check after project",
                    "steps": [{ "kind": "text", "text": "CWD={cwd}\nPROMPT={prompt}" }]
                },
                {
                    "match": "PRIOR_CONTEXT_ALPHA",
                    "steps": [{ "kind": "text", "text": "Initial answer in {cwd}" }]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    let mut watcher = handshake(&lever.surface);
    watcher.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut watcher, &json!({ "op": "list", "watch": true })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "list");

    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // Turn 1 starts in $HOME.
    write_frame(
        &mut client,
        &json!({ "op": "send", "text": "Remember PRIOR_CONTEXT_ALPHA" }),
    )
    .unwrap();
    let sent = read_frame(&mut client).unwrap();
    assert_eq!(sent["reply"], "sent");
    let tid = sent["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

    // Make this thread a project using default ~/Projects/<name>.
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "alpha" }),
    )
    .unwrap();
    let made = read_frame(&mut client).unwrap();
    let expected = home.join("Projects").join("alpha");
    let expected_str = expected.to_str().unwrap();
    assert_eq!(made["reply"], "project-made");
    assert_eq!(made["thread"], tid);
    assert_eq!(made["cwd"], expected_str);
    assert_eq!(made["created"], true);
    assert!(expected.is_dir());

    // Subscribed peer receives `thread-updated` with new cwd, and list watcher sees it under the project folder.
    let updated = read_until(&mut client, |f| {
        f["event"] == "thread-updated" && f["cwd"] == expected_str
    });
    assert_eq!(updated["thread"], tid);
    let listed = read_until(&mut watcher, |f| {
        f["event"] == "threads"
            && f["threads"]
                .as_array()
                .is_some_and(|ts| ts.len() == 1 && ts[0]["thread"] == tid && ts[0]["cwd"] == expected_str)
    });
    assert_eq!(listed["threads"].as_array().unwrap().len(), 1);

    // Turn 2 opens a new session in the project folder and receives `<thread-history>` containing Turn 1.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "check after project" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut client).unwrap()["reply"], "sent");
    let mut turn2_text = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            turn2_text.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 2 {
            break;
        }
    }
    assert!(turn2_text.contains(&format!("CWD={expected_str}")), "got: {turn2_text}");
    assert!(turn2_text.contains("<thread-history>"), "got: {turn2_text}");
    assert!(turn2_text.contains("PRIOR_CONTEXT_ALPHA"), "got: {turn2_text}");

    // Turn 3 continues in the same new session without re-injecting `<thread-history>`.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "third turn" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut client).unwrap()["reply"], "sent");
    let mut turn3_text = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            turn3_text.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 3 {
            break;
        }
    }
    assert!(
        turn3_text.contains(&format!("THIRD_CWD={expected_str}")),
        "got: {turn3_text}"
    );
    assert!(!turn3_text.contains("<thread-history>"), "got: {turn3_text}");

    // Lever's record stays one thread with all 3 turns and the project's cwd.
    write_frame(&mut client, &json!({ "op": "resume", "thread": tid })).unwrap();
    let resumed = read_frame(&mut client).unwrap();
    assert_eq!(resumed["reply"], "resumed");
    assert_eq!(resumed["cwd"], expected_str);
    assert_eq!(resumed["turns"].as_array().unwrap().len(), 3);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}
