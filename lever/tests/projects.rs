//! Project promotion (make-project), list-dirs, thread-history handoff, retired-session deletion, and MCP routing.

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
fn making_a_project_uses_an_existing_folder_as_is_and_refuses_an_uncreatable_place() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    let existing = home.join("ExistingWork");
    std::fs::create_dir_all(&existing).unwrap();
    let marker = existing.join("README.txt");
    std::fs::write(&marker, "keep me intact").unwrap();
    let blocker = home.join("regular-file");
    std::fs::write(&blocker, "not a directory").unwrap();

    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "hold", "stall": true, "steps": [{ "kind": "text", "text": "holding" }] },
                { "match": "start", "steps": [{ "kind": "text", "text": "ok" }] }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    write_frame(&mut client, &json!({ "op": "send", "text": "start" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done");

    // Existing folder via explicit `path` is used as-is and nothing inside changes.
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "path": "~/ExistingWork" }),
    )
    .unwrap();
    let made = read_frame(&mut client).unwrap();
    assert_eq!(made["reply"], "project-made");
    assert_eq!(made["cwd"], existing.to_str().unwrap());
    assert_eq!(made["created"], false);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep me intact");
    assert_eq!(std::fs::read_dir(&existing).unwrap().count(), 1);
    let _ = read_until(&mut client, |f| f["event"] == "thread-updated");

    // Uncreatable place (blocked by a regular file) fails cleanly and keeps thread cwd unchanged.
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "sub", "place": "~/regular-file" }),
    )
    .unwrap();
    let err = read_frame(&mut client).unwrap();
    assert_eq!(err["reply"], "error");
    assert_eq!(err["code"], "project-error");

    // Existing regular file as exact path also fails cleanly.
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "path": "~/regular-file" }),
    )
    .unwrap();
    let err2 = read_frame(&mut client).unwrap();
    assert_eq!(err2["reply"], "error");
    assert_eq!(err2["code"], "project-error");

    // Invalid project names fail with invalid-request.
    for bad in ["", "a/b", ".."] {
        write_frame(
            &mut client,
            &json!({ "op": "make-project", "thread": tid, "name": bad }),
        )
        .unwrap();
        let bad_reply = read_frame(&mut client).unwrap();
        assert_eq!(bad_reply["reply"], "error");
        assert_eq!(bad_reply["code"], "invalid-request");
    }

    // Thread still points at ~/ExistingWork.
    write_frame(&mut client, &json!({ "op": "list" })).unwrap();
    let list = read_frame(&mut client).unwrap();
    assert_eq!(list["threads"][0]["cwd"], existing.to_str().unwrap());

    // Control call refused while a turn is working.
    write_frame(&mut client, &json!({ "op": "send", "thread": tid, "text": "hold" })).unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let _ = read_until(&mut client, |f| f["event"] == "text");
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "busy" }),
    )
    .unwrap();
    let busy_reply = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(busy_reply["reply"], "error");
    assert_eq!(busy_reply["code"], "working");

    write_frame(&mut client, &json!({ "op": "stop", "thread": tid })).unwrap();
    let _ = read_until(&mut client, |f| f["event"] == "stopped");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_becomes_a_project_by_the_mcp_tool_respecting_mode_and_named_place() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [
                {
                    "match": "where am i",
                    "steps": [
                        { "kind": "text", "text": "NOW_IN={cwd}\nHIST={prompt}" }
                    ]
                },
                {
                    "match": "make gamma",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_proj_gamma",
                            "tool": "make_project",
                            "arguments": { "name": "gamma" },
                            "reply": "GAMMA={mcp_result}"
                        }
                    ]
                },
                {
                    "match": "make beta in ~/Code",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_proj_beta",
                            "tool": "make_project",
                            "arguments": { "name": "beta", "place": "~/Code" },
                            "reply": "MCP={mcp_result}"
                        }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Turn 1 in Ask first: MCP call raises a permission request; denying it creates nothing.
    write_frame(&mut client, &json!({ "op": "send", "text": "make beta in ~/Code" })).unwrap();
    let sent1 = read_frame(&mut client).unwrap();
    let tid = sent1["thread"].as_str().unwrap().to_string();
    let req1 = read_until(&mut client, |f| f["event"] == "request" && f["state"] == "waiting");
    assert_eq!(req1["detail"], "~/Code/beta");
    let req1_id = req1["request"].as_str().unwrap().to_string();

    write_frame(
        &mut client,
        &json!({ "op": "answer", "thread": tid, "request": req1_id, "answer": "no" }),
    )
    .unwrap();
    let _ = read_until(&mut client, |f| f["event"] == "stopped" || f["event"] == "done");
    let beta_path = home.join("Code").join("beta");
    assert!(!beta_path.exists(), "denied MCP call must not create project folder");

    // Turn 2 in Ask first: answering `once` runs the real `crowbar-lever mcp-bridge` over stdio.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "make beta in ~/Code (approved)" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut client).unwrap()["reply"], "sent");
    let req2 = read_until(&mut client, |f| f["event"] == "request" && f["state"] == "waiting");
    let req2_id = req2["request"].as_str().unwrap().to_string();
    write_frame(
        &mut client,
        &json!({ "op": "answer", "thread": tid, "request": req2_id, "answer": "once" }),
    )
    .unwrap();

    let mut saw_updated = false;
    let mut saw_mcp_text = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "thread-updated" && f["cwd"] == beta_path.to_str().unwrap() {
            saw_updated = true;
        }
        if f["event"] == "text" {
            saw_mcp_text.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 2 {
            break;
        }
    }
    assert!(saw_updated, "expected thread-updated with ~/Code/beta");
    assert!(beta_path.is_dir(), "approved MCP call must create ~/Code/beta");
    assert!(
        saw_mcp_text.contains(beta_path.to_str().unwrap()),
        "got: {saw_mcp_text}"
    );

    // Turn 3 runs in a fresh session in ~/Code/beta and receives `<thread-history>` from before.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "where am i" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut client).unwrap()["reply"], "sent");
    let mut turn3 = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            turn3.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 3 {
            break;
        }
    }
    assert!(
        turn3.contains(&format!("NOW_IN={}", beta_path.display())),
        "got: {turn3}"
    );
    assert!(turn3.contains("<thread-history>"), "got: {turn3}");
    assert!(turn3.contains("make beta in ~/Code"), "got: {turn3}");

    // Second thread in full-access: MCP call runs with no permission request.
    write_frame(
        &mut client,
        &json!({ "op": "send", "text": "make gamma", "mode": "full-access" }),
    )
    .unwrap();
    let sent_g = read_frame(&mut client).unwrap();
    let tid_g = sent_g["thread"].as_str().unwrap().to_string();
    let mut saw_request_in_full_access = false;
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "request" {
            saw_request_in_full_access = true;
        }
        if f["event"] == "done" && f["thread"] == tid_g {
            break;
        }
    }
    assert!(!saw_request_in_full_access);
    assert!(home.join("Projects").join("gamma").is_dir());

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn mcp_tool_routes_correctly_for_both_per_session_and_shared_cwd_harnesses() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "warmup", "steps": [{ "kind": "text", "text": "ready" }] },
                {
                    "match": "shared-mcp-call",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_shared",
                            "tool": "make_project",
                            "arguments": { "name": "from-shared" },
                            "shared_by_cwd": true,
                            "send_meta_session": true,
                            "reply": "SHARED={mcp_result}"
                        }
                    ]
                },
                {
                    "match": "token-mcp-call",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_token",
                            "tool": "make_project",
                            "arguments": { "name": "from-token" },
                            "shared_by_cwd": false,
                            "send_meta_session": false,
                            "reply": "TOKEN={mcp_result}"
                        }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Start two threads in the same folder ($HOME): T1 first, then T2 (overwriting `cwd_mcps[$HOME]`).
    write_frame(&mut client, &json!({ "op": "send", "text": "warmup t1" })).unwrap();
    let t1 = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t1);

    write_frame(&mut client, &json!({ "op": "send", "text": "warmup t2" })).unwrap();
    let t2 = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t2);

    // T1 calls the shared-per-cwd MCP bridge (which carries T2's env token) with T1's `_meta` session ID.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": t1, "text": "shared-mcp-call" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut client).unwrap()["reply"], "sent");
    let _ = read_until(&mut client, |f| {
        f["event"] == "done" && f["thread"] == t1 && f["turn"] == 2
    });

    // T2 calls its per-session MCP bridge with NO `_meta` session ID (as Codex/Claude/Pi/Antigravity do).
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": t2, "text": "token-mcp-call" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut client).unwrap()["reply"], "sent");
    let _ = read_until(&mut client, |f| {
        f["event"] == "done" && f["thread"] == t2 && f["turn"] == 2
    });

    // Verify each thread moved to its own project folder, with zero cross-talk.
    write_frame(&mut client, &json!({ "op": "list" })).unwrap();
    let list = read_frame(&mut client).unwrap();
    let threads = list["threads"].as_array().unwrap();
    let t1_row = threads.iter().find(|r| r["thread"] == t1).unwrap();
    let t2_row = threads.iter().find(|r| r["thread"] == t2).unwrap();
    assert_eq!(
        t1_row["cwd"],
        home.join("Projects").join("from-shared").to_str().unwrap()
    );
    assert_eq!(
        t2_row["cwd"],
        home.join("Projects").join("from-token").to_str().unwrap()
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn late_session_reply_after_stop_and_make_project_does_not_rebind_old_folder() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "new_session_delay_ms": 350,
            "turns": [
                { "steps": [{ "kind": "text", "text": "CWD={cwd}" }] }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Start turn 1 so `session/new` in `$HOME` is in flight, then stop before it answers.
    write_frame(&mut client, &json!({ "op": "send", "text": "first" })).unwrap();
    let sent1 = read_frame(&mut client).unwrap();
    assert_eq!(sent1["reply"], "sent");
    let tid = sent1["thread"].as_str().unwrap().to_string();

    write_frame(&mut client, &json!({ "op": "stop", "thread": tid })).unwrap();
    let _ = read_until(&mut client, |f| f["event"] == "stopped");

    // Move the thread while the old `$HOME` `session/new` RPC is still in flight.
    let target = home.join("Projects").join("late-move");
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "late-move" }),
    )
    .unwrap();
    let made = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(made["reply"], "project-made");
    assert_eq!(made["cwd"], target.to_str().unwrap());

    // Let the stale old-folder `session/new` reply arrive and be discarded.
    std::thread::sleep(Duration::from_millis(500));

    // Turn 2 must open a fresh session in the project folder, not inherit the stale `$HOME` session.
    write_frame(&mut client, &json!({ "op": "send", "thread": tid, "text": "second" })).unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut answer = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            answer.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 2 {
            break;
        }
    }
    assert_eq!(answer, format!("CWD={}", target.display()));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn handoff_survives_harness_crash_before_first_post_move_prompt_is_accepted() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "after-crash",
                    "steps": [{ "kind": "text", "text": "CWD={cwd}\nPROMPT={prompt}" }]
                },
                {
                    "match": "crash-before-answer",
                    "steps": [],
                    "crash": true
                },
                {
                    "match": "seed-before-move",
                    "steps": [{ "kind": "text", "text": "SEED_CRASH_MEMORY" }]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    write_frame(&mut client, &json!({ "op": "send", "text": "seed-before-move" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

    let target = home.join("Projects").join("crash-retry");
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "crash-retry" }),
    )
    .unwrap();
    let made = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(made["reply"], "project-made");

    // First post-move turn crashes the harness before any output/acknowledgment.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "crash-before-answer" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let _ = read_until(&mut client, |f| f["event"] == "interrupted" && f["turn"] == 2);

    // Next turn restarts the harness in the project folder and STILL delivers `<thread-history>`.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "after-crash" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut answer = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            answer.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 3 {
            break;
        }
    }
    assert!(answer.contains(&format!("CWD={}", target.display())), "{answer}");
    assert!(answer.contains("<thread-history>"), "{answer}");
    assert!(answer.contains("SEED_CRASH_MEMORY"), "{answer}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn moving_a_thread_respects_outliving_subagents_and_stopped_quiet_turns() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "check-cwd",
                    "steps": [{ "kind": "text", "text": "CWD={cwd}\nPROMPT={prompt}" }]
                },
                {
                    "match": "ctrl-subagent",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_ctrl_sub", "title": "Background" },
                        { "kind": "text", "text": "Launched." }
                    ],
                    "after": [
                        {
                            "kind": "subagent_ask",
                            "subagent": "ses_ctrl_sub",
                            "id": "call_ctrl_sub_ask",
                            "tool_kind": "read",
                            "title": "Read .env",
                            "raw_input": { "path": "/tmp/.env" }
                        },
                        { "kind": "subagent_end", "subagent": "ses_ctrl_sub", "status": "completed" }
                    ]
                },
                {
                    "match": "mcp-with-subagent",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_mcp_sub", "title": "Helper" },
                        {
                            "kind": "mcp_call",
                            "id": "call_mcp_sub_move",
                            "tool": "make_project",
                            "arguments": { "name": "after-sub" },
                            "reply": "MOVED={mcp_result}"
                        }
                    ],
                    "after": [
                        {
                            "kind": "subagent_ask",
                            "subagent": "ses_mcp_sub",
                            "id": "call_mcp_sub_ask",
                            "tool_kind": "read",
                            "title": "Read config",
                            "raw_input": { "path": "/tmp/cfg" }
                        },
                        { "kind": "subagent_end", "subagent": "ses_mcp_sub", "status": "completed" }
                    ]
                },
                {
                    "match": "quiet-stop-move",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_qs_sub", "title": "Look" },
                        { "kind": "subagent_end", "subagent": "ses_qs_sub", "status": "completed" },
                        { "kind": "text", "text": "Done main." }
                    ],
                    "after": [
                        { "kind": "text", "text": "Harness follow-up." },
                        { "kind": "pause_ms", "ms": 100 },
                        {
                            "kind": "mcp_call",
                            "id": "call_qs_move",
                            "tool": "make_project",
                            "arguments": { "name": "after-quiet-stop" },
                            "reply": "QS_MOVED={mcp_result}"
                        }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Part A: `make-project` control is refused while an outliving subagent/request is active.
    write_frame(&mut client, &json!({ "op": "send", "text": "ctrl-subagent" })).unwrap();
    let t1 = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t1);
    let req1 = read_until(&mut client, |f| {
        f["event"] == "request" && f["thread"] == t1 && f["state"] == "waiting"
    });
    let req1_id = req1["request"].as_str().unwrap().to_string();

    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": t1, "name": "blocked-by-sub" }),
    )
    .unwrap();
    let refused = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(refused["reply"], "error");
    assert_eq!(refused["code"], "working");

    // Answer the subagent's request, wait for the subagent to finish, then `make-project` succeeds.
    write_frame(
        &mut client,
        &json!({ "op": "answer", "thread": t1, "request": req1_id, "answer": "once" }),
    )
    .unwrap();
    let _ = read_until(&mut client, |f| {
        f["event"] == "subagent" && f["thread"] == t1 && f["state"] == "done"
    });
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": t1, "name": "blocked-by-sub" }),
    )
    .unwrap();
    let made1 = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(made1["reply"], "project-made");

    // Part B: MCP `make_project` during a turn with an outliving subagent keeps the old session
    // attached until the subagent and its request finish, then opens the next turn in the project folder.
    write_frame(&mut client, &json!({ "op": "send", "text": "mcp-with-subagent" })).unwrap();
    let t2 = read_until(&mut client, |f| f["reply"] == "sent")["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t2);
    let req2 = read_until(&mut client, |f| {
        f["event"] == "request" && f["thread"] == t2 && f["state"] == "waiting"
    });
    let req2_id = req2["request"].as_str().unwrap().to_string();
    write_frame(
        &mut client,
        &json!({ "op": "answer", "thread": t2, "request": req2_id, "answer": "once" }),
    )
    .unwrap();
    let _ = read_until(&mut client, |f| {
        f["event"] == "subagent" && f["thread"] == t2 && f["state"] == "done"
    });

    write_frame(&mut client, &json!({ "op": "send", "thread": t2, "text": "check-cwd" })).unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut t2_answer = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" && f["thread"] == t2 {
            t2_answer.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["thread"] == t2 && f["turn"] == 2 {
            break;
        }
    }
    let after_sub_dir = home.join("Projects").join("after-sub");
    assert!(
        t2_answer.contains(&format!("CWD={}", after_sub_dir.display())),
        "{t2_answer}"
    );
    assert!(t2_answer.contains("<thread-history>"), "{t2_answer}");

    // Part C: Harness-started turn moves thread via MCP, then person stops it and it goes quiet
    // (`stopped_quiet` path). The old session must still detach so the next turn runs in the project folder.
    write_frame(&mut client, &json!({ "op": "send", "text": "quiet-stop-move" })).unwrap();
    let t3 = read_until(&mut client, |f| f["reply"] == "sent")["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let after_qs_dir = home.join("Projects").join("after-quiet-stop");
    let _ = read_until(&mut client, |f| {
        f["event"] == "thread-updated" && f["thread"] == t3 && f["cwd"] == after_qs_dir.to_str().unwrap()
    });
    write_frame(&mut client, &json!({ "op": "stop", "thread": t3 })).unwrap();
    let _ = read_until(&mut client, |f| {
        f["event"] == "stopped" && f["thread"] == t3 && f["turn"] == 2
    });
    write_frame(&mut client, &json!({ "op": "send", "thread": t3, "text": "check-cwd" })).unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut t3_answer = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" && f["thread"] == t3 {
            t3_answer.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["thread"] == t3 && f["turn"] == 3 {
            break;
        }
    }
    assert!(
        t3_answer.contains(&format!("CWD={}", after_qs_dir.display())),
        "{t3_answer}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn deleting_a_moved_thread_deletes_retired_sessions_and_warns_when_unsupported() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": true,
            "session_prefix": "ses-del-",
            "turns": [
                { "steps": [{ "kind": "text", "text": "ok in {cwd}" }] }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let rpc_log_path = dir.0.join("rpc.jsonl");
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Thread 1: start turn (ses-del-1), move to project, delete immediately before any post-move turn.
    write_frame(&mut client, &json!({ "op": "send", "text": "one" })).unwrap();
    let t1 = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t1);
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": t1, "name": "del-immed" }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut client, |f| f.get("reply").is_some())["reply"],
        "project-made"
    );
    write_frame(&mut client, &json!({ "op": "delete", "thread": t1 })).unwrap();
    let del1 = read_until(&mut client, |f| f["reply"] == "deleted");
    assert!(del1["warning"].is_null(), "{del1}");

    // Thread 2: start turn (ses-del-2), move to project, run post-move turn (ses-del-3), delete.
    write_frame(&mut client, &json!({ "op": "send", "text": "two" })).unwrap();
    let t2 = read_until(&mut client, |f| f["reply"] == "sent")["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t2);
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": t2, "name": "del-after" }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut client, |f| f.get("reply").is_some())["reply"],
        "project-made"
    );
    write_frame(&mut client, &json!({ "op": "send", "thread": t2, "text": "two-after" })).unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let _ = read_until(&mut client, |f| {
        f["event"] == "done" && f["thread"] == t2 && f["turn"] == 2
    });
    write_frame(&mut client, &json!({ "op": "delete", "thread": t2 })).unwrap();
    let del2 = read_until(&mut client, |f| f["reply"] == "deleted");
    assert!(del2["warning"].is_null(), "{del2}");

    let rpc_log = std::fs::read_to_string(dir.0.join("rpc.jsonl")).unwrap();
    let deleted_sids: Vec<String> = rpc_log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["method"] == "session/delete")
        .filter_map(|v| v["params"]["sessionId"].as_str().map(str::to_string))
        .collect();
    assert!(deleted_sids.contains(&"ses-del-1".to_string()), "{deleted_sids:?}");
    assert!(deleted_sids.contains(&"ses-del-2".to_string()), "{deleted_sids:?}");
    assert!(deleted_sids.contains(&"ses-del-3".to_string()), "{deleted_sids:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // When the harness does NOT support `session/delete`, deleting right after a move still warns.
    let dir2 = TempDir::new();
    let home2 = dir2.0.join("lever-home");
    std::fs::create_dir_all(&home2).unwrap();
    let script2 = write_script(
        &dir2.0,
        &json!({
            "delete_session": false,
            "turns": [{ "steps": [{ "kind": "text", "text": "ok" }] }]
        }),
    );
    let mut lever2 = RunningLever::start_with_stub(&dir2.0, uid, &script2, None, None);
    let mut client2 = handshake(&lever2.person);
    client2.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write_frame(&mut client2, &json!({ "op": "send", "text": "hi" })).unwrap();
    let t3 = read_frame(&mut client2).unwrap()["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = read_until(&mut client2, |f| f["event"] == "done" && f["thread"] == t3);
    write_frame(
        &mut client2,
        &json!({ "op": "make-project", "thread": t3, "name": "warn-del" }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut client2, |f| f.get("reply").is_some())["reply"],
        "project-made"
    );
    write_frame(&mut client2, &json!({ "op": "delete", "thread": t3 })).unwrap();
    let del3 = read_until(&mut client2, |f| f["reply"] == "deleted");
    assert!(del3["warning"].as_str().is_some_and(|w| !w.is_empty()), "{del3}");

    lever2.signal(libc::SIGTERM);
    assert_eq!(lever2.wait().code(), Some(0));
}

#[test]
fn mcp_make_project_rejects_uncreatable_place_and_list_dirs_browses_home() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(home.join("Projects")).unwrap();
    std::fs::create_dir_all(home.join("Code")).unwrap();
    std::fs::create_dir_all(home.join(".hidden")).unwrap();
    let blocker = home.join("regular-file");
    std::fs::write(&blocker, "not a dir").unwrap();

    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "bad-mcp-place",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_bad_place",
                            "tool": "make_project",
                            "arguments": { "name": "demo", "place": "~/regular-file" },
                            "reply": "ERR={mcp_result}"
                        }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    write_frame(&mut client, &json!({ "op": "send", "text": "bad-mcp-place" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let mut answer = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            answer.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 1 {
            break;
        }
    }
    assert!(answer.contains("ERR="), "{answer}");
    assert!(
        answer.contains("not a directory") || answer.contains("Could not create") || answer.contains("Not a directory"),
        "{answer}"
    );

    // Thread remains in `$HOME`.
    write_frame(&mut client, &json!({ "op": "list" })).unwrap();
    let list = read_frame(&mut client).unwrap();
    assert_eq!(list["threads"][0]["cwd"], home.to_str().unwrap());

    // `make-project` rejects combining `name` and `path`.
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "foo", "path": "~/Code/bar" }),
    )
    .unwrap();
    let combo = read_frame(&mut client).unwrap();
    assert_eq!(combo["reply"], "error");
    assert_eq!(combo["code"], "invalid-request");

    // `list-dirs` over the surface socket lists non-hidden subdirectories as the person.
    let mut surface = handshake(&lever.surface);
    surface.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write_frame(
        &mut surface,
        &json!({ "op": "list-dirs", "path": "~/Projects/not-there-yet" }),
    )
    .unwrap();
    let dirs = read_frame(&mut surface).unwrap();
    assert_eq!(dirs["reply"], "dirs");
    assert_eq!(dirs["path"], home.join("Projects").to_str().unwrap());
    assert_eq!(dirs["home"], home.to_str().unwrap());
    assert_eq!(dirs["parent"], home.to_str().unwrap());

    write_frame(&mut surface, &json!({ "op": "list-dirs" })).unwrap();
    let home_dirs = read_frame(&mut surface).unwrap();
    let names: Vec<&str> = home_dirs["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(names.contains(&"Code"), "{names:?}");
    assert!(names.contains(&"Projects"), "{names:?}");
    assert!(!names.contains(&".hidden"), "{names:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn reassigned_session_id_is_never_deleted_when_old_moved_thread_is_deleted() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    // Harness reuses the same session ID for every new session, simulating
    // session ID reassignment after the first thread moved and detached.
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": true,
            "session_id": "ses-reused-live",
            "turns": [
                { "steps": [{ "kind": "text", "text": "ok in {cwd}" }] }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let rpc_log_path = dir.0.join("rpc.jsonl");
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Thread 1 starts on `ses-reused-live`, then moves to a project (retiring `ses-reused-live`).
    write_frame(&mut client, &json!({ "op": "send", "text": "first thread" })).unwrap();
    let t1 = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t1);
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": t1, "name": "old-proj" }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut client, |f| f.get("reply").is_some())["reply"],
        "project-made"
    );

    // Thread 2 now receives the reassigned `ses-reused-live`.
    write_frame(&mut client, &json!({ "op": "send", "text": "second thread" })).unwrap();
    let t2 = read_until(&mut client, |f| f["reply"] == "sent")["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["thread"] == t2);

    // Deleting thread 1 must NOT send `session/delete` for `ses-reused-live` (now owned by thread 2).
    write_frame(&mut client, &json!({ "op": "delete", "thread": t1 })).unwrap();
    let del = read_until(&mut client, |f| f["reply"] == "deleted");
    assert_eq!(del["thread"], t1);

    let rpc_log = std::fs::read_to_string(&rpc_log_path).unwrap();
    let deleted_sids: Vec<String> = rpc_log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["method"] == "session/delete")
        .filter_map(|v| v["params"]["sessionId"].as_str().map(str::to_string))
        .collect();
    assert!(
        !deleted_sids.contains(&"ses-reused-live".to_string()),
        "must not delete session owned by thread 2: {deleted_sids:?}"
    );

    // Thread 2 continues working normally.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": t2, "text": "still alive" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let _ = read_until(&mut client, |f| {
        f["event"] == "done" && f["thread"] == t2 && f["turn"] == 2
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn chained_mcp_move_on_first_post_move_turn_preserves_handoff_and_post_turn_subagent() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "check-b",
                    "steps": [{ "kind": "text", "text": "CWD={cwd}\nPROMPT={prompt}" }]
                },
                {
                    "match": "move-to-b",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_chain_b",
                            "tool": "make_project",
                            "arguments": { "name": "proj-b" },
                            "reply": "MOVED_B={mcp_result}"
                        }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 150 },
                        { "kind": "subagent_start", "id": "ses_after_prompt", "title": "Post-turn helper" },
                        {
                            "kind": "subagent_ask",
                            "subagent": "ses_after_prompt",
                            "id": "call_after_ask",
                            "tool_kind": "read",
                            "title": "Read file",
                            "raw_input": { "path": "/tmp/a" }
                        },
                        { "kind": "subagent_text", "subagent": "ses_after_prompt", "text": "SUBAGENT_FINDING_42" },
                        { "kind": "subagent_end", "subagent": "ses_after_prompt", "status": "completed" }
                    ]
                },
                {
                    "match": "seed-chain",
                    "steps": [{ "kind": "text", "text": "CHAIN_SEED_TOKEN" }]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Turn 1 in `$HOME`, then control moves thread to `proj-a`.
    write_frame(&mut client, &json!({ "op": "send", "text": "seed-chain" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);
    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "proj-a" }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut client, |f| f.get("reply").is_some())["reply"],
        "project-made"
    );

    // Turn 2 (first turn in `proj-a`, carrying `proj-a` handoff) immediately calls MCP `make_project`
    // for `proj-b`, finishes, and starts a subagent right after `PromptResponse`.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "move-to-b" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 2);

    // Subagent started right after `PromptResponse` must still be routed and answerable.
    let req = read_until(&mut client, |f| f["event"] == "request" && f["state"] == "waiting");
    let req_id = req["request"].as_str().unwrap().to_string();
    write_frame(
        &mut client,
        &json!({ "op": "answer", "thread": tid, "request": req_id, "answer": "once" }),
    )
    .unwrap();
    let _ = read_until(&mut client, |f| f["event"] == "subagent" && f["state"] == "done");

    // Turn 3 must run in `proj-b` AND still receive `<thread-history>` containing `CHAIN_SEED_TOKEN`.
    let proj_b = home.join("Projects").join("proj-b");
    write_frame(&mut client, &json!({ "op": "send", "thread": tid, "text": "check-b" })).unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut turn3 = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            turn3.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 3 {
            break;
        }
    }
    assert!(turn3.contains(&format!("CWD={}", proj_b.display())), "{turn3}");
    assert!(turn3.contains("<thread-history>"), "{turn3}");
    assert!(turn3.contains("CHAIN_SEED_TOKEN"), "{turn3}");
    assert!(turn3.contains("SUBAGENT_FINDING_42"), "{turn3}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stopping_first_project_turn_before_output_preserves_handoff() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "check-after-stop",
                    "steps": [{ "kind": "text", "text": "CWD={cwd}\nPROMPT={prompt}" }]
                },
                {
                    "match": "cancel-before-output",
                    "steps": [],
                    "stall": true,
                    "cancel_with_error": true
                },
                {
                    "match": "seed-stop-handoff",
                    "steps": [{ "kind": "text", "text": "STOP_HANDOFF_SEED" }]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let rpc_log_path = dir.0.join("rpc.jsonl");
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    write_frame(&mut client, &json!({ "op": "send", "text": "seed-stop-handoff" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

    write_frame(
        &mut client,
        &json!({ "op": "make-project", "thread": tid, "name": "proj-stop" }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut client, |f| f.get("reply").is_some())["reply"],
        "project-made"
    );

    // Turn 2 carries `<thread-history>`, emits nothing, and is cancelled via RequestCancelled.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "cancel-before-output" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    // Wait deterministically until Turn 2's `session/prompt` has actually been sent.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let prompt_count = std::fs::read_to_string(&rpc_log_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["method"] == "session/prompt")
            .count();
        if prompt_count >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "timed out waiting for Turn 2 session/prompt");
        std::thread::sleep(Duration::from_millis(10));
    }
    write_frame(&mut client, &json!({ "op": "stop", "thread": tid })).unwrap();
    let _ = read_until(&mut client, |f| f["event"] == "stopped" && f["turn"] == 2);

    // Turn 3 must STILL receive `<thread-history>` containing `STOP_HANDOFF_SEED`.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "check-after-stop" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut turn3 = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            turn3.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 3 {
            break;
        }
    }
    assert!(turn3.contains("<thread-history>"), "{turn3}");
    assert!(turn3.contains("STOP_HANDOFF_SEED"), "{turn3}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn partial_delete_retries_only_remaining_session_and_warns_on_missing_retired() {
    let uid = unsafe { libc::geteuid() };

    // Part A: retired session delete succeeds, current session delete fails once;
    // retry must NOT send session/delete for the already-deleted retired session again.
    {
        let dir = TempDir::new();
        let home = dir.0.join("lever-home");
        std::fs::create_dir_all(&home).unwrap();
        let script = write_script(
            &dir.0,
            &json!({
                "delete_session": true,
                "session_prefix": "ses-partial-",
                "delete_fail_once_for": "ses-partial-2",
                "turns": [{ "steps": [{ "kind": "text", "text": "ok" }] }]
            }),
        );
        let rpc_log_path = dir.0.join("rpc.jsonl");
        let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
        let mut client = handshake(&lever.person);
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

        write_frame(&mut client, &json!({ "op": "send", "text": "turn 1" })).unwrap();
        let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
        let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

        write_frame(
            &mut client,
            &json!({ "op": "make-project", "thread": tid, "name": "proj-partial" }),
        )
        .unwrap();
        assert_eq!(
            read_until(&mut client, |f| f.get("reply").is_some())["reply"],
            "project-made"
        );

        write_frame(&mut client, &json!({ "op": "send", "thread": tid, "text": "turn 2" })).unwrap();
        assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
        let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 2);

        // First delete fails on `ses-partial-2` after deleting `ses-partial-1`.
        write_frame(&mut client, &json!({ "op": "delete", "thread": tid })).unwrap();
        let err = read_until(&mut client, |f| f.get("reply").is_some());
        assert_eq!(err["reply"], "error", "{err}");
        assert_eq!(err["code"], "harness-error", "{err}");

        // Second delete retries ONLY `ses-partial-2` and succeeds cleanly.
        write_frame(&mut client, &json!({ "op": "delete", "thread": tid })).unwrap();
        let ok = read_until(&mut client, |f| f["reply"] == "deleted");
        assert!(ok["warning"].is_null(), "{ok}");

        let rpc_log = std::fs::read_to_string(&rpc_log_path).unwrap();
        let deleted_sids: Vec<String> = rpc_log
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["method"] == "session/delete")
            .filter_map(|v| v["params"]["sessionId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            deleted_sids,
            vec![
                "ses-partial-1".to_string(),
                "ses-partial-2".to_string(),
                "ses-partial-2".to_string(),
            ],
            "retired session should only be deleted once across retry: {deleted_sids:?}"
        );

        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
    }

    // Part B: a retired session that permanently fails deletion warns instead
    // of blocking deletion of the current session and thread record.
    {
        let dir = TempDir::new();
        let home = dir.0.join("lever-home");
        std::fs::create_dir_all(&home).unwrap();
        let script = write_script(
            &dir.0,
            &json!({
                "delete_session": true,
                "session_prefix": "ses-retired-gone-",
                "delete_fail_for": ["ses-retired-gone-1"],
                "turns": [{ "steps": [{ "kind": "text", "text": "ok" }] }]
            }),
        );
        let rpc_log_path = dir.0.join("rpc.jsonl");
        let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
        let mut client = handshake(&lever.person);
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

        write_frame(&mut client, &json!({ "op": "send", "text": "turn 1" })).unwrap();
        let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
        let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

        write_frame(
            &mut client,
            &json!({ "op": "make-project", "thread": tid, "name": "proj-gone" }),
        )
        .unwrap();
        assert_eq!(
            read_until(&mut client, |f| f.get("reply").is_some())["reply"],
            "project-made"
        );

        write_frame(&mut client, &json!({ "op": "send", "thread": tid, "text": "turn 2" })).unwrap();
        assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
        let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 2);

        write_frame(&mut client, &json!({ "op": "delete", "thread": tid })).unwrap();
        let ok = read_until(&mut client, |f| f["reply"] == "deleted");
        assert!(ok["warning"].as_str().is_some(), "expected warning: {ok}");

        let rpc_log = std::fs::read_to_string(&rpc_log_path).unwrap();
        let deleted_sids: Vec<String> = rpc_log
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["method"] == "session/delete")
            .filter_map(|v| v["params"]["sessionId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            deleted_sids,
            vec!["ses-retired-gone-1".to_string(), "ses-retired-gone-2".to_string()]
        );

        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
    }

    // Part C: retired session fails permanently AND current session fails once;
    // retry must still surface the warning for the unresolved retired session.
    {
        let dir = TempDir::new();
        let home = dir.0.join("lever-home");
        std::fs::create_dir_all(&home).unwrap();
        let script = write_script(
            &dir.0,
            &json!({
                "delete_session": true,
                "session_prefix": "ses-both-",
                "delete_fail_for": ["ses-both-1"],
                "delete_fail_once_for": "ses-both-2",
                "turns": [{ "steps": [{ "kind": "text", "text": "ok" }] }]
            }),
        );
        let rpc_log_path = dir.0.join("rpc.jsonl");
        let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
        let mut client = handshake(&lever.person);
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

        write_frame(&mut client, &json!({ "op": "send", "text": "turn 1" })).unwrap();
        let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
        let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

        write_frame(
            &mut client,
            &json!({ "op": "make-project", "thread": tid, "name": "proj-both" }),
        )
        .unwrap();
        assert_eq!(
            read_until(&mut client, |f| f.get("reply").is_some())["reply"],
            "project-made"
        );

        write_frame(&mut client, &json!({ "op": "send", "text": "turn 2", "thread": tid })).unwrap();
        assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
        let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 2);

        // First delete fails on `ses-both-2` after `ses-both-1` failed.
        write_frame(&mut client, &json!({ "op": "delete", "thread": tid })).unwrap();
        let err = read_until(&mut client, |f| f.get("reply").is_some());
        assert_eq!(err["reply"], "error", "{err}");
        assert_eq!(err["code"], "harness-error", "{err}");

        // Retry still attempts `ses-both-1`, succeeds on `ses-both-2`, and keeps the warning.
        write_frame(&mut client, &json!({ "op": "delete", "thread": tid })).unwrap();
        let ok = read_until(&mut client, |f| f["reply"] == "deleted");
        assert!(ok["warning"].as_str().is_some(), "warning must survive retry: {ok}");

        let rpc_log = std::fs::read_to_string(&rpc_log_path).unwrap();
        let deleted_sids: Vec<String> = rpc_log
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["method"] == "session/delete")
            .filter_map(|v| v["params"]["sessionId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            deleted_sids,
            vec![
                "ses-both-1".to_string(),
                "ses-both-2".to_string(),
                "ses-both-1".to_string(),
                "ses-both-2".to_string(),
            ],
            "retry must re-attempt unresolved retired session: {deleted_sids:?}"
        );

        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
    }
}

