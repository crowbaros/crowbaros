//! Models, free hosted Models, modes, permission requests, efforts, commands, catalog refresh, and transport retry.

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
fn known_acp_updates_like_thoughts_and_available_commands_do_not_become_notes() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "emit_available_commands": true,
            "turns": [
                {
                    "steps": [
                        { "kind": "thought", "text": "thinking " },
                        { "kind": "thought", "text": "hard" },
                        { "kind": "available_commands" },
                        { "kind": "unknown", "update": { "sessionUpdate": "vendor_real_note", "summary": "Keep me" } },
                        { "kind": "text", "text": "Clean answer." }
                    ]
                }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (tid, turn) = send(&mut peer, None, "go");
    let evs = turn_events(&mut peer, &tid, turn);
    assert_eq!(text_of(&evs), "Clean answer.");

    let notes: Vec<&str> = evs
        .iter()
        .filter(|e| e["event"] == "note")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert_eq!(notes, vec!["Keep me"]);

    let rec = resume(&mut peer, &tid);
    let saved_notes: Vec<&str> = rec["turns"][0]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["text"].as_str())
        .collect();
    assert_eq!(saved_notes, vec!["Keep me"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn surface_socket_can_toggle_free_models_and_model_switch_preserves_default() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "models": [
                { "id": "stub/stub-1", "name": "Stub One" },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "turns": [
                { "steps": [{ "kind": "text", "text": "on {model}" }] },
                { "steps": [{ "kind": "text", "text": "after {model}" }] }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    // Surface socket can change free hosted Models and broadcasts to watchers.
    let mut surface = handshake(&lever.surface);
    write_frame(&mut surface, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut surface).unwrap()["reply"], "activity");
    write_frame(&mut surface, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "reply": "free-models", "enabled": true, "models_chosen": true })
    );
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "event": "free-models", "enabled": true, "models_chosen": true })
    );
    write_frame(&mut surface, &json!({ "op": "set-free-models", "enabled": false })).unwrap();
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "reply": "free-models", "enabled": false, "models_chosen": false })
    );
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "event": "free-models", "enabled": false, "models_chosen": false })
    );

    // Person socket can read models, switch one thread's Model, and `models` still reports stub/stub-1 as default.
    let mut person = handshake(&lever.person);
    write_frame(&mut person, &json!({ "op": "models" })).unwrap();
    let m1 = read_frame(&mut person).unwrap();
    assert_eq!(m1["default"], "stub/stub-1");

    let (tid, t1) = send(&mut person, None, "hello");
    let ev1 = turn_events(&mut person, &tid, t1);
    assert_eq!(text_of(&ev1), "on stub/stub-1");

    write_frame(
        &mut person,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/stub-2" }),
    )
    .unwrap();
    let set_ok = read_frame(&mut person).unwrap();
    assert_eq!(set_ok["reply"], "model-set");
    // Who watches the thread is told its Model.
    assert_eq!(
        read_frame(&mut person).unwrap(),
        json!({ "event": "thread-updated", "thread": tid, "model": "stub/stub-2" })
    );

    write_frame(&mut person, &json!({ "op": "models" })).unwrap();
    let m2 = read_frame(&mut person).unwrap();
    assert_eq!(m2["default"], "stub/stub-1");

    // A rejected `--model` on a new thread is not persisted in SQLite.
    write_frame(
        &mut person,
        &json!({ "op": "send", "thread": null, "text": "bad model", "model": "stub/nonexistent" }),
    )
    .unwrap();
    let sent = read_frame(&mut person).unwrap();
    let bad_tid = sent["thread"].as_str().unwrap().to_string();
    let bad_evs = turn_events(&mut person, &bad_tid, 1);
    assert_eq!(bad_evs.last().unwrap()["event"], "failed");
    let rec = resume(&mut person, &bad_tid);
    assert!(rec.get("model").is_none(), "rejected model must not persist: {rec}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn free_models_off_while_working_refuses_new_sends_until_harness_restarts() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "no_models_when_free_disabled": true,
            "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default_model": "stub/stub-1",
            "turns": [
                { "stall": true, "steps": [{ "kind": "text", "text": "working..." }] }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut p1 = handshake(&lever.person);
    write_frame(&mut p1, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    assert_eq!(read_frame(&mut p1).unwrap()["reply"], "free-models");

    // Start a stalled turn on p1.
    let (tid, t1) = send(&mut p1, None, "hold");
    let ev = read_frame(&mut p1).unwrap();
    assert_eq!(ev["event"], "message");
    read_until(&mut p1, |ev| ev["event"] == "text");

    // Turn free hosted Models off from a second client while turn 1 is still working.
    let mut p2 = handshake(&lever.person);
    write_frame(&mut p2, &json!({ "op": "set-free-models", "enabled": false })).unwrap();
    assert_eq!(
        read_frame(&mut p2).unwrap(),
        json!({ "reply": "free-models", "enabled": false, "models_chosen": false })
    );

    // A new send and models query must be refused while the stale harness is still winding down.
    write_frame(
        &mut p2,
        &json!({ "op": "send", "thread": null, "text": "sneak through" }),
    )
    .unwrap();
    let refused = read_frame(&mut p2).unwrap();
    assert_eq!(refused["reply"], "error");
    assert_eq!(refused["code"], "working");

    write_frame(&mut p2, &json!({ "op": "models" })).unwrap();
    let refused_models = read_frame(&mut p2).unwrap();
    assert_eq!(refused_models["reply"], "error");
    assert_eq!(refused_models["code"], "working");

    // Stop turn 1; Lever stops the stale harness so the next check sees free hosted Models off.
    write_frame(&mut p1, &json!({ "op": "stop", "thread": tid, "turn": t1 })).unwrap();
    let _ = turn_events(&mut p1, &tid, t1);

    write_frame(&mut p2, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut p2).unwrap();
    assert_eq!(models["free_hosted_models"], false);
    assert_eq!(models["models"], json!([]));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn disconnecting_while_waiting_for_models_reply_does_not_stall_lever() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "new_session_delay_ms": 2000,
            "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default_model": "stub/stub-1",
            "turns": []
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    // Send `models` and close the socket well before the 2s probe finishes.
    {
        let mut departing = handshake(&lever.person);
        write_frame(&mut departing, &json!({ "op": "models" })).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }

    // Lever must notice the hang-up and log it BEFORE the 2s probe finishes
    // (without the POLLHUP fix, Lever busy-spins and never logs disconnect until the probe completes).
    let log_path = lever.state.join("lever.log");
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        if std::fs::read_to_string(&log_path)
            .unwrap_or_default()
            .contains("the person left")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let log_before_probe = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log_before_probe.contains("the person left"),
        "expected disconnect to be logged before probe completed: {log_before_probe}"
    );

    // Another peer stays responsive.
    let mut p2 = handshake(&lever.person);
    write_frame(&mut p2, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut p2).unwrap();
    assert_eq!(status["reply"], "status");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stopping_turn_during_model_switch_locks_thread_and_preserves_saved_model() {
    let dir = TempDir::new();
    let uid = my_uid();
    let rpc_log = dir.0.join("rpc.log");
    let script = write_script(
        &dir.0,
        &json!({
            "set_config_delay_ms": 400,
            "models": [
                { "id": "stub/stub-1", "name": "Stub One" },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "turns": [
                { "steps": [{ "kind": "text", "text": "reply on {model}" }] }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    // Start a new thread requesting stub/stub-2 (which triggers a delayed set_config_option from default stub/stub-1).
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "switch and stop", "model": "stub/stub-2" }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    assert_eq!(sent["reply"], "sent");
    let tid = sent["thread"].as_str().unwrap().to_string();
    let ev_msg = read_frame(&mut peer).unwrap();
    assert_eq!(ev_msg["event"], "message");

    // Wait until the stub actually receives session/set_config_option, then stop turn 1.
    wait_until("the harness to receive session/set_config_option", || {
        std::fs::read_to_string(&rpc_log)
            .unwrap_or_default()
            .contains("session/set_config_option")
    });
    write_frame(&mut peer, &json!({ "op": "stop", "thread": tid, "turn": 1 })).unwrap();
    let stop_reply = read_frame(&mut peer).unwrap();
    assert_eq!(stop_reply["reply"], "ok");
    let ev_stopped = read_frame(&mut peer).unwrap();
    assert_eq!(ev_stopped["event"], "stopped");

    // While the stopped turn's set_config_option is still in flight, a new send on the same thread is refused with `working`.
    write_frame(&mut peer, &json!({ "op": "send", "thread": tid, "text": "too soon" })).unwrap();
    let busy = read_frame(&mut peer).unwrap();
    assert_eq!(busy["reply"], "error");
    assert_eq!(busy["code"], "working");

    // Once the delayed set_config_option settles, the stopped turn's Model was NOT saved to SQLite,
    // and the next turn without `--model` runs on the default Model (stub/stub-1), not stub/stub-2.
    std::thread::sleep(Duration::from_millis(550));
    let rec = resume(&mut peer, &tid);
    assert!(rec.get("model").is_none(), "stopped switch must not persist: {rec}");

    let (_, turn2) = send(&mut peer, Some(&tid), "after settle");
    let evs = turn_events(&mut peer, &tid, turn2);
    assert_eq!(text_of(&evs), "reply on stub/stub-1");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn surface_send_attaches_fulcrum_desktop_context_and_falls_back_when_fulcrum_is_away() {
    use std::os::unix::net::UnixListener;

    let dir = TempDir::new();
    let uid = my_uid();
    let rpc_log = dir.0.join("rpc.log");
    let fulcrum_sock = dir.0.join("fulcrum.sock");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "", "steps": [{ "kind": "text", "text": "seen" }] }
            ]
        }),
    );

    // Fake Fulcrum agent socket: accepts one connection, answers handshake + snapshot.
    let listener = UnixListener::bind(&fulcrum_sock).unwrap();
    let fulcrum_thread = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let v = read_frame(&mut peer).unwrap();
        assert_eq!(v, json!({ "version": 1 }));
        write_frame(&mut peer, &json!({ "version": 1 })).unwrap();
        let req = read_frame(&mut peer).unwrap();
        assert_eq!(req, json!({ "op": "snapshot" }));
        write_frame(
            &mut peer,
            &json!({
                "reply": "snapshot",
                "workspace": 2,
                "active": "0x2",
                "locked": false,
                "windows": [
                    { "id": "0x1", "app_id": "com.crowbaros.Files", "title": "Home", "workspace": 1 },
                    { "id": "0x2", "app_id": "foot", "title": "terminal", "workspace": 2 }
                ],
                "displays": [
                    { "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }
                ]
            }),
        )
        .unwrap();
    });

    let extra: Vec<String> = vec![
        "--fulcrum-socket".into(),
        fulcrum_sock.to_str().unwrap().into(),
        "--harness".into(),
        STUB_BIN.into(),
        "--harness-name".into(),
        "Stub".into(),
        "--harness-arg".into(),
        "--script".into(),
        "--harness-arg".into(),
        script.to_str().unwrap().into(),
        "--harness-arg".into(),
        "--log".into(),
        "--harness-arg".into(),
        rpc_log.to_str().unwrap().into(),
    ];
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);

    // 1. Surface send with Fulcrum running: carries desktop context in message event, resume, and ACP prompt.
    let mut surface = handshake(&lever.surface);
    write_frame(
        &mut surface,
        &json!({ "op": "send", "thread": null, "text": "what is open?" }),
    )
    .unwrap();
    let sent = read_frame(&mut surface).unwrap();
    assert_eq!(sent["reply"], "sent");
    let tid = sent["thread"].as_str().unwrap().to_string();
    let msg_ev = read_frame(&mut surface).unwrap();
    assert_eq!(msg_ev["event"], "message");
    assert_eq!(msg_ev["text"], "what is open?");
    assert_eq!(msg_ev["context"]["workspace"], 2);
    assert_eq!(msg_ev["context"]["active"]["app_id"], "foot");
    assert_eq!(msg_ev["context"]["active"]["title"], "terminal");
    assert_eq!(msg_ev["context"]["displays"][0]["name"], "Virtual-1");
    let evs = turn_events(&mut surface, &tid, 1);
    assert_eq!(text_of(&evs), "seen");
    fulcrum_thread.join().unwrap();

    // Persisted in SQLite and returned by resume.
    let rec = resume(&mut surface, &tid);
    assert_eq!(rec["turns"][0]["context"]["active"]["app_id"], "foot");
    assert_eq!(rec["turns"][0]["message"], "what is open?");

    // Harness received fenced desktop context as an assistant-audience block
    // separate from the person's text, so leading slash commands remain visible.
    let log_after_1 = std::fs::read_to_string(&rpc_log).unwrap();
    assert!(log_after_1.contains("<desktop-context>"), "{log_after_1}");
    assert!(log_after_1.contains("[Desktop context"), "{log_after_1}");
    assert!(log_after_1.contains("terminal (app_id: foot)"), "{log_after_1}");
    assert!(log_after_1.contains("Virtual-1: 1280x960"), "{log_after_1}");
    let prompt_req = log_after_1
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v["method"] == "session/prompt")
        .expect("session/prompt logged");
    let blocks = prompt_req["params"]["prompt"].as_array().expect("prompt blocks");
    assert_eq!(blocks.len(), 2, "expected context + user text blocks: {prompt_req}");
    assert_eq!(blocks[0]["annotations"]["audience"], json!(["assistant"]), "{blocks:?}");
    assert!(
        blocks[0]["text"].as_str().unwrap_or("").contains("[Desktop context"),
        "{blocks:?}"
    );
    assert!(
        blocks[1].get("annotations").is_none(),
        "user prompt block must not carry assistant audience: {blocks:?}"
    );
    assert_eq!(blocks[1]["text"], "what is open?");

    // 2. Remove Fulcrum socket: next surface send succeeds with NO desktop context.
    let _ = std::fs::remove_file(&fulcrum_sock);
    let log_offset = log_after_1.len();
    write_frame(
        &mut surface,
        &json!({ "op": "send", "thread": tid, "text": "without fulcrum" }),
    )
    .unwrap();
    let sent2 = read_frame(&mut surface).unwrap();
    assert_eq!(sent2["reply"], "sent");
    let msg_ev2 = read_frame(&mut surface).unwrap();
    assert_eq!(msg_ev2["event"], "message");
    assert!(
        msg_ev2.get("context").is_none(),
        "expected no context when Fulcrum is away: {msg_ev2}"
    );
    let evs2 = turn_events(&mut surface, &tid, 2);
    assert_eq!(text_of(&evs2), "seen");

    let log_after_2 = &std::fs::read_to_string(&rpc_log).unwrap()[log_offset..];
    assert!(log_after_2.contains("without fulcrum"), "{log_after_2}");
    assert!(!log_after_2.contains("<desktop-context>"), "{log_after_2}");

    // 3. Person-socket send carries no desktop context even if Fulcrum were listening.
    let listener2 = UnixListener::bind(&fulcrum_sock).unwrap();
    listener2.set_nonblocking(true).unwrap();
    let mut person = handshake(&lever.person);
    let (ptid, pturn) = send(&mut person, None, "from terminal");
    let pevs = turn_events(&mut person, &ptid, pturn);
    assert_eq!(text_of(&pevs), "seen");
    assert!(listener2.accept().is_err(), "person send should not query Fulcrum");
    let prec = resume(&mut person, &ptid);
    assert!(prec["turns"][0].get("context").is_none());

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_waits_for_the_person_with_no_time_limit_and_is_answered_once_always_or_no() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--idle-timeout-ms", "200"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let mut shell = handshake(&lever.surface);

    // The shell watches what works and what waits.
    assert_eq!(
        ask_lever(&mut shell, json!({ "op": "watch" })),
        json!({ "reply": "activity", "working": [], "waiting": [] })
    );

    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(request["state"], "waiting");
    assert_eq!(request["kind"], "command");
    assert_eq!(request["detail"], "echo hi && id -un");
    assert_eq!(
        request["answers"],
        json!([
            { "answer": "once", "name": "Allow once" },
            { "answer": "always", "name": "Always allow" },
            { "answer": "no", "name": "Reject" }
        ])
    );
    assert!(request["act"].as_str().unwrap().starts_with("act-"), "{request}");
    let request_id = request["request"].as_str().unwrap().to_string();
    assert!(request_id.starts_with("r-"), "{request}");
    let activity = read_until(&mut shell, |f| f["event"] == "activity" && f["waiting"] != json!([]));
    assert_eq!(activity["waiting"], json!([thread.clone()]));
    assert_eq!(activity["working"], json!([thread.clone()]));

    // It waits with no time limit: past the idle timeout, the harness still runs.
    std::thread::sleep(Duration::from_millis(700));
    let status = ask_lever(&mut peer, json!({ "op": "status" }));
    assert_eq!(status["harness_started"], true);
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], true, "{list}");
    assert_eq!(list["threads"][0]["mode"], "ask-first", "{list}");

    // Only an answer it offers, for its own thread, is taken.
    let wrong = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request_id, "answer": "maybe" }),
    );
    assert_eq!(wrong["code"], "invalid-answer");
    let missing = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": "r-000000000000", "answer": "once" }),
    );
    assert_eq!(missing["code"], "no-request");

    // Once: it runs, and the next time it asks again.
    let ok = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request_id, "answer": "once" }),
    );
    assert_eq!(ok, json!({ "reply": "ok" }));
    let answered = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (answered["state"].as_str(), answered["answer"].as_str()),
        (Some("answered"), Some("once"))
    );
    let act = read_until(&mut peer, |f| f["event"] == "act" && f["state"] == "done");
    assert_eq!(act["detail"], "hi");
    read_until(&mut peer, |f| f["event"] == "done");
    let again = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request_id, "answer": "once" }),
    );
    assert_eq!(again["code"], "no-request", "an answered request is answered once");

    send(&mut peer, Some(&thread), "please run it again");
    let second = read_until(&mut peer, |f| f["event"] == "request");
    // Always: it runs, and is not asked again.
    ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": second["request"], "answer": "always" }),
    );
    read_until(&mut peer, |f| f["event"] == "done");
    send(&mut peer, Some(&thread), "please run it a third time");
    let third = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(third["event"], "done", "always is the harness's to keep: {third}");

    // No: the harness is told, and ends the turn as it has it.
    send(&mut peer, Some(&thread), "please edit it");
    let edit = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(edit["kind"], "edit");
    assert_eq!(edit["path"], "/w/seed.txt");
    assert_eq!(edit["detail"], "-alpha\n+beta");
    ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": edit["request"], "answer": "no" }),
    );
    let failed = read_until(&mut peer, |f| f["event"] == "act" && f["state"] == "failed");
    assert_eq!(failed["detail"], "The user declined this tool call");
    read_until(&mut peer, |f| f["event"] == "stopped" && f["turn"] == 4);
    assert_eq!(stub_heard(&stub_log), ["once", "always", "reject"]);

    // The record has each request, and how it was answered.
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    let answers: Vec<_> = resumed["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            t["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["answer"].clone())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        answers,
        vec![vec![json!("once")], vec![json!("always")], vec![], vec![json!("no")]]
    );
    assert_eq!(resumed["turns"][0]["mode"], "ask-first");
    read_until(&mut shell, |f| {
        f["event"] == "activity" && f["waiting"] == json!([]) && f["working"] == json!([])
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stop_and_the_ends_of_lever_and_its_harness_withdraw_a_waiting_request() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    // Stop withdraws it, and the harness is told so.
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread })),
        json!({ "reply": "ok" })
    );
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (withdrawn["request"].clone(), withdrawn["state"].as_str()),
        (request["request"].clone(), Some("withdrawn"))
    );
    read_until(&mut peer, |f| f["event"] == "stopped");
    assert_eq!(stub_heard(&stub_log), ["cancelled"]);
    let gone = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" }),
    );
    assert_eq!(gone["code"], "no-request");

    // Lever killed while a request waits: after it starts again, the turn
    // was interrupted and the request withdrawn.
    send(&mut peer, Some(&thread), "please run it again");
    read_until(&mut peer, |f| f["event"] == "request");
    drop(peer);
    lever.kill_and_wait();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][1]["state"], "interrupted");
    assert_eq!(resumed["turns"][1]["requests"][0]["state"], "withdrawn");
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], false);

    // The harness ends while a request waits: withdrawn, and interrupted.
    send(&mut peer, Some(&thread), "please run it once more");
    read_until(&mut peer, |f| f["event"] == "request");
    let stub_pid: Vec<u32> = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| c.starts_with(&format!("{STUB_BIN}\0")) && c.contains(script.to_str().unwrap()))
        })
        .collect();
    assert_eq!(stub_pid.len(), 1, "one stub harness runs");
    unsafe { libc::kill(stub_pid[0] as libc::pid_t, libc::SIGKILL) };
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(withdrawn["state"], "withdrawn");
    read_until(&mut peer, |f| f["event"] == "interrupted");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_keeps_its_mode_and_new_threads_start_in_the_mode_the_person_chose() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let mut shell = handshake(&lever.surface);

    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["default_mode"],
        "ask-first"
    );
    let models = ask_lever(&mut peer, json!({ "op": "models" }));
    assert_eq!(
        models["modes"],
        json!([
            { "id": "ask-first", "name": "Ask first" },
            { "id": "full-access", "name": "Full access" },
            { "id": "plan", "name": "Plan" }
        ])
    );

    // A new thread asks first; the session starts there, and Lever sets nothing.
    let (first, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": first, "request": request["request"], "answer": "once" }),
    );
    read_until(&mut peer, |f| f["event"] == "done");
    assert!(stub_modes_set(&stub_log).is_empty());

    // Full access from the drawer: the next turn runs in OpenCode's build, and asks nothing.
    let set = ask_lever(
        &mut shell,
        json!({ "op": "set-mode", "thread": first, "mode": "full-access" }),
    );
    assert_eq!(
        set,
        json!({ "reply": "mode-set", "thread": first, "mode": "full-access" })
    );
    send(&mut peer, Some(&first), "please run it again");
    let next = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(next["event"], "done", "{next}");
    assert_eq!(stub_modes_set(&stub_log), ["build"]);

    // Only Lever's modes, and only starting modes for new threads.
    let bad = ask_lever(&mut peer, json!({ "op": "set-mode", "thread": first, "mode": "yolo" }));
    assert_eq!(bad["code"], "invalid-mode");
    let bad = ask_lever(&mut peer, json!({ "op": "set-default-mode", "mode": "plan" }));
    assert_eq!(bad["code"], "invalid-mode");
    let bad = ask_lever(&mut peer, json!({ "op": "send", "text": "x", "mode": "yolo" }));
    assert_eq!(bad["code"], "invalid-mode");

    // Full access as the starting mode, from the drawer.
    let default = ask_lever(&mut shell, json!({ "op": "set-default-mode", "mode": "full-access" }));
    assert_eq!(default, json!({ "reply": "default-mode", "mode": "full-access" }));
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["default_mode"],
        "full-access"
    );
    let (second, _) = send(&mut peer, None, "please run it");
    let next = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(next["event"], "done", "{next}");
    // A thread can still start in another mode.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "please run it", "cwd": "/tmp", "mode": "ask-first" }),
    )
    .unwrap();
    let third = read_until(&mut peer, |f| f["reply"] == "sent")["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(request["thread"], third.as_str());

    // A thread working cannot change its mode.
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": third, "mode": "full-access" }),
    );
    assert_eq!(busy["code"], "working");
    ask_lever(&mut peer, json!({ "op": "stop", "thread": third }));
    read_until(&mut peer, |f| f["event"] == "stopped" && f["thread"] == third.as_str());

    // Plan: an edit is refused by the harness, and nothing is asked.
    ask_lever(&mut peer, json!({ "op": "set-mode", "thread": first, "mode": "plan" }));
    send(&mut peer, Some(&first), "please edit it");
    let refused = read_until(&mut peer, |f| f["event"] == "act" && f["state"] == "failed");
    assert_eq!(refused["detail"], "Not allowed in this mode");
    read_until(&mut peer, |f| f["event"] == "done");

    // The harness changes the mode itself: the thread takes it.
    ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": second, "mode": "ask-first" }),
    );
    send(&mut peer, Some(&second), "please switch");
    let updated = read_until(&mut peer, |f| f["event"] == "thread-updated");
    assert_eq!(
        updated,
        json!({ "event": "thread-updated", "thread": second, "mode": "full-access" })
    );
    read_until(&mut peer, |f| f["event"] == "done");

    // Each thread keeps its mode across a restart of Lever, and its next turn
    // sets it on the harness started again.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    let modes: std::collections::BTreeMap<String, String> = list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["thread"].as_str().unwrap().to_string(),
                t["mode"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(modes[&first], "plan");
    assert_eq!(modes[&second], "full-access");
    assert_eq!(modes[&third], "ask-first");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "resume", "thread": first }))["mode"],
        "plan"
    );
    let before = stub_modes_set(&stub_log).len();
    send(&mut peer, Some(&first), "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    assert_eq!(stub_modes_set(&stub_log)[before..], ["plan"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn lever_adds_ask_first_for_its_own_harness_and_leaves_the_persons_opencode_files_alone() {
    let dir = TempDir::new();
    let uid = my_uid();
    let home = dir.0.join("home");
    let config = home.join(".config/opencode");
    std::fs::create_dir_all(&config).unwrap();
    let mine = b"{\n  // the person's own\n  \"permission\": {\"bash\": {\"*\": \"allow\", \"rm *\": \"deny\"}},\n  \"agent\": {\"mine\": {\"mode\": \"primary\"}},\n}\n";
    std::fs::write(config.join("opencode.jsonc"), mine).unwrap();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let env_out = dir.0.join("env.json");
    let extra = stub_args(
        &script,
        &stub_log,
        &["--harness-arg", "--env-out", "--harness-arg", env_out.to_str().unwrap()],
    );
    let xdg = home.join(".config");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        (
            "OPENCODE_CONFIG_CONTENT",
            r#"{"agent":{"from-env":{"mode":"primary"}}}"#,
        ),
    ];
    let mut lever = RunningLever::start_with_env(&dir.0, uid, uid, &extra, &env);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");

    let seen: Value = serde_json::from_str(&std::fs::read_to_string(&env_out).unwrap()).unwrap();
    let content: Value = serde_json::from_str(seen["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert_eq!(content["default_agent"], "crowbar-ask-first");
    // Ask first asks before every command and edit, except where the
    // person's own rules say otherwise: what they refuse stays refused.
    let permission = &content["agent"]["crowbar-ask-first"]["permission"];
    assert_eq!(
        permission,
        &json!({
            "bash": { "*": "ask", "rm *": "deny" },
            "edit": { "*": "ask" },
            "crowbar_*": { "*": "ask" },
            "crowbar_make_project": { "*": "ask" }
        })
    );
    let order: Vec<&String> = permission["bash"].as_object().unwrap().keys().collect();
    assert_eq!(order, ["*", "rm *"], "the person's rule comes after Ask first's");
    assert_eq!(content["agent"]["from-env"], json!({ "mode": "primary" }), "{content}");
    assert_eq!(content["update"], "disable");

    // The person's own file is as they made it, and Lever made no other.
    assert_eq!(std::fs::read(config.join("opencode.jsonc")).unwrap(), mine);
    let files: Vec<_> = std::fs::read_dir(&config)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(files, vec![std::ffi::OsString::from("opencode.jsonc")]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_the_harness_ignores_ends_the_turn_and_every_command_it_started() {
    let dir = TempDir::new();
    let uid = my_uid();
    let pid_file = dir.0.join("sleep.pid");
    let detached_file = dir.0.join("detached.pid");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "ignore",
                    "steps": [
                        { "kind": "spawn", "pid_file": pid_file.to_str().unwrap() },
                        { "kind": "spawn", "pid_file": detached_file.to_str().unwrap(), "detach": true },
                        { "kind": "text", "text": "Working" }
                    ],
                    "stall": true,
                    "ignore_cancel": true
                },
                { "match": "other", "steps": [{ "kind": "text", "text": "Other" }], "stall": true },
                { "match": "hello", "steps": [{ "kind": "text", "text": "Hello again" }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "500"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    let (other, _) = send(&mut peer, None, "other");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == other.as_str());
    let (stuck, _) = send(&mut peer, None, "ignore");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == stuck.as_str());
    let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    assert!(alive(pid), "the turn's command runs");
    // A command run in the background, whose shell has ended.
    let detached: u32 = std::fs::read_to_string(&detached_file).unwrap().trim().parse().unwrap();
    assert!(alive(detached), "the command it left behind runs");

    let asked = Instant::now();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": stuck })),
        json!({ "reply": "ok" })
    );
    let mut ended = std::collections::BTreeMap::new();
    while ended.len() < 2 {
        let frame = read_until(&mut peer, |f| {
            ["stopped", "interrupted"].contains(&f["event"].as_str().unwrap_or(""))
        });
        ended.insert(
            frame["thread"].as_str().unwrap().to_string(),
            frame["event"].as_str().unwrap().to_string(),
        );
    }
    let waited = asked.elapsed();
    assert!(
        waited >= Duration::from_millis(450),
        "Lever waited for the harness first: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(2),
        "Lever did not wait for ever: {waited:?}"
    );
    assert_eq!(ended[&stuck], "stopped");
    assert_eq!(ended[&other], "interrupted", "a turn of the same harness cannot go on");
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive(pid) || alive(detached) {
        assert!(Instant::now() < deadline, "the turn's commands still run");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"],
        false
    );
    assert!(
        lever.log_text().contains("did not stop a turn when asked"),
        "{}",
        lever.log_text()
    );

    // The record says so, and the next message starts the harness again.
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": stuck }));
    assert_eq!(resumed["turns"][0]["state"], "stopped");
    send(&mut peer, Some(&other), "hello");
    let text = read_until(&mut peer, |f| f["event"] == "text");
    assert_eq!(text["text"], "Hello again");
    read_until(&mut peer, |f| f["event"] == "done");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_the_harness_carries_out_leaves_the_other_threads_working() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "one", "steps": [{ "kind": "text", "text": "One" }], "stall": true },
                { "match": "two", "steps": [{ "kind": "text", "text": "Two" }], "stall": true }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (one, _) = send(&mut peer, None, "one");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == one.as_str());
    let (two, _) = send(&mut peer, None, "two");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == two.as_str());

    ask_lever(&mut peer, json!({ "op": "stop", "thread": two }));
    read_until(&mut peer, |f| f["event"] == "stopped" && f["thread"] == two.as_str());
    std::thread::sleep(Duration::from_millis(600));
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    let one_now = list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["thread"] == one.as_str())
        .unwrap();
    assert_eq!(one_now["state"], "working", "{list}");
    assert!(!lever.log_text().contains("did not stop a turn when asked"));

    ask_lever(&mut peer, json!({ "op": "stop", "thread": one }));
    read_until(&mut peer, |f| f["event"] == "stopped" && f["thread"] == one.as_str());
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_after_its_turn_keeps_the_harness_until_stop_withdraws_it() {
    let dir = TempDir::new();
    let uid = my_uid();
    // The turn ends, and a request comes after it, as a subagent's would.
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "text", "text": "Done here." }],
                "after": [{ "kind": "ask", "id": "c7", "title": "make", "raw_input": { "command": "make" } }]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--idle-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut watcher = handshake(&lever.surface);
    assert_eq!(
        ask_lever(&mut watcher, json!({"op":"list","watch":true}))["reply"],
        "list"
    );
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (request["turn"].as_u64(), request["detail"].as_str()),
        (Some(1), Some("make"))
    );

    // Past the idle timeout, the harness still runs: a request waits on it.
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"], true);
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(
        (
            list["threads"][0]["working"].clone(),
            list["threads"][0]["waiting"].clone()
        ),
        (json!(false), json!(true))
    );

    // A post-turn request still keeps deletion unsafe and the list current.
    let snapshot = read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"][0]["waiting"] == true
    });
    assert_eq!(snapshot["threads"][0]["working"], false);
    assert_eq!(snapshot["threads"][0]["archived"], false);
    assert_eq!(snapshot["threads"][0]["mode"], "ask-first");
    assert_eq!(
        ask_lever(&mut peer, json!({"op":"delete","thread":thread}))["code"],
        "working"
    );
    assert_eq!(
        ask_lever(&mut peer, json!({"op":"resume","thread":thread}))["reply"],
        "resumed"
    );

    // Stop withdraws it though no turn works, and the harness is then idle.
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread })),
        json!({ "reply": "ok" })
    );
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(withdrawn["state"], "withdrawn");
    let deadline = Instant::now() + Duration::from_secs(3);
    while ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == true {
        assert!(Instant::now() < deadline, "an idle harness is stopped");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(stub_heard(&stub_log), ["cancelled"]);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_for_a_turn_that_ended_withdraws_nothing_of_the_turn_after_it() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    send(&mut peer, Some(&thread), "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    // A Stop that comes late, for the turn before, leaves this one's request.
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": first })),
        json!({ "reply": "ok" })
    );
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], true, "{list}");
    assert_eq!(
        ask_lever(
            &mut peer,
            json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" })
        ),
        json!({ "reply": "ok" })
    );
    read_until(&mut peer, |f| f["event"] == "done");
    assert_eq!(stub_heard(&stub_log), ["once"]);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_person_asked_to_stop_asks_them_nothing_more() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "text", "text": "Working" },
                    { "kind": "pause_ms", "ms": 400 },
                    { "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" } }
                ],
                "stall": true,
                "ignore_cancel": true
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "5000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "text");
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));

    // What the harness asks after the Stop is answered for the person: no.
    let end = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "stopped");
    assert_eq!(end["event"], "stopped", "{end}");
    assert_eq!(stub_heard(&stub_log), ["cancelled"]);
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][0]["requests"], json!([]));
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_stopped_while_its_mode_is_set_holds_the_thread_until_the_harness_answers() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 600,
            "turns": [
                {
                    "match": "please run",
                    "steps": [{ "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" } }]
                },
                { "steps": [{ "kind": "text", "text": "Hello." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");

    // The message asks for Full access. The harness takes it at once, and
    // answers late; the turn is stopped before it answers.
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "hello again", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    wait_until("Lever asks for Full access", || stub_modes_set(&stub_log) == ["build"]);
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");

    // Until it answers, the thread takes no other mode and no message.
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "ask-first" }),
    );
    assert_eq!(busy["code"], "working", "{busy}");
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "please run it" }),
    );
    assert_eq!(busy["code"], "working", "{busy}");

    // Then Ask first is set again, and the next turn asks.
    wait_until("the thread is free", || {
        ask_lever(
            &mut peer,
            json!({ "op": "set-mode", "thread": thread, "mode": "ask-first" }),
        )["reply"]
            == "mode-set"
    });
    send(&mut peer, Some(&thread), "please run it");
    let asked = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(asked["event"], "request", "{asked}");
    assert_eq!(stub_modes_set(&stub_log), ["build", "crowbar-ask-first"]);
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_says_only_what_changed_still_offers_its_modes() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "partial_config_replies": true,
            "models": [{ "id": "m1", "name": "One" }, { "id": "m2", "name": "Two" }],
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "set-model", "thread": thread, "model": "m2" }))["reply"],
        "model-set"
    );
    let models = ask_lever(&mut peer, json!({ "op": "models" }));
    assert_eq!(models["modes"].as_array().map(Vec::len), Some(3), "{models}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_whose_modes_are_not_levers_runs_turns_and_new_threads_start_only_in_one_it_offers() {
    let dir = TempDir::new();
    let uid = my_uid();
    // Modes of its own: Lever sets none, and the turn runs.
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_values": ["default", "yolo"],
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "hello");
    let end = read_until(&mut peer, |f| {
        ["done", "failed"].contains(&f["event"].as_str().unwrap_or(""))
    });
    assert_eq!(end["event"], "done", "{end}");
    assert!(stub_modes_set(&stub_log).is_empty());
    assert_eq!(ask_lever(&mut peer, json!({ "op": "models" }))["modes"], json!([]));
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // No Full access: new threads cannot start in it.
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_values": ["crowbar-ask-first", "plan"],
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    ask_lever(&mut peer, json!({ "op": "models" }));
    let refused = ask_lever(&mut peer, json!({ "op": "set-default-mode", "mode": "full-access" }));
    assert_eq!(refused["code"], "invalid-mode", "{refused}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_answer_the_database_does_not_take_is_kept_and_saved_when_it_does() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE ON requests");
    assert_eq!(
        ask_lever(
            &mut peer,
            json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" })
        ),
        json!({ "reply": "ok" })
    );
    // Who watches is told, and whoever asks hears it answered.
    let answered = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (answered["state"].as_str(), answered["answer"].as_str()),
        (Some("answered"), Some("once"))
    );
    read_until(&mut peer, |f| f["event"] == "done");
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], false, "{list}");
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][0]["requests"][0]["state"], "answered");

    // The record gets it when the database takes writes again.
    db.execute_batch("DROP TRIGGER refused").unwrap();
    wait_until("the answer is saved", || {
        db.query_row("SELECT state FROM requests", [], |r| r.get::<_, String>(0))
            .is_ok_and(|state| state == "answered")
    });
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_the_harness_withdraws_is_answered_cancelled_and_withdrawn_for_the_person() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" },
                      "withdraw_after_ms": 300 },
                    { "kind": "text", "text": "Done." }
                ]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "request" && f["state"] == "waiting");
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(withdrawn["state"], "withdrawn");
    read_until(&mut peer, |f| f["event"] == "stopped");
    wait_until("the harness has its answer", || stub_heard(&stub_log) == ["cancelled"]);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_in_another_mode_tells_who_watches_the_threads_mode() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    let mut shell = handshake(&lever.surface);
    ask_lever(&mut shell, json!({ "op": "resume", "thread": thread }));

    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "hello", "mode": "full-access" }),
    )
    .unwrap();
    let updated = read_until(&mut shell, |f| {
        f["event"] == "thread-updated" || f["event"] == "message"
    });
    assert_eq!(
        updated,
        json!({ "event": "thread-updated", "thread": thread, "mode": "full-access" })
    );
    read_until(&mut shell, |f| f["event"] == "done");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_that_does_not_say_its_kind_leaves_its_acts_kind() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" },
                            "request_without_kind": true }]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    let act = read_until(&mut peer, |f| f["event"] == "act");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][0]["acts"][0]["kind"], act["kind"], "{resumed}");
    assert_eq!(request["kind"], "other");
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn turns_the_person_asked_to_stop_end_as_stopped_when_the_harness_is_ended() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "one", "steps": [{ "kind": "text", "text": "One" }], "stall": true, "ignore_cancel": true },
                { "match": "two", "steps": [{ "kind": "text", "text": "Two" }], "stall": true, "ignore_cancel": true }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "600"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (one, _) = send(&mut peer, None, "one");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == one.as_str());
    let (two, _) = send(&mut peer, None, "two");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == two.as_str());
    ask_lever(&mut peer, json!({ "op": "stop", "thread": one }));
    std::thread::sleep(Duration::from_millis(200));
    ask_lever(&mut peer, json!({ "op": "stop", "thread": two }));
    let mut ended = std::collections::BTreeMap::new();
    while ended.len() < 2 {
        let frame = read_until(&mut peer, |f| {
            ["stopped", "interrupted"].contains(&f["event"].as_str().unwrap_or(""))
        });
        ended.insert(
            frame["thread"].as_str().unwrap().to_string(),
            frame["event"].as_str().unwrap().to_string(),
        );
    }
    assert_eq!(ended[&one], "stopped");
    assert_eq!(ended[&two], "stopped", "the person asked to stop it too");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_ends_when_told_to_stop_leaves_nothing_of_the_turn_running() {
    let dir = TempDir::new();
    let uid = my_uid();
    let detached_file = dir.0.join("detached.pid");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "spawn", "pid_file": detached_file.to_str().unwrap(), "detach": true },
                    { "kind": "text", "text": "Working" }
                ],
                "stall": true,
                "exit_on_cancel": true
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "text");
    let detached: u32 = std::fs::read_to_string(&detached_file).unwrap().trim().parse().unwrap();
    assert!(alive(detached), "the command it left behind runs");

    // The harness ends on the Stop, before it ends the turn.
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    let end = read_until(&mut peer, |f| {
        ["stopped", "interrupted"].contains(&f["event"].as_str().unwrap_or(""))
    });
    assert_eq!(end["event"], "stopped");
    wait_until("the turn's command ends", || !alive(detached));
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_never_answers_a_stopped_turns_mode_is_ended_and_the_thread_is_free() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 60000,
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "500"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    send(&mut peer, Some(&thread), "hello again");
    wait_until("Lever asks for Full access", || stub_modes_set(&stub_log) == ["build"]);
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");

    // The harness never answers: after the Stop's deadline it is ended, and
    // the thread takes a mode again.
    wait_until("the thread is free", || {
        ask_lever(
            &mut peer,
            json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
        )["reply"]
            == "mode-set"
    });
    assert!(
        lever.log_text().contains("did not answer a change of mode"),
        "{}",
        lever.log_text()
    );
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_the_harness_ignores_ends_what_it_started_and_not_what_an_earlier_harness_left() {
    let dir = TempDir::new();
    let uid = my_uid();
    let earlier_file = dir.0.join("earlier.pid");
    let stuck_file = dir.0.join("stuck.pid");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "leave",
                    "steps": [
                        { "kind": "spawn", "pid_file": earlier_file.to_str().unwrap(), "detach": true },
                        { "kind": "text", "text": "Left a server running." }
                    ]
                },
                {
                    "match": "ignore",
                    "steps": [
                        { "kind": "spawn", "pid_file": stuck_file.to_str().unwrap(), "detach": true },
                        { "kind": "text", "text": "Working" }
                    ],
                    "stall": true,
                    "ignore_cancel": true
                }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(
        &script,
        &stub_log,
        &["--idle-timeout-ms", "300", "--stop-timeout-ms", "500"],
    );
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    // One harness leaves a command running in the background, and is
    // stopped when idle.
    send(&mut peer, None, "leave");
    read_until(&mut peer, |f| f["event"] == "done");
    let earlier: u32 = std::fs::read_to_string(&earlier_file).unwrap().trim().parse().unwrap();
    wait_until("the idle harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });

    // The next one ignores a Stop: what it started ends, and what the one
    // before left goes on.
    let (stuck, _) = send(&mut peer, None, "ignore");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == stuck.as_str());
    let mine: u32 = std::fs::read_to_string(&stuck_file).unwrap().trim().parse().unwrap();
    ask_lever(&mut peer, json!({ "op": "stop", "thread": stuck }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    wait_until("the stopped turn's command ends", || !alive(mine));
    let still = alive(earlier);
    unsafe { libc::kill(earlier as libc::pid_t, libc::SIGKILL) };
    assert!(still, "what an earlier harness left is not this one's to end");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn how_a_request_ends_is_told_though_the_database_can_be_neither_read_nor_written() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE requests RENAME TO requests_away")
        .unwrap();
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (
            withdrawn["request"].clone(),
            withdrawn["state"].as_str(),
            withdrawn["detail"].as_str()
        ),
        (request["request"].clone(), Some("withdrawn"), Some("echo hi && id -un"))
    );
    db.execute_batch("ALTER TABLE requests_away RENAME TO requests")
        .unwrap();
    wait_until("the end is saved", || {
        db.query_row("SELECT state FROM requests", [], |r| r.get::<_, String>(0))
            .is_ok_and(|state| state == "withdrawn")
    });
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_lever_holds_is_answered_and_a_working_thread_stopped_though_its_record_cannot_be_read() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE threads RENAME TO threads_away").unwrap();
    assert_eq!(
        ask_lever(
            &mut peer,
            json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" })
        ),
        json!({ "reply": "ok" })
    );
    read_until(&mut peer, |f| f["event"] == "request" && f["state"] == "answered");
    db.execute_batch("ALTER TABLE threads_away RENAME TO threads").unwrap();
    read_until(&mut peer, |f| f["event"] == "done" || f["event"] == "failed");

    send(&mut peer, Some(&thread), "please stall");
    read_until(&mut peer, |f| f["event"] == "text");
    db.execute_batch("ALTER TABLE threads RENAME TO threads_away").unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread })),
        json!({ "reply": "ok" })
    );
    read_until(&mut peer, |f| f["event"] == "stopped");
    db.execute_batch("ALTER TABLE threads_away RENAME TO threads").unwrap();
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn list_watch_tracks_terminal_work_and_archive_preserves_history_without_a_harness() {
    let dir = TempDir::new();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        r#"{"turns":[{"match":"slow","stall":true,"steps":[{"kind":"text","text":"working"}]}]}"#,
    )
    .unwrap();
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut watcher = handshake(&lever.surface);
    write_frame(&mut watcher, &json!({"op":"list","watch":true})).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["threads"], json!([]));
    let mut terminal = handshake(&lever.person);
    write_frame(&mut terminal, &json!({"op":"send","text":"slow","cwd":"/tmp"})).unwrap();
    let sent = read_until(&mut terminal, |v| v["reply"] == "sent");
    let tid = &sent["thread"];
    let list = read_until(&mut watcher, |v| v["event"] == "threads");
    assert_eq!(list["threads"][0]["thread"], *tid);
    assert_eq!(list["threads"][0]["cwd"], "/tmp");
    assert_eq!(list["threads"][0]["state"], "working");
    let mut admin = handshake(&lever.person);
    write_frame(&mut admin, &json!({"op":"delete","thread":tid})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["code"], "working");
    write_frame(&mut admin, &json!({"op":"archive","thread":tid})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["reply"], "archived");
    let hidden = read_until(&mut watcher, |v| v["event"] == "threads");
    assert_eq!(hidden["threads"][0]["archived"], true);
    assert_eq!(hidden["threads"][0]["working"], true);
    write_frame(&mut admin, &json!({"op":"list"})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["threads"], json!([]));
    write_frame(&mut admin, &json!({"op":"stop","thread":tid})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["reply"], "ok");
    let ended = read_until(&mut watcher, |v| v["event"] == "threads" && v["finished"] == true);
    assert_eq!(ended["threads"][0]["state"], "stopped");
    // Stop/restart with no harness configured: archive persists, resume reads SQLite alone.
    lever.signal(libc::SIGTERM);
    assert!(lever.wait().success());
    let mut lever = RunningLever::start(&dir.0, my_uid(), my_uid());
    let mut peer = handshake(&lever.surface);
    write_frame(&mut peer, &json!({"op":"list"})).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["threads"], json!([]));
    write_frame(&mut peer, &json!({"op":"resume","thread":tid})).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["turns"][0]["message"], "slow");
    write_frame(&mut peer, &json!({"op":"status"})).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);
    lever.signal(libc::SIGTERM);
    assert!(lever.wait().success());
}

