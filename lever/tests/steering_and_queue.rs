//! Steering and ADR 0037 queued-message FIFO drain, unqueue, stop, and interrupt.

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
fn a_message_sent_while_a_turn_works_is_steered_into_it_where_the_harness_can() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "steering": true,
            "turns": [{
                "match": "long task",
                "steps": [
                    { "kind": "text", "text": "Looking. " },
                    { "kind": "await_steer", "text": "Took: {steer}" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let mut other = handshake(&lever.person);

    // The adapter says its harness can steer a turn.
    write_frame(&mut other, &json!({ "op": "models" })).unwrap();
    let models = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(models["steering"], true, "{models}");

    let (thread, turn) = send(&mut peer, None, "Do the long task");
    read_until(&mut peer, |f| f["event"] == "text" && f["text"] == "Looking. ");

    // A second client sends while the turn works: it is taken into that turn.
    write_frame(
        &mut other,
        &json!({ "op": "send", "thread": thread, "text": "Also check the docs" }),
    )
    .unwrap();
    let steered = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(
        steered,
        json!({ "reply": "steered", "thread": thread, "turn": turn }),
        "{steered}"
    );

    // Who watches the thread sees where the message was taken, then the rest.
    let events = turn_events(&mut peer, &thread, turn);
    let at = events
        .iter()
        .position(|e| e["event"] == "steered")
        .expect("a steered event");
    assert_eq!(
        events[at],
        json!({ "event": "steered", "thread": thread, "turn": turn, "text": "Also check the docs" })
    );
    assert_eq!(text_of(&events[at..]), "Took: Also check the docs");
    assert_eq!(events.last().unwrap()["event"], "done");

    // The record keeps it in its turn, at the place in the answer it was taken.
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    let kept = &record["turns"][0];
    assert_eq!(kept["answer"], "Looking. Took: Also check the docs");
    assert_eq!(
        kept["steered"],
        json!([{ "text": "Also check the docs", "at": 9 }]),
        "{kept}"
    );

    // One prompt, and the message steered into it through the adapter.
    let sent = rpc_sent(&rpc_log);
    assert_eq!(sent.iter().filter(|m| m["method"] == "session/prompt").count(), 1);
    let steer = sent
        .iter()
        .find(|m| m["method"] == "_session/steering")
        .expect("Lever steers through OpenCode's extension");
    let prompt = sent.iter().find(|m| m["method"] == "session/prompt").unwrap();
    assert_eq!(steer["params"]["sessionId"], prompt["params"]["sessionId"]);
    assert_eq!(
        steer["params"]["prompt"],
        json!([{ "type": "text", "text": "Also check the docs" }])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// A stub script whose first turn works a while and ends on its own, and
/// whose later turns answer with what they were sent.
fn queueing_script(extra: Value) -> Value {
    let mut script = json!({
        "turns": [
            { "match": "slow", "steps": [{ "kind": "text", "text": "Working. " }, { "kind": "pause_ms", "ms": 800 }, { "kind": "text", "text": "Done." }] },
            { "match": "stall", "stall": true, "steps": [{ "kind": "text", "text": "Working. " }] },
            { "match": "Change of plan", "steps": [{ "kind": "text", "text": "Changed." }] },
            { "match": "Then this", "steps": [{ "kind": "text", "text": "This too." }] }
        ]
    });
    for (key, value) in extra.as_object().unwrap() {
        script[key] = value.clone();
    }
    script
}

/// Sends a message while the thread works and reads Lever's reply.
fn send_while_working(peer: &mut UnixStream, thread: &str, text: &str) -> Value {
    write_frame(peer, &json!({ "op": "send", "thread": thread, "text": text })).unwrap();
    read_until(peer, |f| f.get("reply").is_some())
}

#[test]
fn a_message_the_harness_does_not_take_into_its_turn_waits_and_goes_as_the_next_turn() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &queueing_script(json!({ "steering": true, "steer_refused": true })),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut other = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");

    let queued = send_while_working(&mut other, &thread, "Change of plan");
    assert_eq!(queued["reply"], "queued", "{queued}");
    assert_eq!(queued["turn"], turn, "{queued}");
    let id = queued["queued"].as_str().unwrap().to_string();
    assert!(id.starts_with("q-"), "{queued}");

    // Who watches sees it wait behind the turn, then go as its own when the
    // turn ends.
    let events = turn_events(&mut peer, &thread, turn);
    assert!(
        events.iter().any(|e| *e
            == json!({ "event": "queued", "thread": thread, "turn": turn, "queued": id,
                       "text": "Change of plan", "state": "queued" })),
        "{events:?}"
    );
    assert_eq!(events.last().unwrap()["event"], "done");
    let sent = read_until(&mut peer, |f| f["event"] == "queued" && f["state"] == "sent");
    assert_eq!(sent["queued"], id);
    let next = read_until(&mut peer, |f| f["event"] == "message");
    assert_eq!(next["turn"], turn + 1);
    assert_eq!(next["text"], "Change of plan");
    let events = turn_events(&mut peer, &thread, turn + 1);
    assert_eq!(text_of(&events), "Changed.");

    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{record}");
    assert!(
        turns[0].get("queued").is_none(),
        "what went is no longer queued: {record}"
    );
    assert_eq!(turns[1]["message"], "Change of plan");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_cannot_steer_is_never_asked_to_and_messages_wait_in_order() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(&dir.0, &queueing_script(json!({})));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(models["steering"], false, "{models}");

    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");
    let first = send_while_working(&mut peer, &thread, "Never mind this");
    let second = send_while_working(&mut peer, &thread, "Change of plan");
    let third = send_while_working(&mut peer, &thread, "Then this");
    for q in [&first, &second, &third] {
        assert_eq!(q["reply"], "queued", "{q}");
    }

    // The person takes one back before it goes.
    write_frame(
        &mut peer,
        &json!({ "op": "unqueue", "thread": thread, "queued": first["queued"] }),
    )
    .unwrap();
    assert_eq!(
        read_until(&mut peer, |f| f.get("reply").is_some()),
        json!({ "reply": "ok" })
    );
    let removed = read_until(&mut peer, |f| f["event"] == "queued");
    assert_eq!(
        (&removed["queued"], &removed["state"]),
        (&first["queued"], &json!("removed"))
    );
    write_frame(
        &mut peer,
        &json!({ "op": "unqueue", "thread": thread, "queued": first["queued"] }),
    )
    .unwrap();
    assert_eq!(read_until(&mut peer, |f| f.get("reply").is_some())["code"], "no-queued");

    // The rest go in the order sent, each as its own turn.
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "done");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 1)), "Changed.");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 2)), "This too.");
    let record = resume(&mut peer, &thread);
    let messages: Vec<&str> = record["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages, ["slow work", "Change of plan", "Then this"], "{record}");
    assert!(
        !rpc_sent(&rpc_log).iter().any(|m| m["method"] == "_session/steering"),
        "a harness that does not say it can steer is never asked to"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stop_drops_what_waits_behind_the_turn_it_stops() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &queueing_script(json!({})));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "stall here");
    read_until(&mut peer, |f| f["event"] == "text");
    let queued = send_while_working(&mut peer, &thread, "Change of plan");
    assert_eq!(queued["reply"], "queued", "{queued}");

    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    let dropped = read_until(&mut peer, |f| f["event"] == "queued" && f["state"] != "queued");
    assert_eq!(dropped["state"], "dropped", "{dropped}");
    read_until(&mut peer, |f| f["event"] == "stopped");

    // Nothing goes after the person said stop, and the thread says what did not.
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert_eq!(record["working"], false, "{record}");
    assert_eq!(
        record["turns"][0]["queued"],
        json!([{ "queued": queued["queued"], "text": "Change of plan", "state": "dropped" }])
    );
    // A message after it begins a turn of its own.
    let (_, next) = send(&mut peer, Some(&thread), "Then this");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, next)), "This too.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_waits_behind_an_interrupted_turn_waits_for_the_person_and_survives_a_restart() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &queueing_script(json!({ "steering": true, "steer_exits": true })),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut other = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "stall here");
    read_until(&mut peer, |f| f["event"] == "text");

    // The harness ends before it says whether it took the message: the
    // message waits, and the turn is interrupted.
    let queued = send_while_working(&mut other, &thread, "Change of plan");
    assert_eq!(queued["reply"], "queued", "{queued}");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "interrupted");

    // It is not sent on its own, and Lever's restart keeps it.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(resume(&mut peer, &thread)["turns"].as_array().unwrap().len(), 1);
    lever.kill_and_wait();
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let record = resume(&mut peer, &thread);
    assert_eq!(
        record["turns"][0]["queued"],
        json!([{ "queued": queued["queued"], "text": "Change of plan", "state": "queued" }]),
        "{record}"
    );

    // The person's next message goes behind it, and both go, in order.
    let then = send_while_working(&mut peer, &thread, "Then this");
    assert_eq!(then["reply"], "queued", "{then}");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 1)), "Changed.");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 2)), "This too.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_waits_behind_a_turn_that_fails_goes_next() {
    let dir = TempDir::new();
    let mut script = queueing_script(json!({}));
    script["turns"].as_array_mut().unwrap().insert(
        0,
        json!({ "match": "fail", "steps": [{ "kind": "text", "text": "Trying. " }, { "kind": "pause_ms", "ms": 500 }],
                "error": { "code": -32000, "message": "boom" } }),
    );
    let script = write_script(&dir.0, &script);
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "fail please");
    read_until(&mut peer, |f| f["event"] == "text");
    assert_eq!(
        send_while_working(&mut peer, &thread, "Change of plan")["reply"],
        "queued"
    );
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "failed");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 1)), "Changed.");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stop_drops_all_that_waits_on_the_thread_though_it_waited_behind_an_older_turn() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &queueing_script(json!({})));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");
    // Both wait behind turn 1; the first goes as turn 2 and works on.
    assert_eq!(send_while_working(&mut peer, &thread, "stall now")["reply"], "queued");
    let behind = send_while_working(&mut peer, &thread, "Then this");
    assert_eq!(behind["turn"], turn, "{behind}");
    read_until(&mut peer, |f| f["event"] == "message" && f["turn"] == turn + 1);
    read_until(&mut peer, |f| f["event"] == "text" && f["turn"] == turn + 1);

    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn + 1 })).unwrap();
    let dropped = read_until(&mut peer, |f| f["event"] == "queued" && f["state"] != "queued");
    assert_eq!(
        (&dropped["queued"], &dropped["state"]),
        (&behind["queued"], &json!("dropped"))
    );
    read_until(&mut peer, |f| f["event"] == "stopped");
    std::thread::sleep(Duration::from_millis(300));
    let record = resume(&mut peer, &thread);
    assert_eq!(
        record["turns"].as_array().unwrap().len(),
        2,
        "nothing runs after Stop: {record}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_the_harness_had_yet_to_answer_keeps_its_place_ahead_of_one_sent_after() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &queueing_script(json!({ "steering": true, "steer_delay_ms": 1200 })),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut other = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");
    // Steered as the turn nears its end; the harness answers after it ended.
    write_frame(
        &mut other,
        &json!({ "op": "send", "thread": thread, "text": "Change of plan" }),
    )
    .unwrap();
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "done");
    // Sent while that answer is still to come: it waits behind it.
    assert_eq!(send_while_working(&mut peer, &thread, "Then this")["reply"], "queued");
    let first = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(first["reply"], "queued", "{first}");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 1)), "Changed.");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn + 2)), "This too.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}
