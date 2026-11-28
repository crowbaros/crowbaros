//! Subagents, harness-started turns, held stops, nested subagents, mode settling, and subagent-request prompt holds.

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
fn subagent_outlives_turn_raises_request_keeps_harness_alive_and_hides_harness_names() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "ses_child_secret_99",
                        "title": "Check secrets",
                        "prompt": "Read .env and report"
                    },
                    { "kind": "text", "text": "Handed off to a subagent." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 550 },
                    {
                        "kind": "subagent_act",
                        "subagent": "ses_child_secret_99",
                        "id": "call_ls_1",
                        "tool_kind": "read",
                        "title": "List folder",
                        "status": "completed",
                        "content": ".env"
                    },
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_child_secret_99",
                        "id": "call_env_1",
                        "tool_kind": "read",
                        "title": "Read /tmp/.env",
                        "raw_input": { "path": "/tmp/.env" },
                        "output": "API_MODE=ok"
                    },
                    {
                        "kind": "subagent_text",
                        "subagent": "ses_child_secret_99",
                        "text": "Found API_MODE=ok."
                    },
                    {
                        "kind": "subagent_end",
                        "subagent": "ses_child_secret_99",
                        "status": "completed"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, Some(350));
    let mut watcher = handshake(&lever.surface);
    write_frame(&mut watcher, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "activity");

    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Check the workspace");
    let mut seen_frames: Vec<Value> = Vec::new();
    let mut subagent_id: Option<String> = None;

    // Wait for the parent turn to end (`done`), recording every event.
    loop {
        let frame = read_frame(&mut peer).unwrap();
        seen_frames.push(frame.clone());
        if frame["event"] == "subagent" {
            subagent_id = frame["subagent"].as_str().map(str::to_string);
        }
        if frame["event"] == "done" && frame["thread"] == thread && frame["turn"] == turn {
            break;
        }
    }
    let sub_id = subagent_id.expect("subagent event before turn ended");
    assert!(sub_id.starts_with("sub-"), "expected Lever sub-... id, got {sub_id}");

    // Wait past the 350ms idle timeout while the subagent is still working.
    std::thread::sleep(Duration::from_millis(450));

    let mut inspector = handshake(&lever.person);
    write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut inspector).unwrap();
    assert_eq!(
        status["harness_started"], true,
        "harness must stay running while subagent works past idle timeout"
    );

    write_frame(&mut inspector, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut inspector).unwrap();
    seen_frames.push(listed.clone());
    let threads = listed["threads"].as_array().unwrap();
    assert_eq!(
        threads.len(),
        1,
        "subagent session must never be listed as a thread: {listed}"
    );
    assert_eq!(threads[0]["thread"], thread);
    assert_eq!(threads[0]["working"], true);
    assert_eq!(threads[0]["state"], "working");
    assert_eq!(threads[0]["subagents"], 1);

    // Subagent raises a request after its parent turn has ended.
    let req = loop {
        let frame = read_frame(&mut peer).unwrap();
        seen_frames.push(frame.clone());
        if frame["event"] == "request" && frame["state"] == "waiting" {
            break frame;
        }
    };
    assert_eq!(req["thread"], thread);
    assert_eq!(req["turn"], turn);
    assert_eq!(req["subagent"], sub_id);
    assert_eq!(
        req["title"], "Read /tmp/.env",
        "child title prefix must be stripped: {req}"
    );
    let req_id = req["request"].as_str().unwrap().to_string();

    // Activity reports the thread as both working and waiting.
    let waiting_activity = read_until(&mut watcher, |v| {
        v["event"] == "activity" && v["waiting"].as_array().is_some_and(|w| w.iter().any(|t| t == &thread))
    });
    assert!(
        waiting_activity["working"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == &thread),
        "{waiting_activity}"
    );

    // Answer the subagent's request and wait for the subagent to finish.
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": req_id, "answer": "once" }),
    )
    .unwrap();
    let ans_reply = read_until(&mut peer, |v| v.get("reply").is_some());
    assert_eq!(ans_reply["reply"], "ok", "{ans_reply}");

    let sub_done = loop {
        let frame = read_frame(&mut peer).unwrap();
        seen_frames.push(frame.clone());
        if frame["event"] == "subagent" && frame["state"] == "done" {
            break frame;
        }
    };
    assert_eq!(sub_done["subagent"], sub_id);

    // Activity clears working and waiting once the subagent ends.
    let clear = read_until(&mut watcher, |v| {
        v["event"] == "activity"
            && v["working"].as_array().is_some_and(Vec::is_empty)
            && v["waiting"].as_array().is_some_and(Vec::is_empty)
    });
    assert_eq!(clear["working"], json!([]));

    // Resumed record keeps the subagent's title, messages, acts, state, and request link.
    let rec = resume(&mut inspector, &thread);
    seen_frames.push(rec.clone());
    assert_eq!(rec["working"], false);
    assert_eq!(
        rec["turns"][0]["acts"].as_array().unwrap().len(),
        0,
        "OpenCode subagent wrapper must not appear as parent act: {rec}"
    );
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1, "{rec}");
    let sub = &subs[0];
    assert_eq!(sub["subagent"], sub_id);
    assert_eq!(sub["title"], "Check secrets");
    assert_eq!(sub["state"], "done");
    assert_eq!(
        sub["messages"],
        json!([
            { "role": "prompt", "text": "Read .env and report" },
            { "role": "answer", "text": "Found API_MODE=ok." }
        ])
    );
    let sub_acts = sub["acts"].as_array().unwrap();
    assert_eq!(sub_acts.len(), 2, "{sub}");
    assert_eq!(sub_acts[0]["title"], "List folder");
    assert_eq!(sub_acts[1]["title"], "Read /tmp/.env");
    assert_eq!(sub_acts[1]["state"], "done");
    assert_eq!(rec["turns"][0]["requests"][0]["subagent"], sub_id);
    assert_eq!(rec["turns"][0]["requests"][0]["state"], "answered");
    assert_eq!(rec["turns"][0]["requests"][0]["answer"], "once");

    // No harness/OpenCode session or tool call identifier crosses Lever's sockets.
    for frame in &seen_frames {
        let raw = frame.to_string();
        for forbidden in [
            "ses_child_secret_99",
            "opencode",
            "childSessionId",
            "sessionID",
            "call_env_1",
            "call_ls_1",
        ] {
            assert!(
                !raw.contains(forbidden),
                "harness identifier {forbidden:?} leaked onto Lever socket: {raw}"
            );
        }
    }

    // Once the subagent ends, the idle timeout stops the harness as usual.
    wait_until("idle harness to stop after subagent ends", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn acp_v1_subagent_works_and_stop_cancels_subagent_outliving_its_turn() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "acp-child-42",
                        "title": "Audit tree",
                        "prompt": "Check every file",
                        "protocol": "acp"
                    },
                    { "kind": "text", "text": "Running in background." }
                ],
                "after": [
                    {
                        "kind": "subagent_act",
                        "subagent": "acp-child-42",
                        "id": "child-act-1",
                        "tool_kind": "search",
                        "title": "Search repo",
                        "status": "completed",
                        "content": "3 matches",
                        "protocol": "acp"
                    },
                    {
                        "kind": "subagent_text",
                        "subagent": "acp-child-42",
                        "text": "Partial findings.",
                        "protocol": "acp"
                    },
                    {
                        "kind": "subagent_stall",
                        "subagent": "acp-child-42",
                        "protocol": "acp"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Audit");

    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    read_until(&mut peer, |v| v["event"] == "subagent-text");

    // Thread is still working while its ACP v1 subagent runs.
    let mut inspector = handshake(&lever.person);
    let rec = resume(&mut inspector, &thread);
    assert_eq!(rec["working"], true);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "working");

    // Stop cancels the subagent and marks it stopped.
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    let stopped_sub = read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] == "stopped");
    assert_eq!(stopped_sub["title"], "Audit tree");

    let rec2 = resume(&mut inspector, &thread);
    assert_eq!(rec2["working"], false);
    assert_eq!(rec2["turns"][0]["subagents"][0]["state"], "stopped");
    assert_eq!(
        rec2["turns"][0]["subagents"][0]["messages"],
        json!([
            { "role": "prompt", "text": "Check every file" },
            { "role": "answer", "text": "Partial findings." }
        ])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn lever_restart_marks_working_subagent_interrupted_and_withdraws_its_request() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "ses_sub_restart_1",
                        "title": "Long check"
                    },
                    { "kind": "text", "text": "Started." }
                ],
                "after": [
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_sub_restart_1",
                        "id": "call_wait_1",
                        "tool_kind": "execute",
                        "title": "uname -a",
                        "raw_input": { "command": "uname -a" }
                    },
                    {
                        "kind": "subagent_stall",
                        "subagent": "ses_sub_restart_1"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, _turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    drop(peer);

    // Stop Lever while the subagent is working and waiting on its request.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Reopen Lever: subagent is interrupted, request is withdrawn, thread is idle.
    let mut lever2 = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer2 = handshake(&lever2.person);
    let rec = resume(&mut peer2, &thread);
    assert_eq!(rec["working"], false);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "interrupted");
    assert_eq!(rec["turns"][0]["requests"][0]["state"], "withdrawn");

    lever2.signal(libc::SIGTERM);
    assert_eq!(lever2.wait().code(), Some(0));
}