#[test]
fn delete_removes_the_record_and_asks_the_harness_but_keeps_history_on_failure() {
    for fails in [false, true] {
        let dir = TempDir::new();
        let script = dir.0.join("script.json");
        let rpc = dir.0.join("rpc.jsonl");
        let mut spec = json!({"delete_session":true,"turns":[{"steps":[{"kind":"text","text":"answer"}]}]});
        if fails {
            spec["delete_session_error"] = json!({"code":-32603,"message":"Could not delete session"});
        }
        std::fs::write(&script, spec.to_string()).unwrap();
        let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc), None);
        let mut peer = handshake(&lever.person);
        write_frame(&mut peer, &json!({"op":"send","text":"hello","cwd":"/tmp"})).unwrap();
        let sent = read_until(&mut peer, |v| v["reply"] == "sent");
        let tid = sent["thread"].clone();
        read_until(&mut peer, |v| v["event"] == "done");
        // Restart so deletion must initialize the harness again, without loading history.
        lever.signal(libc::SIGTERM);
        assert!(lever.wait().success());
        let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc), None);
        let mut peer = handshake(&lever.person);
        write_frame(&mut peer, &json!({"op":"delete","thread":tid})).unwrap();
        let reply = read_until(&mut peer, |v| v.get("reply").is_some());
        assert_eq!(reply["reply"], if fails { "error" } else { "deleted" });
        write_frame(&mut peer, &json!({"op":"resume","thread":tid})).unwrap();
        let record = read_until(&mut peer, |v| v.get("reply").is_some());
        assert_eq!(record["reply"], if fails { "resumed" } else { "error" });
        assert!(std::fs::read_to_string(&rpc).unwrap().contains("session/delete"));
        let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
        let turns: i64 = db.query_row("SELECT count(*) FROM turns", [], |r| r.get(0)).unwrap();
        assert_eq!(turns, if fails { 1 } else { 0 });
        lever.signal(libc::SIGTERM);
        assert!(lever.wait().success());
    }
}