#[test]
fn queued_message_draining_after_move_still_routes_delayed_old_session_subagent() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "queued-followup",
                    "steps": [
                        { "kind": "pause_ms", "ms": 300 },
                        { "kind": "text", "text": "CWD={cwd}" }
                    ]
                },
                {
                    "match": "move-and-queue",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_q_move",
                            "tool": "make_project",
                            "arguments": { "name": "proj-queued" },
                            "reply": "MOVED={mcp_result}"
                        },
                        { "kind": "pause_ms", "ms": 250 }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 150 },
                        { "kind": "subagent_start", "id": "ses_late_queued", "title": "Delayed helper" },
                        { "kind": "subagent_text", "subagent": "ses_late_queued", "text": "LATE_QUEUED_CHILD_RESULT" },
                        { "kind": "subagent_end", "subagent": "ses_late_queued", "status": "completed" },
                        {
                            "kind": "subagent_start",
                            "id": "ses_nested_late",
                            "title": "Delayed nested helper",
                            "launched_by": "ses_parent_t1"
                        },
                        { "kind": "subagent_text", "subagent": "ses_nested_late", "text": "LATE_NESTED_RESULT" },
                        { "kind": "subagent_end", "subagent": "ses_nested_late", "status": "completed" }
                    ]
                },
                {
                    "match": "turn1-parent",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_parent_t1", "title": "Turn 1 parent" },
                        { "kind": "subagent_end", "subagent": "ses_parent_t1", "status": "completed" },
                        { "kind": "text", "text": "Turn 1 parent finished" }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Turn 1 runs a parent subagent that finishes on Turn 1.
    write_frame(&mut client, &json!({ "op": "send", "text": "turn1-parent" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

    // Turn 2 calls `make_project`, and while Turn 2 is finishing its pause we
    // queue Turn 3 behind it.
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "move-and-queue" }),
    )
    .unwrap();
    let _ = read_until(&mut client, |f| {
        f["event"] == "thread-updated" && f.get("cwd").is_some()
    });

    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "queued-followup" }),
    )
    .unwrap();
    let q_reply = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(q_reply["reply"], "queued", "{q_reply}");

    // Turn 2 ends, queued Turn 3 drains immediately into `proj-queued`, AND:
    // - `Delayed helper` (direct child of retired `ses-1` on Turn 2) lands on Turn 2,
    // - `Delayed nested helper` (child of Turn 1's finished parent) lands on Turn 1,
    // - Turn 3 owns neither.
    let proj_q = home.join("Projects").join("proj-queued");
    let mut saw_direct_done = false;
    let mut saw_nested_done = false;
    let mut saw_subagent_text = false;
    let mut saw_nested_text = false;
    let mut saw_turn3_done = false;
    let mut turn3_text = String::new();
    while !(saw_direct_done && saw_nested_done && saw_turn3_done) {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" && f["turn"] == 3 {
            turn3_text.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "subagent-text" {
            let text = f["text"].as_str().unwrap_or("");
            if text.contains("LATE_QUEUED_CHILD_RESULT") {
                assert_eq!(
                    f["turn"], 2,
                    "delayed direct child of Turn 2 must belong to Turn 2: {f}"
                );
                saw_subagent_text = true;
            }
            if text.contains("LATE_NESTED_RESULT") {
                assert_eq!(
                    f["turn"], 1,
                    "delayed nested child of Turn 1 parent must belong to Turn 1: {f}"
                );
                saw_nested_text = true;
            }
        }
        if f["event"] == "subagent" {
            match f["title"].as_str() {
                Some("Delayed helper") => {
                    assert_eq!(f["turn"], 2, "direct delayed child must belong to Turn 2: {f}");
                    if f["state"] == "done" {
                        saw_direct_done = true;
                    }
                }
                Some("Delayed nested helper") => {
                    assert_eq!(f["turn"], 1, "nested delayed child must belong to Turn 1: {f}");
                    if f["state"] == "done" {
                        saw_nested_done = true;
                    }
                }
                _ => {}
            }
        }
        if f["event"] == "done" && f["turn"] == 3 {
            saw_turn3_done = true;
        }
    }
    assert!(
        turn3_text.contains(&format!("CWD={}", proj_q.display())),
        "{turn3_text}"
    );
    assert!(saw_subagent_text, "late subagent text was not routed");
    assert!(saw_nested_text, "late nested subagent text was not routed");

    // Verify saved record attributes each subagent to its true turn.
    write_frame(&mut client, &json!({ "op": "resume", "thread": tid })).unwrap();
    let resumed = read_until(&mut client, |f| f["reply"] == "resumed");
    let turn1_titles: Vec<&str> = resumed["turns"][0]["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["title"].as_str())
        .collect();
    let turn2_titles: Vec<&str> = resumed["turns"][1]["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["title"].as_str())
        .collect();
    let turn3_subs = resumed["turns"][2]["subagents"].as_array().unwrap();
    assert_eq!(
        turn1_titles,
        vec!["Turn 1 parent", "Delayed nested helper"],
        "Turn 1 must own its parent and delayed nested child: {resumed}"
    );
    assert_eq!(
        turn2_titles,
        vec!["Delayed helper"],
        "Turn 2 must own its delayed direct child: {resumed}"
    );
    assert!(
        turn3_subs.is_empty(),
        "Turn 3 must not own pre-move subagents: {resumed}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn mcp_make_project_rejected_after_stop_leaves_folder_and_thread_unchanged() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "stop-before-mcp",
                    "ignore_cancel": true,
                    "steps": [
                        { "kind": "pause_ms", "ms": 300 },
                        {
                            "kind": "mcp_call",
                            "id": "call_after_stop",
                            "tool": "make_project",
                            "arguments": { "name": "proj-after-stop" },
                            "reply": "MCP_AFTER_STOP={mcp_result}"
                        }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let rpc_log_path = dir.0.join("rpc.jsonl");
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    write_frame(&mut client, &json!({ "op": "send", "text": "stop-before-mcp" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();

    // Wait until `session/prompt` is in flight, then send Stop before the MCP call fires.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let prompt_sent = std::fs::read_to_string(&rpc_log_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|v| v["method"] == "session/prompt");
        if prompt_sent {
            break;
        }
        assert!(Instant::now() < deadline, "timed out waiting for session/prompt");
        std::thread::sleep(Duration::from_millis(10));
    }

    write_frame(&mut client, &json!({ "op": "stop", "thread": tid })).unwrap();
    let _ = read_until(&mut client, |f| {
        (f["event"] == "done" || f["event"] == "stopped") && f["turn"] == 1
    });

    // Neither the folder on disk nor the thread's cwd may have changed.
    assert!(
        !home.join("Projects").join("proj-after-stop").exists(),
        "make_project created folder after Stop"
    );
    write_frame(&mut client, &json!({ "op": "resume", "thread": tid })).unwrap();
    let resumed = read_until(&mut client, |f| f["reply"] == "resumed");
    assert_eq!(
        resumed["cwd"].as_str().unwrap(),
        home.to_string_lossy(),
        "thread cwd changed after Stop: {resumed}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn failed_delete_and_rejected_model_preserve_pending_session_reset() {
    let dir = TempDir::new();
    let home = dir.0.join("lever-home");
    std::fs::create_dir_all(&home).unwrap();
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": false,
            "models": [
                { "id": "stub/model-a", "name": "Model A" },
                { "id": "stub/model-b", "name": "Model B" }
            ],
            "turns": [
                {
                    "match": "check-cwd",
                    "steps": [{ "kind": "text", "text": "CWD={cwd}" }]
                },
                {
                    "match": "move-mid-turn",
                    "steps": [
                        {
                            "kind": "mcp_call",
                            "id": "call_mid_move",
                            "tool": "make_project",
                            "arguments": { "name": "proj-keep-reset" },
                            "reply": "MOVED={mcp_result}"
                        }
                    ]
                }
            ]
        }),
    );

    let uid = unsafe { libc::geteuid() };
    let rpc_log_path = dir.0.join("rpc.jsonl");
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log_path), None);
    let mut client = handshake(&lever.person);
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    // Turn 1 moves mid-turn, leaving `pending_session_resets` populated while idle.
    write_frame(&mut client, &json!({ "op": "send", "text": "move-mid-turn" })).unwrap();
    let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
    let _ = read_until(&mut client, |f| f["event"] == "done" && f["turn"] == 1);

    // 1. A rejected `set-model` must NOT flush the pending reset or send `session/close`.
    write_frame(
        &mut client,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/nonexistent" }),
    )
    .unwrap();
    let bad_model = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(bad_model["code"], "invalid-model", "{bad_model}");
    let rpc_after_bad = std::fs::read_to_string(&rpc_log_path).unwrap();
    assert!(
        !rpc_after_bad.contains("session/close"),
        "rejected set-model flushed pending reset early: {rpc_after_bad}"
    );

    // 2. A failed SQLite `delete` must NOT clear `pending_session_resets` so
    // the next turn still detaches the old session and runs in the new project.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "DELETE ON threads");
    write_frame(&mut client, &json!({ "op": "delete", "thread": tid })).unwrap();
    let del_err = read_until(&mut client, |f| f.get("reply").is_some());
    assert_eq!(del_err["code"], "storage-error", "{del_err}");
    db.execute_batch("DROP TRIGGER refused").unwrap();

    let proj = home.join("Projects").join("proj-keep-reset");
    write_frame(
        &mut client,
        &json!({ "op": "send", "thread": tid, "text": "check-cwd" }),
    )
    .unwrap();
    assert_eq!(read_until(&mut client, |f| f.get("reply").is_some())["reply"], "sent");
    let mut turn2 = String::new();
    loop {
        let f = read_frame(&mut client).unwrap();
        if f["event"] == "text" {
            turn2.push_str(f["text"].as_str().unwrap());
        }
        if f["event"] == "done" && f["turn"] == 2 {
            break;
        }
    }
    assert!(
        turn2.contains(&format!("CWD={}", proj.display())),
        "next turn reused old folder after failed delete: {turn2}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn mcp_make_project_works_with_relative_and_long_state_directories() {
    let uid = unsafe { libc::geteuid() };

    for mode in [
        "relative",
        "long",
        "colliding-surface",
        "colliding-fallback",
        "occupied-fallback",
    ] {
        let dir = TempDir::new();
        let home = dir.0.join("lever-home");
        let launch_dir = dir.0.join("launch");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&launch_dir).unwrap();
        let surface_dir = dir.0.join("s");
        let person_dir = dir.0.join("p");
        std::fs::create_dir_all(&surface_dir).unwrap();
        let person = person_dir.join("p");

        let mut occupied_listener: Option<(std::path::PathBuf, std::os::unix::net::UnixListener)> = None;
        let (state_arg, state_abs, surface) = match mode {
            "relative" => (
                "rel-state".to_string(),
                launch_dir.join("rel-state"),
                surface_dir.join("s"),
            ),
            "long" => {
                let long_state = dir.0.join("a".repeat(40)).join("b".repeat(40)).join("c".repeat(40));
                (
                    long_state.to_str().unwrap().to_string(),
                    long_state,
                    surface_dir.join("s"),
                )
            }
            "colliding-surface" => {
                let st = dir.0.join("st");
                std::fs::create_dir_all(&st).unwrap();
                (st.to_str().unwrap().to_string(), st.clone(), st.join("tools.sock"))
            }
            "colliding-fallback" => {
                let long_state = dir.0.join("a".repeat(40)).join("b".repeat(40)).join("c".repeat(40));
                std::fs::create_dir_all(&person_dir).unwrap();
                (
                    long_state.to_str().unwrap().to_string(),
                    long_state,
                    person_dir.join("tools.sock"),
                )
            }
            "occupied-fallback" => {
                let long_state = dir.0.join("a".repeat(40)).join("b".repeat(40)).join("c".repeat(40));
                std::fs::create_dir_all(&person_dir).unwrap();
                let occupied_path = person_dir.join("tools.sock");
                let listener = std::os::unix::net::UnixListener::bind(&occupied_path).unwrap();
                listener.set_nonblocking(true).unwrap();
                occupied_listener = Some((occupied_path, listener));
                (
                    long_state.to_str().unwrap().to_string(),
                    long_state,
                    surface_dir.join("s"),
                )
            }
            _ => unreachable!(),
        };

        let proj_name = format!("proj-{mode}-state");
        let script = write_script(
            &dir.0,
            &json!({
                "turns": [
                    {
                        "steps": [
                            {
                                "kind": "mcp_call",
                                "id": "call_state_test",
                                "tool": "make_project",
                                "arguments": { "name": proj_name },
                                "reply": "MOVED={mcp_result}"
                            }
                        ]
                    }
                ]
            }),
        );

        let mut cmd = Command::new(BIN);
        cmd.current_dir(&launch_dir)
            .args([
                "--state",
                &state_arg,
                "--surface-socket",
                surface.to_str().unwrap(),
                "--shell-uid",
                &uid.to_string(),
                "--person-socket",
                person.to_str().unwrap(),
                "--person-uid",
                &uid.to_string(),
                "--harness",
                STUB_BIN,
                "--harness-name",
                "Stub",
                "--harness-arg",
                "--script",
                "--harness-arg",
                script.to_str().unwrap(),
            ])
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"));
        for key in [
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "OPENCODE_CONFIG_CONTENT",
            "OPENCODE_DB",
        ] {
            cmd.env_remove(key);
        }
        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn crowbar-lever");
        let mut lever = RunningLever {
            child: Some(child),
            state: state_abs,
            surface,
            person,
        };
        lever.wait_for_sockets();

        let mut client = handshake(&lever.person);
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        // Verify the surface listener is ALSO still reachable and wasn't replaced by MCP.
        let mut surface_client = handshake(&lever.surface);
        surface_client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write_frame(&mut surface_client, &json!({ "op": "status" })).unwrap();
        let status = read_until(&mut surface_client, |f| f["reply"] == "status");
        assert!(
            status.get("harnesses").is_some(),
            "{mode}: surface status failed: {status}"
        );

        write_frame(&mut client, &json!({ "op": "send", "text": "make project" })).unwrap();
        let tid = read_frame(&mut client).unwrap()["thread"].as_str().unwrap().to_string();
        let mut text = String::new();
        loop {
            let f = read_frame(&mut client).unwrap();
            if f["event"] == "text" {
                text.push_str(f["text"].as_str().unwrap());
            }
            if f["event"] == "done" && f["turn"] == 1 {
                break;
            }
        }
        let expected_dir = home.join("Projects").join(&proj_name);
        assert!(
            expected_dir.is_dir(),
            "{mode}: expected {} to exist; text={text}",
            expected_dir.display()
        );
        write_frame(&mut client, &json!({ "op": "resume", "thread": tid })).unwrap();
        let resumed = read_until(&mut client, |f| f["reply"] == "resumed");
        assert_eq!(resumed["cwd"], expected_dir.to_string_lossy().as_ref());

        if let Some((occupied_path, listener)) = occupied_listener {
            let probe = std::os::unix::net::UnixStream::connect(&occupied_path)
                .expect("unrelated live socket at fallback tools.sock must stay reachable");
            let (_accepted, _) = listener
                .accept()
                .expect("unrelated listener at fallback tools.sock must accept the connection");
            drop(probe);
            drop(listener);
        }

        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
    }
}