#[test]
fn subagent_ending_withdraws_waiting_request_and_maps_cancelled_to_stopped() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "ses_sub_cancel_1",
                        "title": "Check tree"
                    },
                    { "kind": "text", "text": "Dispatched." }
                ],
                "after": [
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_sub_cancel_1",
                        "id": "call_cancel_1",
                        "tool_kind": "read",
                        "title": "Check tree: Read .env",
                        "raw_input": { "path": "/tmp/.env" },
                        "wait_for_answer": false
                    },
                    {
                        "kind": "pause_ms",
                        "ms": 150
                    },
                    {
                        "kind": "subagent_end",
                        "subagent": "ses_sub_cancel_1",
                        "status": "cancelled"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, Some(300));
    let mut peer = handshake(&lever.person);
    let (thread, _turn) = send(&mut peer, None, "Go");

    let req = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    let req_id = req["request"].as_str().unwrap().to_string();

    // Subagent ends with status "cancelled" without cancelling the request first:
    // Lever withdraws the waiting request and maps cancelled -> stopped.
    let withdrawn = read_until(&mut peer, |v| {
        v["event"] == "request" && v["request"] == req_id && v["state"] == "withdrawn"
    });
    assert_eq!(withdrawn["state"], "withdrawn");
    let ended = read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] != "working");
    assert_eq!(ended["state"], "stopped");

    let mut inspector = handshake(&lever.person);
    let rec = resume(&mut inspector, &thread);
    assert_eq!(rec["working"], false);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "stopped");
    assert_eq!(rec["turns"][0]["requests"][0]["state"], "withdrawn");

    // Because the subagent ended and its request was withdrawn, the harness idles out.
    wait_until("idle harness stops after cancelled subagent", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_says_after_a_subagent_reports_is_a_turn_of_its_own_that_ends_when_quiet() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_wake_1", "title": "Look around", "prompt": "Look around" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 100 },
                    { "kind": "subagent_text", "subagent": "ses_wake_1", "text": "Found it." },
                    { "kind": "subagent_end", "subagent": "ses_wake_1", "status": "completed" },
                    // OpenCode wakes the parent session with the report, and
                    // its Model answers there.
                    { "kind": "text", "text": "The subagent reported." },
                    { "kind": "pause_ms", "ms": 900 },
                    { "kind": "text", "text": " One more thing." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look around");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);

    let woke = read_until(&mut peer, |v| v["event"] == "message");
    assert_eq!(woke["origin"], "harness");
    assert_eq!(woke["turn"], turn + 1, "the harness's turn is a new one: {woke}");
    assert_eq!(woke["text"], "");
    let text = read_until(&mut peer, |v| v["event"] == "text");
    assert_eq!(
        (text["turn"].clone(), text["text"].clone()),
        (json!(turn + 1), json!("The subagent reported."))
    );
    // No end comes from the harness: Lever ends the turn once it is quiet.
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);

    // What comes after reopens that turn, the thread's newest.
    let again = read_until(&mut peer, |v| v["event"] == "message");
    assert_eq!(
        (again["turn"].clone(), again["origin"].clone()),
        (json!(turn + 1), json!("harness"))
    );
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);

    let rec = resume(&mut peer, &thread);
    let turns = rec["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{rec}");
    assert_eq!(
        turns[0]["answer"], "Handed off.",
        "the person's turn keeps only its own answer"
    );
    assert_eq!(turns[0].get("origin"), None);
    assert_eq!(turns[1]["origin"], "harness");
    assert_eq!(turns[1]["message"], "");
    assert_eq!(turns[1]["answer"], "The subagent reported. One more thing.");
    assert_eq!(turns[1]["state"], "done");
    assert_eq!(rec["working"], false);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_is_sent_while_a_subagent_works_and_takes_over_a_turn_the_harness_began() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_take_1", "title": "Dig", "prompt": "Dig in" },
                        { "kind": "text", "text": "Started." }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 800 },
                        { "kind": "subagent_end", "subagent": "ses_take_1", "status": "completed" },
                        { "kind": "text", "text": "Report in." }
                    ]
                },
                { "match": "second", "steps": [{ "kind": "text", "text": "Second answer." }] },
                { "match": "third", "steps": [{ "kind": "text", "text": "Third answer." }] }
            ]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);

    // The subagent works on, and the person's message is sent all the same.
    let (_, second) = send(&mut peer, Some(&thread), "second");
    assert_eq!(second, turn + 1);
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second);

    // The subagent reports, the harness begins a turn, and the person's
    // next message takes it over.
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], second + 1);
    read_until(&mut peer, |v| v["event"] == "text" && v["text"] == "Report in.");
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "third", "cwd": "/tmp" }),
    )
    .unwrap();
    let mut ended_woken = false;
    let sent = read_until(&mut peer, |v| {
        if v["event"] == "done" && v["turn"] == second + 1 {
            ended_woken = true;
        }
        v.get("reply").is_some()
    });
    assert_eq!(sent["reply"], "sent", "{sent}");
    assert_eq!(sent["turn"], second + 2);
    if !ended_woken {
        read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second + 1);
    }
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second + 2);

    let rec = resume(&mut peer, &thread);
    let answers: Vec<&str> = rec["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["answer"].as_str().unwrap())
        .collect();
    assert_eq!(answers, ["Started.", "Second answer.", "Report in.", "Third answer."]);
    assert_eq!(rec["turns"][2]["origin"], "harness");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_first_heard_of_after_its_turn_was_stopped_is_stopped() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [{ "kind": "text", "text": "Working." }],
                "stall": true,
                // The harness says of the subagent only after it answered the Stop.
                "after": [
                    { "kind": "subagent_start", "id": "ses_late_1", "title": "Late", "prompt": "Go on" },
                    { "kind": "subagent_stall", "subagent": "ses_late_1" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "3000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Work");
    read_until(&mut peer, |v| v["event"] == "text");
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn);
    let ended = read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] != "working");
    assert_eq!(ended["state"], "stopped", "{ended}");

    let mut inspector = handshake(&lever.person);
    write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut inspector).unwrap();
    assert_eq!(
        status["harness_started"], true,
        "the harness stopped the subagent when told: {status}"
    );
    let rec = resume(&mut inspector, &thread);
    assert_eq!(rec["working"], false);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "stopped");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_that_launches_a_subagent_shows_it_and_names_no_session() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_outer_secret_7", "title": "Outer", "prompt": "Plan it" },
                    {
                        "kind": "subagent_start",
                        "id": "ses_inner_secret_8",
                        "title": "Inner",
                        "prompt": "Do the part",
                        "launched_by": "ses_outer_secret_7"
                    },
                    { "kind": "subagent_text", "subagent": "ses_inner_secret_8", "text": "Part done." },
                    { "kind": "subagent_end", "subagent": "ses_inner_secret_8", "status": "completed" },
                    { "kind": "subagent_end", "subagent": "ses_outer_secret_7", "status": "completed" },
                    { "kind": "text", "text": "All done." }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Plan");
    let mut frames = Vec::new();
    read_until(&mut peer, |v| {
        frames.push(v.clone());
        v["event"] == "done" && v["turn"] == turn
    });
    let rec = resume(&mut peer, &thread);
    frames.push(rec.clone());
    for frame in &frames {
        let said = frame.to_string();
        assert!(
            !said.contains("secret") && !said.contains("sessionID"),
            "a harness session crossed Lever's socket: {said}"
        );
    }
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    let titles: Vec<&str> = subs.iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Outer", "Inner"], "{rec}");
    assert_eq!(
        subs[0]["acts"],
        json!([]),
        "the launcher is no act of the outer subagent: {rec}"
    );
    assert_eq!(
        subs[1]["messages"],
        json!([
            { "role": "prompt", "text": "Do the part" },
            { "role": "answer", "text": "Part done." }
        ])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_launch_the_harness_has_yet_to_report_lends_its_prompt_to_no_other_subagent() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_alpha", "title": "Alpha", "prompt": "Alpha prompt" },
                    {
                        "kind": "subagent_start",
                        "id": "ses_beta",
                        "title": "Beta",
                        "prompt": "Beta prompt",
                        "hold_launcher": true
                    },
                    // Alpha's status comes again while Beta's launch names no child.
                    { "kind": "subagent_end", "subagent": "ses_alpha", "status": "running" },
                    { "kind": "subagent_end", "subagent": "ses_alpha", "status": "completed" },
                    { "kind": "text", "text": "ok" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Launch two");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let rec = resume(&mut peer, &thread);
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1, "{rec}");
    assert_eq!(subs[0]["title"], "Alpha");
    assert_eq!(
        subs[0]["messages"],
        json!([{ "role": "prompt", "text": "Alpha prompt" }])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_act_of_one_thread_is_not_taken_for_another_threads_launch_with_the_same_call_id() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "alpha",
                    "steps": [
                        {
                            "kind": "subagent_start",
                            "id": "ses_hold",
                            "title": "Held",
                            "prompt": "Wait",
                            "hold_launcher": true
                        },
                        { "kind": "text", "text": "Launching." }
                    ]
                },
                {
                    "match": "beta",
                    "steps": [
                        // A call ID is unique only within its session.
                        { "kind": "act", "id": "call_subagent_ses_hold", "tool_kind": "read", "title": "List files", "status": "pending" },
                        { "kind": "act_update", "id": "call_subagent_ses_hold", "status": "completed", "content": "a.txt" },
                        { "kind": "text", "text": "Listed." }
                    ]
                }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (_, first) = send(&mut peer, None, "alpha");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let (thread_b, turn_b) = send(&mut peer, None, "beta");
    read_until(&mut peer, |v| {
        v["event"] == "done" && v["thread"] == thread_b && v["turn"] == turn_b
    });
    let rec = resume(&mut peer, &thread_b);
    let acts = rec["turns"][0]["acts"].as_array().unwrap();
    assert_eq!(acts.len(), 1, "{rec}");
    assert_eq!(acts[0]["state"], "done", "{rec}");
    assert_eq!(acts[0]["detail"], "a.txt");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_id_from_a_harness_that_ended_routes_no_new_session_to_its_thread() {
    let dir = TempDir::new();
    let uid = my_uid();
    // Every stub gives the same IDs; the first stub's subagent has the ID the
    // second stub gives a new session.
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "s-",
            "load_session": false,
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "s-2", "title": "Old helper", "prompt": "Help" },
                        { "kind": "subagent_end", "subagent": "s-2", "status": "completed" },
                        { "kind": "text", "text": "First." }
                    ]
                },
                { "match": "again", "steps": [{ "kind": "text", "text": "Again." }] },
                { "match": "other", "steps": [{ "kind": "text", "text": "Other thread's answer." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, Some(200));
    let mut peer = handshake(&lever.person);
    let (thread_a, first) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let mut inspector = handshake(&lever.person);
    wait_until("the idle harness to stop", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });

    // A new harness: thread A's session is s-1 again, and thread B's is s-2.
    let (_, again) = send(&mut peer, Some(&thread_a), "again");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == again);
    let (thread_b, other) = send(&mut peer, None, "other");
    read_until(&mut peer, |v| {
        v["event"] == "done" && v["thread"] == thread_b && v["turn"] == other
    });

    let rec_b = resume(&mut peer, &thread_b);
    assert_eq!(rec_b["turns"][0]["answer"], "Other thread's answer.", "{rec_b}");
    let rec_a = resume(&mut peer, &thread_a);
    let subs = rec_a["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(
        subs[0]["messages"],
        json!([{ "role": "prompt", "text": "Help" }]),
        "{rec_a}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_launch_that_fails_ends_its_subagent_and_an_end_said_twice_is_said_once() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_fails_secret", "title": "Doomed", "prompt": "Try", "launch_fails": true },
                    { "kind": "subagent_start", "id": "ses_twice", "title": "Twice", "prompt": "Go" },
                    { "kind": "subagent_end", "subagent": "ses_twice", "status": "completed" },
                    { "kind": "subagent_end", "subagent": "ses_twice", "status": "completed" },
                    { "kind": "text", "text": "ok" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Launch");
    let mut frames = Vec::new();
    read_until(&mut peer, |v| {
        frames.push(v.clone());
        v["event"] == "done" && v["turn"] == turn
    });
    let rec = resume(&mut peer, &thread);
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs[0]["title"], "Doomed");
    assert_eq!(
        subs[0]["state"], "failed",
        "a launch that failed leaves no subagent working: {rec}"
    );
    assert!(!subs[0].to_string().contains("secret"), "{rec}");
    assert_eq!(rec["working"], false);
    let twice_done = frames
        .iter()
        .filter(|f| f["event"] == "subagent" && f["title"] == "Twice" && f["state"] == "done")
        .count();
    assert_eq!(twice_done, 1, "{frames:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_a_quiet_turn_the_harness_began_ends_it_and_no_message_takes_it_over_meanwhile() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quiet_stop", "title": "Look", "prompt": "Look" },
                    { "kind": "subagent_end", "subagent": "ses_quiet_stop", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [{ "kind": "text", "text": "The subagent reported." }]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], turn + 1);
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == turn + 1);

    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    // A message while the Stop goes through does not take the turn over.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "more", "cwd": "/tmp" }),
    )
    .unwrap();
    let refused = read_until(&mut peer, |v| v.get("reply").is_some() && v["reply"] != "ok");
    assert_eq!(
        (refused["reply"].clone(), refused["code"].clone()),
        (json!("error"), json!("working")),
        "{refused}"
    );
    let stopped = read_until(&mut peer, |v| {
        matches!(v["event"].as_str(), Some("stopped" | "done" | "interrupted"))
    });
    assert_eq!(
        (stopped["event"].clone(), stopped["turn"].clone()),
        (json!("stopped"), json!(turn + 1))
    );

    // The harness said nothing after the Stop: it is not stopped for it.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_until(&mut peer, |v| v["reply"] == "status");
    assert_eq!(status["harness_started"], true, "{status}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_subagent_first_asks_after_its_work_was_stopped_is_asked_of_nobody() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [{ "kind": "text", "text": "Working." }],
                "stall": true,
                "after": [{
                    "kind": "subagent_ask",
                    "subagent": "ses_asks_late",
                    "subagent_title": "Late",
                    "id": "call_late_read",
                    "tool_kind": "read",
                    "title": "Read /tmp/.env",
                    "raw_input": { "path": "/tmp/.env" },
                    "output": "SECRET=1"
                }]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "800"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Work");
    read_until(&mut peer, |v| v["event"] == "text");
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn);
    let mut asked = false;
    read_until(&mut peer, |v| {
        asked |= v["event"] == "request" && v["state"] == "waiting";
        v["event"] == "subagent" && v["state"] != "working"
    });
    assert!(!asked, "a stopped subagent's request was put to the person");
    let rec = resume(&mut peer, &thread);
    assert!(
        rec["turns"][0]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["state"] != "waiting"),
        "{rec}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_in_the_name_of_another_threads_subagent_is_put_to_nobody() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "alpha",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_owned_by_a", "title": "A's helper", "prompt": "Help A" },
                        { "kind": "text", "text": "A." }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 1500 },
                        { "kind": "subagent_end", "subagent": "ses_owned_by_a", "status": "completed" }
                    ]
                },
                {
                    "match": "beta",
                    "steps": [
                        {
                            "kind": "subagent_ask",
                            "subagent": "ses_owned_by_a",
                            "subagent_title": "A's helper",
                            "id": "call_cross",
                            "tool_kind": "read",
                            "title": "Read /tmp/a",
                            "raw_input": { "path": "/tmp/a" },
                            "output": "a"
                        },
                        { "kind": "text", "text": "B." }
                    ]
                }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (_, first) = send(&mut peer, None, "alpha");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let (thread_b, turn_b) = send(&mut peer, None, "beta");
    let mut asked = false;
    read_until(&mut peer, |v| {
        asked |= v["event"] == "request" && v["thread"] == thread_b;
        v["event"] == "done" && v["thread"] == thread_b && v["turn"] == turn_b
    });
    assert!(!asked, "thread B was asked in the name of thread A's subagent");
    let rec = resume(&mut peer, &thread_b);
    assert_eq!(rec["turns"][0]["requests"], json!([]), "{rec}");
    assert_eq!(rec["turns"][0]["subagents"], json!([]), "{rec}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_that_starts_and_asks_while_its_thread_is_deleted_goes_nowhere() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": true,
            // The harness takes its time over the delete, and meanwhile a
            // subagent of the thread starts and asks.
            "delete_delay_ms": 1500,
            "turns": [{
                "steps": [{ "kind": "text", "text": "Done." }],
                "after": [
                    { "kind": "pause_ms", "ms": 300 },
                    { "kind": "subagent_start", "id": "ses_late", "title": "Late", "prompt": "Late work" },
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_late",
                        "id": "call_late",
                        "tool_kind": "read",
                        "title": "Read /tmp/late",
                        "raw_input": { "path": "/tmp/late" },
                        "output": "late"
                    },
                    { "kind": "subagent_end", "subagent": "ses_late", "status": "completed" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    write_frame(&mut peer, &json!({ "op": "delete", "thread": thread })).unwrap();
    let mut told = Vec::new();
    let reply = read_until(&mut peer, |v| {
        if v["event"] == "subagent" || v["event"] == "request" || v["event"] == "message" {
            told.push(v.clone());
        }
        v.get("reply").is_some()
    });
    assert_eq!(reply["reply"], "deleted", "{reply}");
    assert!(told.is_empty(), "a thread being deleted told of its harness: {told:?}");
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let subagents: i64 = db
        .query_row("SELECT count(*) FROM subagents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(subagents, 0);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_a_turn_the_harness_began_spares_an_older_turns_subagent_and_the_harness() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quick", "title": "Quick", "prompt": "Quick work" },
                    { "kind": "subagent_start", "id": "ses_slow", "title": "Slow", "prompt": "Slow work" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_quick", "status": "completed" },
                    { "kind": "text", "text": "Quick reported." },
                    // The person stops the turn the harness began here. The
                    // slow subagent of the first turn asks while that Stop
                    // waits, and goes on past the stop timeout.
                    { "kind": "pause_ms", "ms": 600 },
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_slow",
                        "id": "call_slow",
                        "tool_kind": "read",
                        "title": "Read /tmp/slow",
                        "raw_input": { "path": "/tmp/slow" },
                        "output": "slow"
                    },
                    { "kind": "pause_ms", "ms": 600 },
                    { "kind": "subagent_end", "subagent": "ses_slow", "status": "completed" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1200", "--wake-quiet-ms", "5000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    assert_ne!(second, first);
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": second })).unwrap();
    read_until(&mut peer, |v| v["reply"] == "ok");
    let (mut asked, mut stopped) = (None, false);
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "subagent" && v["state"] == "interrupted"),
            "the older turn's subagent was cut off: {v}"
        );
        assert!(
            !(v["event"] == "request" && v["state"] == "withdrawn"),
            "the older turn's subagent's request was withdrawn: {v}"
        );
        if v["event"] == "request" && v["state"] == "waiting" {
            asked = Some(v.clone());
        }
        stopped |= v["event"] == "stopped" && v["turn"] == second;
        asked.is_some() && stopped
    });
    let asked = asked.unwrap();
    assert_eq!(asked["turn"], first, "{asked}");
    let request = asked["request"].as_str().unwrap();
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": request, "answer": "once" }),
    )
    .unwrap();
    let ended = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Slow" && v["state"] != "working"
    });
    assert_eq!(ended["state"], "done", "{ended}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_whose_end_is_unsaved_stays_ended_though_it_says_more() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_said_more", "title": "Talker", "prompt": "Talk" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 400 },
                    { "kind": "subagent_end", "subagent": "ses_said_more", "status": "completed" },
                    // A last word of the subagent's, after the harness said
                    // it ended, while the database still refuses that end.
                    { "kind": "pause_ms", "ms": 200 },
                    { "kind": "subagent_text", "subagent": "ses_said_more", "text": "One more thing." }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_subagent_end BEFORE UPDATE OF state ON subagents \
         WHEN NEW.state <> 'working' BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] == "done");
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "subagent" && v["state"] == "working"),
            "the ended subagent went back to work: {v}"
        );
        v["event"] == "subagent-text"
    });
    db.execute_batch("DROP TRIGGER refuse_subagent_end").unwrap();
    // The end is saved on Lever's next try, and the thread is free.
    std::thread::sleep(Duration::from_millis(600));
    let state: String = db.query_row("SELECT state FROM subagents", [], |r| r.get(0)).unwrap();
    assert_eq!(state, "done");
    drop(db);
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    let listed = read_until(&mut peer, |v| v["reply"] == "list");
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    write_frame(&mut peer, &json!({ "op": "delete", "thread": thread })).unwrap();
    let reply = read_until(&mut peer, |v| v.get("reply").is_some());
    assert_eq!(reply["reply"], "deleted", "{reply}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_holds_against_a_turn_the_harness_begins_on_a_stopped_subagents_report() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_dig_1", "title": "Dig", "prompt": "Dig" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    // Stopped, the subagent reports, and the harness answers
                    // its report with another try.
                    { "kind": "subagent_stall", "subagent": "ses_dig_1" },
                    { "kind": "text", "text": "The subagent was cancelled; trying again." },
                    { "kind": "subagent_start", "id": "ses_dig_2", "title": "Dig again", "prompt": "Dig again" },
                    { "kind": "subagent_stall", "subagent": "ses_dig_2" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "400", "--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    // As the drawer does, by the turn it shows.
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": first })).unwrap();
    read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Dig" && v["state"] == "stopped"
    });
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    let again = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Dig again" && v["state"] != "working"
    });
    assert_eq!(again["state"], "stopped", "{again}");
    let ended = read_until(&mut peer, |v| {
        v["turn"] == second && ["done", "stopped", "failed", "interrupted"].contains(&v["event"].as_str().unwrap_or(""))
    });
    assert_eq!(ended["event"], "stopped", "{ended}");
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    // It said nothing once told to stop: the harness is spared.
    assert_eq!(ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"], true);

    // The person's next message lifts the Stop.
    let (_, next) = send(&mut peer, Some(&thread), "Go on");
    assert!(next > second);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_a_newer_turn_is_not_taken_on_by_an_older_turns_subagent_that_says_more() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_brief", "title": "Brief", "prompt": "Brief work" },
                    { "kind": "subagent_start", "id": "ses_long", "title": "Long", "prompt": "Long work" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_brief", "status": "completed" },
                    { "kind": "text", "text": "Brief reported." },
                    // While the Stop of the turn the harness began waits, the
                    // first turn's long subagent says it still runs, and it
                    // ends well past the stop timeout.
                    { "kind": "pause_ms", "ms": 300 },
                    { "kind": "subagent_end", "subagent": "ses_long", "status": "running" },
                    { "kind": "pause_ms", "ms": 1200 },
                    { "kind": "subagent_end", "subagent": "ses_long", "status": "completed" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "600", "--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": second })).unwrap();
    let ended = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Long" && v["state"] != "working"
    });
    assert_eq!(ended["state"], "done", "{ended}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_act_a_request_is_the_first_word_of_holds_a_turn_the_harness_began_while_it_runs() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_asker", "title": "Look", "prompt": "Look" },
                    { "kind": "subagent_end", "subagent": "ses_asker", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "text", "text": "Reported." },
                    {
                        "kind": "ask",
                        "id": "call_make",
                        "title": "make",
                        "raw_input": { "command": "make" },
                        "request_without_call": true,
                        "run_ms": 900
                    },
                    { "kind": "text", "text": " Done." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    )
    .unwrap();
    let mut ended_early = false;
    read_until(&mut peer, |v| {
        ended_early |= v["event"] == "done" && v["turn"] == second;
        v["event"] == "text" && v["text"] == " Done."
    });
    assert!(!ended_early, "the turn ended while the act it was asked about ran");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_harness_began_waits_its_quiet_from_when_its_subagent_ended() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "One" },
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "text", "text": "Reported." },
                    // The harness's turn starts another subagent, which runs past
                    // the quiet period, and answers its report soon after.
                    { "kind": "subagent_start", "id": "ses_second", "title": "Second", "prompt": "Two" },
                    { "kind": "pause_ms", "ms": 900 },
                    { "kind": "subagent_end", "subagent": "ses_second", "status": "completed" },
                    { "kind": "pause_ms", "ms": 150 },
                    { "kind": "text", "text": " And the second." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "500"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let mut ended_early = false;
    read_until(&mut peer, |v| {
        ended_early |= v["event"] == "done" && v["turn"] == turn + 1;
        v["event"] == "text" && v["text"] == " And the second."
    });
    assert!(
        !ended_early,
        "the turn ended before the harness answered its subagent's report"
    );
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);
    let rec = resume(&mut peer, &thread);
    assert_eq!(rec["turns"][1]["answer"], "Reported. And the second.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// A script whose first turn hands off to a subagent that ends at once, so
/// the harness's report comes as a turn of its own, followed by `after`.
fn wake_script(dir: &Path, after: Value) -> PathBuf {
    write_script(
        dir,
        &json!({
            "turns": [
                {
                    "match": "Look",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_wake_helper", "title": "Look", "prompt": "Look" },
                        { "kind": "subagent_end", "subagent": "ses_wake_helper", "status": "completed" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": after
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    )
}

#[test]
fn a_subagent_stopped_with_a_quiet_harness_turn_spares_the_harness() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            { "kind": "subagent_start", "id": "ses_lingers", "title": "Linger", "prompt": "Wait" },
            { "kind": "subagent_stall", "subagent": "ses_lingers" }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Linger" && v["state"] == "working"
    });
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    // The subagent stops when told, and the harness says nothing more.
    read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Linger" && v["state"] == "stopped"
    });
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn + 1);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_until(&mut peer, |v| v["reply"] == "status");
    assert_eq!(
        status["harness_started"], true,
        "the harness that stopped was stopped: {status}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_pending_act_of_a_harness_turn_holds_it_open() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            { "kind": "act", "id": "call_slow", "tool_kind": "command", "title": "slow", "status": "pending" },
            { "kind": "pause_ms", "ms": 800 },
            { "kind": "act_update", "id": "call_slow", "status": "completed", "content": "ok" },
            { "kind": "text", "text": " Done." }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "400"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (_, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let mut ended_early = false;
    read_until(&mut peer, |v| {
        ended_early |= v["event"] == "done" && v["turn"] == turn + 1;
        v["event"] == "text" && v["text"] == " Done."
    });
    assert!(!ended_early, "the turn ended while its act was pending");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_turn_whose_end_or_answer_the_database_refuses_begins_no_turn_after_it_and_fails() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(&dir.0, json!([{ "kind": "text", "text": "Reported." }]));
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == turn + 1);

    // The end of the harness's turn is refused: the person's message waits.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_end BEFORE UPDATE OF state ON turns WHEN OLD.origin = 'harness' \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp" }),
    )
    .unwrap();
    let refused = read_until(&mut peer, |v| v.get("reply").is_some());
    assert_eq!(refused["code"], "storage-error", "{refused}");
    db.execute_batch("DROP TRIGGER refuse_end").unwrap();
    let turns: i64 = db
        .query_row("SELECT COUNT(*) FROM turns WHERE thread_id = ?1", [&thread], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(turns, 2, "no turn began after the one whose end is unsaved");
    drop(db);
    let (_, next) = send(&mut peer, Some(&thread), "Next");
    assert_eq!(next, turn + 2);
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_turn_whose_answer_the_database_refuses_ends_failed() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(&dir.0, json!([{ "kind": "text", "text": "Reported." }]));
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_answer BEFORE UPDATE OF answer ON turns WHEN OLD.origin = 'harness' \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    let mut peer = handshake(&lever.person);
    let (_, turn) = send(&mut peer, None, "Look");
    let end = read_until(&mut peer, |v| {
        v["turn"] == turn + 1 && matches!(v["event"].as_str(), Some("done" | "failed"))
    });
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "Lever could not save all of this turn.");
    drop(db);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_new_harness_says_after_a_turn_opens_no_harness_turn_for_an_old_harnesss_subagent() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_old_helper", "title": "Old", "prompt": "Help" },
                        { "kind": "subagent_end", "subagent": "ses_old_helper", "status": "completed" },
                        { "kind": "text", "text": "First." }
                    ]
                },
                {
                    "match": "again",
                    "steps": [{ "kind": "text", "text": "Again." }],
                    "after": [{ "kind": "pause_ms", "ms": 100 }, { "kind": "text", "text": " Late." }]
                }
            ]
        }),
    );
    let extra = stub_extra(&script, &["--idle-timeout-ms", "200", "--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let mut inspector = handshake(&lever.person);
    wait_until("the idle harness to stop", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });
    let (_, again) = send(&mut peer, Some(&thread), "again");
    let mut woke = false;
    read_until(&mut peer, |v| {
        woke |= v["event"] == "message" && v["origin"] == "harness";
        v["event"] == "text" && v["text"] == " Late."
    });
    assert!(!woke, "a harness with no subagent of the thread began a turn");
    let rec = resume(&mut peer, &thread);
    assert_eq!(rec["turns"].as_array().unwrap().len(), 2, "{rec}");
    assert_eq!(rec["turns"][1]["turn"], again);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_act_a_stopped_harness_turn_closes_spares_the_harness() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            { "kind": "act", "id": "call_cut", "tool_kind": "command", "title": "long", "status": "in_progress" },
            { "kind": "pause_ms", "ms": 400 },
            // The harness closes the act when it stops, and says nothing more.
            { "kind": "act_update", "id": "call_cut", "status": "failed", "content": "Cancelled" }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "900"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| {
        v["event"] == "act" && v["turn"] == turn + 1 && v["state"] == "working"
    });
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut peer, |v| {
        v["event"] == "act" && v["turn"] == turn + 1 && v["state"] == "failed"
    });
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn + 1);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_until(&mut peer, |v| v["reply"] == "status");
    assert_eq!(status["harness_started"], true, "{status}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_that_begins_a_turn_under_a_held_stop_asks_nothing() {
    let dir = TempDir::new();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "pause_ms", "ms": 300 },
            { "kind": "ask", "id": "call_wake", "title": "make", "raw_input": { "command": "make" }, "request_without_call": true }
        ]),
    );
    let mut config: Value = serde_json::from_slice(&std::fs::read(&script).unwrap()).unwrap();
    config["modes"] = json!(true);
    let script = write_script(&dir.0, &config);
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "request" && v["state"] == "waiting") || (v["event"] == "stopped" && v["turn"] == turn + 1)
    });
    assert_eq!(got["event"], "stopped", "a request slipped past Stop: {got}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_the_harness_launches_under_a_held_stop_before_it_says_anything_is_stopped() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_stall", "subagent": "ses_first" },
                    { "kind": "subagent_start", "id": "ses_second", "title": "Second", "prompt": "Second" },
                    { "kind": "subagent_ask", "subagent": "ses_second", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_stall", "subagent": "ses_second" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": turn }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "request" && v["state"] == "waiting")
            || (v["event"] == "subagent" && v["title"] == "Second" && v["state"] == "stopped")
    });
    assert_eq!(got["event"], "subagent", "the retry slipped past Stop: {got}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_nested_subagent_of_an_older_turns_subagent_is_not_stopped_with_a_newer_turn() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "subagent_start", "id": "ses_older", "title": "Older", "prompt": "Older" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "First reported." },
                    { "kind": "pause_ms", "ms": 300 },
                    { "kind": "subagent_start", "id": "ses_nested", "title": "Nested", "prompt": "Nested", "launched_by": "ses_older" },
                    { "kind": "subagent_ask", "subagent": "ses_nested", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_stall", "subagent": "ses_nested" },
                    { "kind": "subagent_end", "subagent": "ses_older", "status": "completed" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000", "--wake-quiet-ms", "5000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let newer = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": newer }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "subagent" && v["title"] == "Nested" && v["state"] == "stopped")
            || (v["event"] == "request" && v["state"] == "waiting")
    });
    assert_eq!(
        got["event"], "request",
        "the newer Stop cancelled an older turn's nested subagent: {got}"
    );
    assert_eq!(got["turn"], turn);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_nested_subagent_told_of_before_its_launcher_names_it_keeps_its_launchers_turn() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "subagent_start", "id": "ses_older", "title": "Older", "prompt": "Older" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "First reported." },
                    { "kind": "pause_ms", "ms": 300 },
                    // OpenCode tells of the child before the launcher's output
                    // names it.
                    {
                        "kind": "subagent_start", "id": "ses_nested", "title": "Nested", "prompt": "Nested",
                        "launched_by": "ses_older", "name_after_ms": 300
                    },
                    { "kind": "subagent_ask", "subagent": "ses_nested", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_stall", "subagent": "ses_nested" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1500", "--wake-quiet-ms", "5000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let newer = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": newer }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "subagent" && v["title"] == "Nested" && v["state"] == "stopped")
            || (v["event"] == "request" && v["state"] == "waiting")
    });
    assert_eq!(
        got["event"], "request",
        "the newer Stop stopped an older turn's nested subagent: {got}"
    );
    assert_eq!(got["turn"], turn);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_each_turn_in_turn_holds_as_a_stop_of_the_thread_does() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quick", "title": "Quick", "prompt": "Quick" },
                    { "kind": "subagent_start", "id": "ses_slow", "title": "Slow", "prompt": "Slow" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_quick", "status": "completed" },
                    { "kind": "text", "text": "Quick reported." },
                    // Stopped after the turn the harness began was, the slow
                    // subagent reports, and the harness answers well after
                    // that turn ended.
                    { "kind": "subagent_stall", "subagent": "ses_slow" },
                    { "kind": "pause_ms", "ms": 1200 },
                    { "kind": "text", "text": "Slow reported." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "600", "--wake-quiet-ms", "400"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": second }))["reply"],
        "ok"
    );
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": first }))["reply"],
        "ok"
    );
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == second);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let third = woke["turn"].as_u64().unwrap();
    let ended = read_until(&mut peer, |v| {
        v["turn"] == third && ["done", "stopped", "failed", "interrupted"].contains(&v["event"].as_str().unwrap_or(""))
    });
    assert_eq!(ended["event"], "stopped", "{ended}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_quiet_stopped_turn_ends_without_waiting_on_an_older_turns_subagent_stopped_after_it() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quick", "title": "Quick", "prompt": "Quick" },
                    { "kind": "subagent_start", "id": "ses_slow", "title": "Slow", "prompt": "Slow" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_quick", "status": "completed" },
                    { "kind": "text", "text": "Quick reported." },
                    // Told to stop half a stop timeout after the turn the
                    // harness began, the slow subagent stops within its own.
                    { "kind": "subagent_stall", "subagent": "ses_slow", "linger_ms": 800 }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000", "--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": second }))["reply"],
        "ok"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": first }))["reply"],
        "ok"
    );
    let ended = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Slow" && v["state"] != "working"
    });
    assert_eq!(ended["state"], "stopped", "{ended}");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"],
        true,
        "the harness was stopped before the subagent had its time"
    );
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_of_a_subagent_that_has_ended_is_put_to_nobody() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_done", "title": "Done", "prompt": "Done" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_done", "status": "completed" },
                    { "kind": "subagent_ask", "subagent": "ses_done", "id": "call_late", "tool_kind": "read",
                      "title": "Read /tmp/late", "raw_input": { "path": "/tmp/late" }, "output": "late" },
                    { "kind": "text", "text": "Fin." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "Go");
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "request" && v["state"] == "waiting"),
            "a subagent that ended asked: {v}"
        );
        v["event"] == "text" && v["text"] == "Fin."
    });
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_mode_set_while_a_subagent_works_is_the_sessions_before_the_harness_goes_on() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_waits", "title": "Waits", "prompt": "Wait" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_ask", "subagent": "ses_waits", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_end", "subagent": "ses_waits", "status": "completed" },
                    // The harness answers the report with a command, in the
                    // mode its session is in.
                    { "kind": "text", "text": "Reported." },
                    { "kind": "ask", "id": "call_make", "title": "make", "raw_input": { "command": "make" } }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "Go", "cwd": "/tmp", "mode": "full-access" }),
    )
    .unwrap();
    let sent = read_until(&mut peer, |v| v.get("reply").is_some());
    let thread = sent["thread"].as_str().unwrap().to_string();
    let turn = sent["turn"].as_u64().unwrap();
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // Ask first, while the subagent waits: the mode is the thread's once the
    // session has it.
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "ask-first" }),
    );
    assert_eq!(set["reply"], "mode-set", "{set}");
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    )
    .unwrap();
    let make = read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "act" && v["title"] == "make" && v["state"] == "done"),
            "the command ran without asking: {v}"
        );
        v["event"] == "request" && v["state"] == "waiting" && v["title"] == "make"
    });
    assert_eq!(make["turn"], turn + 1, "{make}");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["mode"], "ask-first", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_taken_up_again_after_its_harness_restarted_is_not_shown_working_on() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_again", "title": "Again", "prompt": "Look" },
                        { "kind": "subagent_end", "subagent": "ses_again", "status": "completed" },
                        { "kind": "text", "text": "Looked." }
                    ]
                },
                {
                    "match": "again",
                    "steps": [
                        // As OpenCode 2.4.1 does after it restarted: only the
                        // launcher names the child it takes up again.
                        { "kind": "subagent_start", "id": "ses_again", "title": "Again", "prompt": "Look again", "unfollowed": true },
                        { "kind": "text", "text": "Took it up again." }
                    ],
                    // It goes on, and reports after the person stopped the
                    // thread, within the harness's idle timeout.
                    "after": [
                        { "kind": "pause_ms", "ms": 200 },
                        { "kind": "text", "text": "Late report." }
                    ]
                }
            ]
        }),
    );
    let extra = stub_extra(
        &script,
        &[
            "--idle-timeout-ms",
            "300",
            "--wake-quiet-ms",
            "60000",
            "--stop-timeout-ms",
            "400",
        ],
    );
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let started = std::time::Instant::now();
    while ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == true {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the harness was never stopped as idle"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let (_, again) = send(&mut peer, Some(&thread), "again");
    // A subagent keeps the turn that first started it.
    let shown = read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "done" && v["turn"] == again),
            "the turn ended with no word of its subagent"
        );
        v["event"] == "subagent" && v["title"] == "Again"
    });
    assert_eq!(shown["state"], "failed", "{shown}");
    assert!(
        shown["error"].as_str().unwrap_or("").contains("cannot be followed"),
        "{shown}"
    );
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == again);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );

    // Its report is a turn the harness began, which the Stop stops.
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], again + 1, "{woke}");
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == again + 1);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["answer"], "Took it up again.", "{record}");
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_held_stop_catches_after_a_turn_the_harness_began_ended_is_a_turn_of_its_own() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_told", "title": "Told", "prompt": "Tell" },
                    { "kind": "subagent_end", "subagent": "ses_told", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "text", "text": "Reported." },
                    // That turn goes quiet and ends; the person stops the
                    // thread; the harness says more.
                    { "kind": "pause_ms", "ms": 1200 },
                    { "kind": "text", "text": "More." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300", "--stop-timeout-ms", "400"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first + 1);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], first + 2, "{woke}");
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == first + 2);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["state"], "done", "{record}");
    assert_eq!(record["turns"][1]["answer"], "Reported.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_after_a_stop_of_a_thread_at_rest_is_put_to_nobody() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "text", "text": "Done." }],
                // The harness asks after its turn ended, and after the person
                // stopped the thread, with nothing of it working, so no
                // session was told to stop.
                "after": [
                    { "kind": "pause_ms", "ms": 600 },
                    { "kind": "ask", "id": "call_late", "title": "make", "raw_input": { "command": "make" } }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &[]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    std::thread::sleep(Duration::from_millis(1000));
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "request" && v["state"] == "waiting"),
            "a request came to the person under a held Stop: {v}"
        );
        v["reply"] == "status"
    });
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["requests"], json!([]), "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_mode_the_record_does_not_take_is_taken_back_from_the_session() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_waits", "title": "Waits", "prompt": "Wait" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_ask", "subagent": "ses_waits", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_end", "subagent": "ses_waits", "status": "completed" },
                    { "kind": "text", "text": "Reported." },
                    { "kind": "ask", "id": "call_make", "title": "make", "raw_input": { "command": "make" } }
                ]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // The harness takes Full access, and the record does not.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_mode BEFORE UPDATE OF mode ON threads \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    assert_eq!(set["code"], "storage-error", "{set}");
    wait_until("the session goes back to Ask first", || {
        stub_modes_set(&stub_log) == ["build", "crowbar-ask-first"]
    });
    db.execute_batch("DROP TRIGGER refuse_mode").unwrap();
    drop(db);

    // What the harness goes on with runs in Ask first.
    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    let make = read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "act" && v["title"] == "make" && v["state"] == "done"),
            "the command ran without asking: {v}"
        );
        v["event"] == "request" && v["state"] == "waiting" && v["title"] == "make"
    });
    assert_eq!(make["turn"], turn + 1, "{make}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_mode_the_live_session_refuses_between_turns_leaves_the_thread_as_it_was() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "refuse_mode": "build",
            "turns": [{ "steps": [{ "kind": "text", "text": "Done." }] }]
        }),
    );
    let uid = my_uid();
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let refused = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    assert_eq!(refused["code"], "harness-error", "{refused}");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["mode"], "ask-first", "{record}");
    // The thread is free again.
    let (_, next) = send(&mut peer, Some(&thread), "Again");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_record_cannot_take_under_a_held_stop_stops_the_harness() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_ran", "title": "Ran", "prompt": "Run" },
                    { "kind": "subagent_end", "subagent": "ses_ran", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 800 },
                    { "kind": "text", "text": "Went on." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "INSERT ON turns");

    // The harness goes on, and the record takes no turn for it.
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert_eq!(record["turns"][0]["answer"], "Handed off.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_comes_in_one_read_after_the_harness_is_stopped_is_not_taken() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The session takes Full access, and refuses to go back to Ask
            // first, with a word that the subagent runs in the same read.
            "refuse_mode": "crowbar-ask-first",
            "refusal_rides_with": "ses_bg",
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_bg", "title": "Background", "prompt": "Work" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [{ "kind": "subagent_stall", "subagent": "ses_bg" }]
            }]
        }),
    );
    let extra = stub_extra(&script, &[]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    let mut working = false;
    read_until(&mut peer, |v| {
        working |= v["event"] == "subagent" && v["state"] == "working";
        v["event"] == "done" && v["turn"] == turn
    });
    assert!(working, "the subagent was never told of");
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF mode ON threads");
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    assert_eq!(set["code"], "storage-error", "{set}");
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    let record = resume(&mut peer, &thread);
    assert_ne!(record["turns"][0]["subagents"][0]["state"], "working", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_says_of_a_mode_that_settles_is_not_told_until_it_has() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_update_first": true,
            "turns": [{ "steps": [{ "kind": "text", "text": "Done." }] }]
        }),
    );
    let uid = my_uid();
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF mode ON threads");
    write_frame(
        &mut peer,
        &json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    )
    .unwrap();
    let mut told = Vec::new();
    let reply = read_until(&mut peer, |v| {
        if v["event"] == "thread-updated" && v.get("mode").is_some() {
            told.push(v.clone());
        }
        v.get("reply").is_some()
    });
    assert_eq!(reply["code"], "storage-error", "{reply}");
    // The session goes back to Ask first, and still nobody is told of Full access.
    std::thread::sleep(Duration::from_millis(400));
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    read_until(&mut peer, |v| {
        if v["event"] == "thread-updated" && v.get("mode").is_some() {
            told.push(v.clone());
        }
        v["reply"] == "status"
    });
    assert!(
        told.iter().all(|v| v["mode"] == "ask-first"),
        "a mode the thread does not have was told: {told:?}"
    );
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    assert_eq!(resume(&mut peer, &thread)["mode"], "ask-first");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_begins_under_a_held_stop_while_the_last_end_is_unsaved_stops_the_harness() {
    let dir = TempDir::new();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            // Stopped, that turn ends and its end is not saved; the harness
            // goes on while it waits to be.
            { "kind": "pause_ms", "ms": 1500 },
            { "kind": "text", "text": "Again." }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| {
        v["event"] == "message" && v["origin"] == "harness" && v["turn"] == turn + 1
    });
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF state ON turns");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn + 1);
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    std::thread::sleep(Duration::from_millis(400));
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["answer"], "Reported.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_under_a_held_stop_that_the_record_cannot_take_stops_the_harness() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                // After the Stop, the harness starts another, which goes on
                // saying nothing.
                "after": [
                    { "kind": "pause_ms", "ms": 800 },
                    { "kind": "subagent_start", "id": "ses_quiet", "title": "Quiet", "prompt": "Quietly" },
                    { "kind": "subagent_stall", "subagent": "ses_quiet" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "INSERT ON subagents");
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_begins_under_a_held_stop_when_its_thread_cannot_be_read_stops_the_harness() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_ran", "title": "Ran", "prompt": "Run" },
                    { "kind": "subagent_end", "subagent": "ses_ran", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 800 },
                    { "kind": "text", "text": "Went on." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    // The record of threads cannot be read when the harness goes on.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE threads RENAME TO threads_away").unwrap();
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("ALTER TABLE threads_away RENAME TO threads").unwrap();
    drop(db);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert_eq!(record["turns"][0]["answer"], "Handed off.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_waits_while_a_subagent_of_its_thread_asks() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_asks", "title": "Asks", "prompt": "Ask" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        { "kind": "subagent_ask", "subagent": "ses_asks", "id": "call_ask", "tool_kind": "read",
                          "title": "Read /tmp/asked", "raw_input": { "path": "/tmp/asked" }, "output": "asked" },
                        { "kind": "subagent_end", "subagent": "ses_asks", "status": "completed" }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // The message waits on the subagent's request.
    let held = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp" }),
    );
    assert_eq!(held["code"], "subagent-asks", "{held}");

    // Answered, the subagent finishes, and the message goes.
    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] == "done");
    let (_, next) = send(&mut peer, Some(&thread), "Next");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_prompt_that_waited_on_a_mode_waits_too_while_a_subagent_asks() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The mode takes long enough for the subagent to ask meanwhile.
            "set_config_delay_ms": 600,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_mid", "title": "Mid", "prompt": "Ask" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 250 },
                        { "kind": "subagent_ask", "subagent": "ses_mid", "id": "call_mid", "tool_kind": "read",
                          "title": "Read /tmp/mid", "raw_input": { "path": "/tmp/mid" }, "output": "mid" },
                        { "kind": "subagent_end", "subagent": "ses_mid", "status": "completed" }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // The mode is set, and the prompt waits while the subagent asks.
    wait_until("the mode is set", || stub_modes_set(&stub_log) == ["build"]);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(stub_prompts(&stub_log).len(), 1, "a prompt went while a subagent asked");

    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);
    assert_eq!(stub_prompts(&stub_log).len(), 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_prompt_whose_mode_comes_with_a_subagents_request_waits_on_it() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_reply_rides_with_ask": "ses_rides",
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_rides", "title": "Rides", "prompt": "Ride" },
                        { "kind": "text", "text": "Handed off." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(stub_prompts(&stub_log).len(), 1, "a prompt went while a subagent asked");

    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == sent["turn"]);
    assert_eq!(stub_prompts(&stub_log).len(), 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_held_for_a_subagent_ends_unsent_when_the_harness_goes_on_by_itself() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 600,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_asker", "title": "Asker", "prompt": "Ask" },
                        { "kind": "subagent_start", "id": "ses_teller", "title": "Teller", "prompt": "Tell" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        // One subagent asks while the next message's mode is
                        // set; the other reports, and the harness answers it.
                        { "kind": "pause_ms", "ms": 200 },
                        { "kind": "subagent_ask", "subagent": "ses_asker", "id": "call_asker", "tool_kind": "read",
                          "title": "Read /tmp/asker", "raw_input": { "path": "/tmp/asker" }, "output": "asker",
                          "wait_for_answer": false },
                        { "kind": "pause_ms", "ms": 900 },
                        { "kind": "subagent_end", "subagent": "ses_teller", "status": "completed" },
                        { "kind": "text", "text": "Background report." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let failed = read_until(&mut peer, |v| {
        v["turn"] == next && (v["event"] == "failed" || v["event"] == "done")
    });
    assert_eq!(failed["event"], "failed", "{failed}");
    assert!(
        failed["message"].as_str().unwrap_or("").contains("Send it again"),
        "{failed}"
    );
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], next + 1, "{woke}");
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == next + 1);
    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns[1]["answer"], "", "{record}");
    assert_eq!(turns[2]["answer"], "Background report.", "{record}");
    assert_eq!(stub_prompts(&stub_log).len(), 1, "the held message was sent");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_waiting_on_its_mode_ends_unsent_when_the_harness_goes_on_by_itself() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The mode is answered only after the other subagent reports.
            "set_config_delay_ms": 1500,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_asker", "title": "Asker", "prompt": "Ask" },
                        { "kind": "subagent_start", "id": "ses_teller", "title": "Teller", "prompt": "Tell" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        // One subagent asks while the next message's mode is
                        // set; the other reports, and the harness answers it.
                        { "kind": "pause_ms", "ms": 200 },
                        { "kind": "subagent_ask", "subagent": "ses_asker", "id": "call_asker", "tool_kind": "read",
                          "title": "Read /tmp/asker", "raw_input": { "path": "/tmp/asker" }, "output": "asker",
                          "wait_for_answer": false },
                        { "kind": "pause_ms", "ms": 900 },
                        { "kind": "subagent_end", "subagent": "ses_teller", "status": "completed" },
                        { "kind": "text", "text": "Background report." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let failed = read_until(&mut peer, |v| {
        v["turn"] == next && (v["event"] == "failed" || v["event"] == "done")
    });
    assert_eq!(failed["event"], "failed", "{failed}");
    assert!(
        failed["message"].as_str().unwrap_or("").contains("Send it again"),
        "{failed}"
    );
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], next + 1, "{woke}");
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == next + 1);
    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns[1]["answer"], "", "{record}");
    assert_eq!(turns[2]["answer"], "Background report.", "{record}");
    // The mode is answered after, and the message still does not go.
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(stub_prompts(&stub_log).len(), 1, "the unsent message was sent");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_that_takes_over_with_a_mode_keeps_what_the_harness_says_as_its_mode_is_set() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The answer to the mode comes with more of what the harness says.
            "mode_reply_rides_with_text": " Still reporting.",
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_take_mode", "title": "Dig", "prompt": "Dig in" },
                        { "kind": "subagent_end", "subagent": "ses_take_mode", "status": "completed" },
                        { "kind": "text", "text": "Started." }
                    ],
                    "after": [{ "kind": "text", "text": "Report in." }]
                },
                { "match": "next", "steps": [{ "kind": "text", "text": " Next answer." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    read_until(&mut peer, |v| v["event"] == "text" && v["text"] == "Report in.");

    // The person's message, with a mode, takes over the turn the harness began.
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let ended = read_until(&mut peer, |v| {
        v["turn"] == next && ["done", "failed", "stopped", "interrupted"].contains(&v["event"].as_str().unwrap_or(""))
    });
    assert_eq!(ended["event"], "done", "{ended}");
    let record = resume(&mut peer, &thread);
    let answer = record["turns"][2]["answer"].as_str().unwrap_or("");
    assert!(answer.contains("Still reporting."), "{record}");
    assert!(answer.contains("Next answer."), "{record}");
    assert_eq!(stub_prompts(&stub_log).len(), 2, "the message was not sent");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_report_the_persons_message_took_over_stays_whole_when_a_subagent_asks_after() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 700,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_reporter", "title": "Reporter", "prompt": "Report" },
                        { "kind": "subagent_start", "id": "ses_later", "title": "Later", "prompt": "Ask later" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        // While the next message's mode is set, one subagent
                        // reports and the harness begins to answer it; the
                        // other asks before the answer is done.
                        { "kind": "pause_ms", "ms": 150 },
                        { "kind": "subagent_end", "subagent": "ses_reporter", "status": "completed" },
                        { "kind": "text", "text": "Report part one." },
                        { "kind": "pause_ms", "ms": 150 },
                        { "kind": "subagent_ask", "subagent": "ses_later", "id": "call_later", "tool_kind": "read",
                          "title": "Read /tmp/later", "raw_input": { "path": "/tmp/later" }, "output": "later",
                          "wait_for_answer": false },
                        { "kind": "pause_ms", "ms": 100 },
                        { "kind": "text", "text": " Part two." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": " Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "failed" && v["turn"] == next),
            "the message ended unsent after it took over the report: {v}"
        );
        v["event"] == "text" && v["text"] == " Part two."
    });
    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);
    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{record}");
    assert_eq!(turns[1]["answer"], "Report part one. Part two. Next.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}