/// A script with three Models from two providers: one with effort levels,
/// one with fewer, and one with none.
fn effort_script(dir: &Path, more: Value) -> PathBuf {
    let mut script = json!({
        "models": [
            { "id": "stub/stub-1", "name": "stub/Stub One", "efforts": ["low", "high"] },
            { "id": "stub/stub-2", "name": "stub/Stub Two" },
            { "id": "other/stub-3", "name": "other/Stub Three", "efforts": ["low"] }
        ],
        "default_model": "stub/stub-1",
        // Every turn says the Model and the effort its session is at.
        "turns": [{ "match": "", "steps": [{ "kind": "text", "text": "{model} at {effort}" }] }]
    });
    for (key, value) in more.as_object().unwrap() {
        script[key] = value.clone();
    }
    write_script(dir, &script)
}

#[test]
fn models_say_each_models_provider_apart_from_its_name() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let models = ask_lever(&mut peer, json!({ "op": "models" }));
    assert_eq!(
        models["models"],
        json!([
            { "id": "stub/stub-1", "name": "Stub One", "provider": "stub", "provider_name": "stub" },
            { "id": "stub/stub-2", "name": "Stub Two", "provider": "stub", "provider_name": "stub" },
            { "id": "other/stub-3", "name": "Stub Three", "provider": "other", "provider_name": "other" }
        ])
    );
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_models_effort_levels_are_read_from_the_harness_and_kept() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&stub_log), None);
    let mut peer = handshake(&lever.person);

    // The harness is started for the asking, and a session opened, set to
    // the Model, read, and closed.
    let levels = ask_lever(&mut peer, json!({ "op": "efforts", "model": "other/stub-3" }));
    assert_eq!(
        levels,
        json!({ "reply": "efforts", "model": "other/stub-3", "efforts": [
            { "id": "low", "name": "Low" },
            { "id": "default", "name": "Default" }
        ]})
    );
    assert_eq!(stub_config_set(&stub_log, "model"), ["other/stub-3"]);
    let methods = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|v| v["method"].as_str().map(str::to_string))
            .collect()
    };
    wait_until("the session to be closed", || {
        methods(&stub_log)
            == [
                "initialize",
                "session/new",
                "session/set_config_option",
                "session/close",
            ]
    });

    // What Lever has read, it does not read again.
    let again = ask_lever(&mut peer, json!({ "op": "efforts", "model": "other/stub-3" }));
    assert_eq!(again, levels);
    assert_eq!(methods(&stub_log).len(), 4);

    // The Model a new session is at needs no setting, and one with no
    // levels has an empty list.
    let default = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/stub-1" }));
    assert_eq!(
        default["efforts"],
        json!([
            { "id": "low", "name": "Low" },
            { "id": "high", "name": "High" },
            { "id": "default", "name": "Default" }
        ])
    );
    let none = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/stub-2" }));
    assert_eq!(none["efforts"], json!([]));
    assert_eq!(stub_config_set(&stub_log, "model"), ["other/stub-3", "stub/stub-2"]);

    // A Model the harness is known not to offer has no levels to read.
    ask_lever(&mut peer, json!({ "op": "models" }));
    let unknown = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/none" }));
    assert_eq!(unknown["code"], "invalid-model", "{unknown}");
    let bad = ask_lever(&mut peer, json!({ "op": "efforts" }));
    assert_eq!(bad["code"], "invalid-request", "{bad}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_keeps_its_effort_and_each_turn_runs_at_it() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // A new thread's first message says its effort, and who watches is told
    // the thread's Model and effort as Lever knows them.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let sent = read_until(&mut peer, |f| f.get("reply").is_some());
    let thread = sent["thread"].as_str().unwrap().to_string();
    let events = turn_events(&mut peer, &thread, 1);
    assert_eq!(text_of(&events), "stub/stub-1 at high");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);
    let record = resume(&mut peer, &thread);
    assert_eq!(
        (&record["model"], &record["effort"]),
        (&json!("stub/stub-1"), &json!("high"))
    );
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["effort"], "high");

    // The next turn asks the harness for nothing: the session is at it.
    let (_, turn) = send(&mut peer, Some(&thread), "again");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at high");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    // `set-effort` applies from the next turn, and is refused for a level
    // the thread's Model is known not to have.
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-effort", "thread": thread, "effort": "low" }),
    );
    assert_eq!(set, json!({ "reply": "effort-set", "thread": thread, "effort": "low" }));
    assert_eq!(
        read_frame(&mut peer).unwrap(),
        json!({ "event": "thread-updated", "thread": thread, "effort": "low" })
    );
    let refused = ask_lever(
        &mut peer,
        json!({ "op": "set-effort", "thread": thread, "effort": "max" }),
    );
    assert_eq!(refused["code"], "invalid-effort", "{refused}");
    let refused = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "no", "effort": "max" }),
    );
    assert_eq!(refused["code"], "invalid-effort", "{refused}");
    let (_, turn) = send(&mut peer, Some(&thread), "lower");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at low");

    // The effort outlasts Lever and the harness: a harness started again has
    // the session at its own effort, and Lever sets the thread's.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    assert_eq!(resume(&mut peer, &thread)["effort"], "low");
    let (_, turn) = send(&mut peer, Some(&thread), "after a restart");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at low");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high", "low", "low"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_effort_is_one_models_and_a_thread_given_another_model_takes_what_the_harness_has() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    turn_events(&mut peer, &thread, 1);

    // A Model that has no `high`: it is not asked for, and the thread is at
    // the effort the harness has the session at.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "on three", "model": "other/stub-3" }),
    )
    .unwrap();
    let mut told = Vec::new();
    let events = loop {
        let frame = read_frame(&mut peer).unwrap();
        if frame["event"] == "thread-updated" {
            told.push(frame);
        } else if frame["event"] == "done" {
            break told;
        } else if frame["event"] == "text" {
            assert_eq!(frame["text"], "other/stub-3 at default");
        }
    };
    assert_eq!(
        events,
        [
            json!({ "event": "thread-updated", "thread": thread, "model": "other/stub-3" }),
            json!({ "event": "thread-updated", "thread": thread, "effort": "default" }),
        ]
    );
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    // `set-model` on the thread's session: a Model with no levels leaves the
    // thread with no effort.
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-model", "thread": thread, "model": "stub/stub-2" }),
    );
    assert_eq!(set["reply"], "model-set", "{set}");
    assert_eq!(
        [read_frame(&mut peer).unwrap(), read_frame(&mut peer).unwrap()],
        [
            json!({ "event": "thread-updated", "thread": thread, "model": "stub/stub-2" }),
            json!({ "event": "thread-updated", "thread": thread, "effort": null }),
        ]
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(record["model"], "stub/stub-2");
    assert!(record.get("effort").is_none(), "{record}");
    // Lever has that Model's levels from the session, with no session of
    // its own opened to read them.
    let news = |log: &Path| {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .matches("\"session/new\"")
            .count()
    };
    let before = news(&stub_log);
    let none = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/stub-2" }));
    assert_eq!(none["efforts"], json!([]));
    assert_eq!(news(&stub_log), before);
    let (_, turn) = send(&mut peer, Some(&thread), "on two");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-2 at none");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_with_no_session_given_another_model_has_no_effort_until_its_next_turn() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    turn_events(&mut peer, &thread, 1);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Lever started again holds no session of the thread's.
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    resume(&mut peer, &thread);
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-model", "thread": thread, "model": "other/stub-3" }),
    );
    assert_eq!(set["reply"], "model-set", "{set}");
    assert_eq!(
        [read_frame(&mut peer).unwrap(), read_frame(&mut peer).unwrap()],
        [
            json!({ "event": "thread-updated", "thread": thread, "model": "other/stub-3" }),
            json!({ "event": "thread-updated", "thread": thread, "effort": null }),
        ]
    );
    let (_, turn) = send(&mut peer, Some(&thread), "on three");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "other/stub-3 at default");
    assert_eq!(resume(&mut peer, &thread)["effort"], "default");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_whose_effort_the_harness_refuses_fails_and_the_thread_goes_back_to_the_sessions() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "refuse_effort": "high" }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let events = turn_events(&mut peer, &thread, 1);
    assert_eq!(events.last().unwrap()["event"], "failed", "{events:?}");
    assert!(stub_prompts(&stub_log).is_empty(), "the prompt was sent");
    assert_eq!(resume(&mut peer, &thread)["effort"], "default");

    // The next turn asks for nothing the harness refused.
    let (_, turn) = send(&mut peer, Some(&thread), "again");
    assert_eq!(
        text_of(&turn_events(&mut peer, &thread, turn)),
        "stub/stub-1 at default"
    );
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_stopped_while_its_effort_is_set_holds_the_thread_until_the_harness_answers() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "set_config_delay_ms": 600 }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");

    // The harness takes the effort at once, and answers late; the turn is
    // stopped before it answers.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "harder", "effort": "high" }),
    )
    .unwrap();
    wait_until("Lever asks for the effort", || {
        stub_config_set(&stub_log, "effort") == ["high"]
    });
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    assert_eq!(stub_prompts(&stub_log).len(), 1, "the stopped turn's prompt was sent");

    // Until it answers, the thread takes no message and no other effort.
    let busy = ask_lever(&mut peer, json!({ "op": "send", "thread": thread, "text": "now" }));
    assert_eq!(busy["code"], "working", "{busy}");
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "set-effort", "thread": thread, "effort": "low" }),
    );
    assert_eq!(busy["code"], "working", "{busy}");

    // Then the thread is free, and its next turn runs at the effort it has.
    wait_until("the thread is free", || {
        ask_lever(
            &mut peer,
            json!({ "op": "set-effort", "thread": thread, "effort": "high" }),
        )["reply"]
            == "effort-set"
    });
    let (_, turn) = send(&mut peer, Some(&thread), "now");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at high");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn the_harness_s_commands_are_kept_for_each_thread_and_a_command_is_sent_as_a_message() {
    let dir = TempDir::new();
    let commands = json!([
        { "name": "review", "description": "review changes", "hint": "commit, branch, or pr" },
        { "name": "compact", "description": "Compact the session" }
    ]);
    let script = write_script(
        &dir.0,
        &json!({
            "commands": commands,
            "turns": [{ "steps": [{ "kind": "text", "text": "Reviewed." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"],
        false
    );

    // Lever answers at once that it has none yet, starts the harness, and
    // tells who watches what the harness names.
    ask_lever(&mut peer, json!({ "op": "watch" }));
    let unknown = ask_lever(&mut peer, json!({ "op": "commands" }));
    assert_eq!(unknown, json!({ "reply": "commands", "known": false, "commands": [] }));
    let told = read_until(&mut peer, |f| f["event"] == "commands");
    assert_eq!(told, json!({ "event": "commands", "commands": commands }));
    let known = ask_lever(&mut peer, json!({ "op": "commands" }));
    assert_eq!(
        known,
        json!({ "reply": "commands", "known": true, "commands": commands })
    );

    // A command is a message that begins with `/` and its name, sent as it
    // is. The thread's session names its commands, and the thread has them.
    let (thread, turn) = send(&mut peer, None, "/review the last commit");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Reviewed.");
    assert_eq!(stub_prompts(&stub_log), ["/review the last commit"]);
    let of_thread = ask_lever(&mut peer, json!({ "op": "commands", "thread": thread }));
    assert_eq!(
        of_thread,
        json!({ "reply": "commands", "known": true, "thread": thread, "commands": commands })
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["message"], "/review the last commit");

    let missing = ask_lever(&mut peer, json!({ "op": "commands", "thread": "t-none" }));
    assert_eq!(missing["code"], "no-thread", "{missing}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_threads_commands_come_as_an_event_to_who_watches_it() {
    let dir = TempDir::new();
    let commands = json!([{ "name": "review", "description": "review changes" }]);
    let script = write_script(
        &dir.0,
        &json!({
            "commands": commands,
            "turns": [{ "steps": [{ "kind": "pause_ms", "ms": 50 }, { "kind": "text", "text": "Hello." }] }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "hello");
    let told = read_until(&mut peer, |f| f["event"] == "commands");
    assert_eq!(
        told,
        json!({ "event": "commands", "thread": thread, "commands": commands })
    );
    // They are no part of the thread's record: nothing of them is a note.
    let events = turn_events(&mut peer, &thread, turn);
    assert!(events.iter().all(|e| e["event"] != "note"), "{events:?}");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["notes"], json!([]));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_record_says_when_each_turn_began_and_ended() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [
            { "steps": [{ "kind": "text", "text": "Hello." }] },
            { "steps": [{ "kind": "text", "text": "Wait" }], "stall": true }
        ]}),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let (thread, turn) = send(&mut peer, None, "hello");
    turn_events(&mut peer, &thread, turn);
    send(&mut peer, Some(&thread), "wait");
    read_until(&mut peer, |f| f["event"] == "text");
    let record = resume(&mut peer, &thread);
    let (first, second) = (&record["turns"][0], &record["turns"][1]);
    let began = first["started"].as_u64().unwrap();
    assert!(began >= before && began <= before + 60, "{first}");
    assert!(first["ended"].as_u64().unwrap() >= began, "{first}");
    assert!(second["started"].as_u64().unwrap() >= began, "{second}");
    assert!(second.get("ended").is_none(), "a working turn has not ended: {second}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_effort_answered_with_the_one_before_is_taken_and_the_update_after_says_so() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "effort_answer_lags": true }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // As OpenCode answers while another of its sessions works: with the
    // effort the session was at, and the new one in an update that follows.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "model": "other/stub-3", "effort": "low" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let events = turn_events(&mut peer, &thread, 1);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");
    assert_eq!(text_of(&events), "other/stub-3 at low");
    assert_eq!(
        stub_config_set(&stub_log, "effort"),
        ["low"],
        "the effort is asked for once"
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(
        (&record["model"], &record["effort"]),
        (&json!("other/stub-3"), &json!("low"))
    );

    // The next turn has nothing to set.
    let (_, turn) = send(&mut peer, Some(&thread), "again");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "other/stub-3 at low");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["low"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_catalog_update_between_set_effort_and_send_keeps_the_picked_effort() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "emit_catalog_update_on_new_session": true }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // Turn 1 starts a live session at the default effort ("default").
    let (thread, turn) = send(&mut peer, None, "first");
    assert_eq!(
        text_of(&turn_events(&mut peer, &thread, turn)),
        "stub/stub-1 at default"
    );

    // Pick "high" while idle (saved for the next turn; live session is still "default").
    write_frame(
        &mut peer,
        &json!({ "op": "set-effort", "thread": thread, "effort": "high" }),
    )
    .unwrap();
    let reply = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(reply["reply"], "effort-set", "{reply}");
    assert_eq!(reply["effort"], "high", "{reply}");

    // Trigger a catalog update on the idle session (via an `efforts` probe for
    // another Model, which opens a new probe session and emits a
    // `config_option_update` naming the live session's previous "default" effort).
    write_frame(&mut peer, &json!({ "op": "efforts", "model": "other/stub-3" })).unwrap();
    let efforts_reply = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(efforts_reply["reply"], "efforts", "{efforts_reply}");

    // The saved effort must still be "high", and the next turn must apply it.
    let record = resume(&mut peer, &thread);
    assert_eq!(record["effort"], "high", "{record}");
    let (_, turn2) = send(&mut peer, Some(&thread), "second");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn2)), "stub/stub-1 at high");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn commands_refuses_with_no_harness_and_does_not_reprobe_when_harness_names_none() {
    // Without a harness configured, `commands` fails immediately with `no-harness`.
    let dir_none = TempDir::new();
    let mut lever_none = RunningLever::start(&dir_none.0, my_uid(), my_uid());
    let mut peer_none = handshake(&lever_none.person);
    write_frame(&mut peer_none, &json!({ "op": "commands" })).unwrap();
    let reply = read_frame(&mut peer_none).unwrap();
    assert_eq!(reply["reply"], "error", "{reply}");
    assert_eq!(reply["code"], "no-harness", "{reply}");
    lever_none.signal(libc::SIGTERM);
    assert_eq!(lever_none.wait().code(), Some(0));

    // With a harness that never emits `available_commands_update`, repeated
    // `commands` polls must not spawn repeated probe sessions.
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{ "steps": [{ "kind": "text", "text": "ok" }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // First `commands` triggers one probe; wait for models so the probe finishes.
    write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
    let first = read_frame(&mut peer).unwrap();
    assert_eq!(first["reply"], "commands", "{first}");
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut peer).unwrap();
    assert_eq!(models["reply"], "models", "{models}");

    // Polling `commands` repeatedly afterwards opens no further probe sessions.
    for _ in 0..5 {
        write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
        let polled = read_frame(&mut peer).unwrap();
        assert_eq!(polled["reply"], "commands", "{polled}");
    }
    let log = std::fs::read_to_string(&stub_log).unwrap();
    let new_sessions = log
        .lines()
        .filter(|line| line.contains("\"method\":\"session/new\""))
        .count();
    assert_eq!(new_sessions, 1, "expected exactly one probe session, log:\n{log}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn restart_during_writes_preserves_every_thread_and_loads_sessions_without_replaying_work() {
    for kill_lever in [false, true] {
        let dir = TempDir::new();
        let uid = my_uid();
        // Keep producing database writes after the first text is acknowledged.
        let chunk = "recorded ".repeat(64);
        let steps: Vec<_> = (0..1000)
            .flat_map(|_| [json!({"kind":"text","text":chunk}), json!({"kind":"pause_ms","ms":2})])
            .collect();
        let script = write_script(
            &dir.0,
            &json!({
                "modes": true,
                "turns": [
                    {"match":"write continuously", "steps":steps},
                    {"match":"wait for permission", "steps":[
                        {"kind":"ask","id":"waiting","title":"echo allowed","raw_input":{"command":"echo allowed"}},
                        {"kind":"text","text":"Should never run"}
                    ]},
                    {"match":"finish", "steps":[{"kind":"text","text":"Kept answer"}]}
                ]
            }),
        );
        let rpc_log = dir.0.join("rpc.jsonl");
        let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
        let mut peer = handshake(&lever.person);
        let (finished, _) = send(&mut peer, None, "finish earlier work");
        read_until(&mut peer, |f| f["event"] == "done");
        let earlier = resume(&mut peer, &finished);
        assert_eq!(
            ask_lever(&mut peer, json!({"op":"archive", "thread":finished}))["reply"],
            "archived"
        );
        let (waiting, _) = send(&mut peer, None, "wait for permission");
        let request = read_until(&mut peer, |f| f["event"] == "request");
        let (writing, _) = send(&mut peer, None, "write continuously");
        read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == writing);
        let children = std::fs::read_to_string(format!("/proc/{}/task/{}/children", lever.pid(), lever.pid())).unwrap();
        let children: Vec<u32> = children.split_whitespace().map(|p| p.parse().unwrap()).collect();
        assert_eq!(children.len(), 1, "one harness serves the threads");
        let harness_pid = children[0];
        let original_sessions: Vec<Value> = std::fs::read_to_string(&rpc_log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|rpc| rpc["method"] == "session/prompt")
            .map(|rpc| rpc["params"]["sessionId"].clone())
            .collect();
        assert_eq!(original_sessions.len(), 3);

        if kill_lever {
            lever.kill_and_wait();
        } else {
            // SAFETY: the harness is the direct child of this test's Lever.
            assert_eq!(unsafe { libc::kill(harness_pid as libc::pid_t, libc::SIGKILL) }, 0);
            read_until(&mut peer, |f| f["event"] == "interrupted" && f["thread"] == writing);
        }
        wait_until("the old harness to die", || !alive(harness_pid));
        if kill_lever {
            lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
        }
        let mut peer = handshake(&lever.person);
        assert_eq!(ask_lever(&mut peer, json!({"op":"status"}))["harness_started"], false);
        assert_eq!(resume(&mut peer, &finished)["turns"], earlier["turns"]);
        let record = resume(&mut peer, &waiting);
        assert_eq!(record["turns"].as_array().unwrap().len(), 1);
        assert_eq!(record["turns"][0]["state"], "interrupted");
        assert_eq!(record["turns"][0]["requests"][0]["state"], "withdrawn");
        assert_eq!(record["turns"][0]["answer"], "");
        let rejected = ask_lever(
            &mut peer,
            json!({"op":"answer","thread":waiting,"request":request["request"],"answer":"once"}),
        );
        assert_eq!(rejected["code"], "no-request");
        let record = resume(&mut peer, &writing);
        assert_eq!(record["turns"].as_array().unwrap().len(), 1);
        assert_eq!(record["turns"][0]["state"], "interrupted");
        assert!(record["turns"][0]["answer"].as_str().unwrap().starts_with(&chunk));
        let list = ask_lever(&mut peer, json!({"op":"list", "archived":true}));
        assert_eq!(list["threads"].as_array().unwrap().len(), 3);
        assert!(
            list["threads"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["working"] == false && t["waiting"] == false)
        );
        assert_eq!(
            ask_lever(&mut peer, json!({"op":"status"}))["harness_started"],
            false,
            "reading recovered threads must not restart or repeat their work"
        );

        for thread in [&waiting, &writing] {
            assert_eq!(
                send(&mut peer, Some(thread), "finish after restart"),
                (thread.clone(), 2)
            );
            read_until(&mut peer, |f| f["event"] == "done" && f["thread"] == *thread);
            let record = resume(&mut peer, thread);
            assert_eq!(record["turns"].as_array().unwrap().len(), 2);
            assert_eq!(record["turns"][1]["answer"], "Kept answer");
            assert_eq!(record["turns"][1]["notes"], json!([]), "no fresh-session fallback");
        }
        let loaded: Vec<Value> = std::fs::read_to_string(&rpc_log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|rpc| rpc["method"] == "session/load")
            .map(|rpc| rpc["params"]["sessionId"].clone())
            .collect();
        assert_eq!(
            loaded,
            original_sessions[1..],
            "each next message loads its original session"
        );
        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
    }
}

#[test]
fn models_refresh_broadcasts_catalog_live_and_persists_signed_in_choice() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");

    // Start with no Models available when free hosted Models are off.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "no_models_when_free_disabled": true,
            "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default_model": "stub/stub-1",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut surface = handshake(&lever.surface);
    write_frame(&mut surface, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut surface).unwrap();
    assert_eq!(status["harness_started"], false);
    assert_eq!(status["free_hosted_models"], false);
    assert_eq!(status["models_chosen"], false);

    // Subscribe surface to watch events (as the shell does).
    write_frame(&mut surface, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut surface).unwrap()["reply"], "activity");

    // Person signs in to a first provider; `crowbar-agent login` sends `models` with `refresh: true`.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [{ "id": "acme/small", "name": "Small" }],
            "default_model": "acme/small",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();

    let mut person = handshake(&lever.person);
    write_frame(&mut person, &json!({ "op": "models", "refresh": true })).unwrap();
    let reply1 = read_frame(&mut person).unwrap();
    assert_eq!(reply1["reply"], "models");
    assert_eq!(reply1["models"].as_array().unwrap().len(), 1);

    let event1 = read_until(&mut surface, |v| v["event"] == "models");
    assert_eq!(event1["models"][0]["id"], "acme/small");
    assert_eq!(event1["free_hosted_models"], false);

    // Person signs in to a second provider while the catalog is already non-empty.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [
                { "id": "acme/small", "name": "Small" },
                { "id": "other/large", "name": "Large" }
            ],
            "default_model": "acme/small",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();
    write_frame(&mut person, &json!({ "op": "models", "refresh": true })).unwrap();
    let reply2 = read_frame(&mut person).unwrap();
    assert_eq!(reply2["models"].as_array().unwrap().len(), 2);

    let event2 = read_until(&mut surface, |v| v["event"] == "models");
    let ids: Vec<&str> = event2["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["acme/small", "other/large"]);

    // Restart Lever: idle status remembers `models_chosen: true` without starting the harness.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let mut restarted = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut surface2 = handshake(&restarted.surface);
    write_frame(&mut surface2, &json!({ "op": "status" })).unwrap();
    let status2 = read_frame(&mut surface2).unwrap();
    assert_eq!(status2["harness_started"], false);
    assert_eq!(status2["free_hosted_models"], false);
    assert_eq!(status2["models_chosen"], true);

    restarted.signal(libc::SIGTERM);
    assert_eq!(restarted.wait().code(), Some(0));
}

#[test]
fn unreachable_model_transport_retry_loop_fails_the_turn_with_reason() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "unknown",
                        "update": {
                            "sessionUpdate": "session_info_update",
                            "_meta": {
                                "opencode/retry": {
                                    "attempt": 2,
                                    "error": {
                                        "type": "provider.transport",
                                        "message": "ECONNREFUSED: Unable to connect."
                                    }
                                }
                            }
                        }
                    },
                    {
                        "kind": "unknown",
                        "update": {
                            "sessionUpdate": "session_info_update",
                            "_meta": {
                                "opencode/retry": {
                                    "attempt": 3,
                                    "error": {
                                        "type": "provider.transport",
                                        "message": "ECONNREFUSED: Unable to connect."
                                    }
                                }
                            }
                        }
                    }
                ],
                "stall": true
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "hello unreachable");
    let failed = read_until(&mut peer, |v| v["event"] == "failed");
    assert_eq!(failed["thread"], thread);
    assert_eq!(failed["turn"], turn);
    assert!(
        failed["message"]
            .as_str()
            .unwrap_or("")
            .contains("ECONNREFUSED: Unable to connect."),
        "{failed}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn models_refresh_while_busy_defers_harness_restart_until_turn_finishes() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");

    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [{ "id": "acme/small", "name": "Small" }],
            "default_model": "acme/small",
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "text", "text": "working..." }],
                "stall": true
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut surface = handshake(&lever.surface);
    write_frame(&mut surface, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut surface).unwrap()["reply"], "activity");

    let mut person = handshake(&lever.person);
    let (thread, _) = send(&mut person, None, "keep busy");
    read_until(&mut person, |v| v["event"] == "text");

    // Sign-in finishes while the thread is still working; credentials update on
    // disk and `crowbar-agent login` sends `models` with `refresh: true`.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [
                { "id": "acme/small", "name": "Small" },
                { "id": "other/new", "name": "New" }
            ],
            "default_model": "acme/small",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();
    write_frame(&mut person, &json!({ "op": "models", "refresh": true })).unwrap();
    let busy_reply = read_frame(&mut person).unwrap();
    assert_eq!(busy_reply["reply"], "models");

    // Stop the active turn: Lever restarts the harness and broadcasts the new catalog.
    write_frame(&mut person, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut person, |v| v["event"] == "stopped");

    let refreshed = read_until(&mut surface, |v| {
        v["event"] == "models"
            && v["models"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|m| m["id"] == "other/new"))
    });
    assert_eq!(refreshed["models"].as_array().unwrap().len(), 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}
