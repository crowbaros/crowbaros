use super::*;
use crate::harness::SessionHistory;

#[test]
fn opens_and_migrates_v2_database_without_threads_model_column() {
    let dir = std::env::temp_dir().join(format!(
        "crowbar-lever-store-migrate-{}-{}",
        std::process::id(),
        crate::sys::random_hex(4)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lever.db");

    // Create a schema-v2 database (without `threads.model`).
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '2');
                 CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     title TEXT NOT NULL,
                     cwd TEXT NOT NULL,
                     harness_name TEXT,
                     harness_session_id TEXT,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 INSERT INTO threads (id, title, cwd, harness_name, harness_session_id, created_at, updated_at)
                 VALUES ('t-old', 'Old thread', '/tmp', 'OpenCode', 'sess-1', 100, 100);",
        )
        .unwrap();
    }

    let store = Store::open(&db, 200).unwrap();
    let meta = store.thread_meta("t-old").unwrap().expect("migrated thread");
    assert_eq!(meta.model, None);
    store.set_thread_model("t-old", "stub/stub-1", 201).unwrap();
    let updated = store.thread_meta("t-old").unwrap().expect("updated thread");
    assert_eq!(updated.model.as_deref(), Some("stub/stub-1"));
    assert!(!store.free_hosted_models().unwrap());
    assert!(!store.provider_models_seen().unwrap());
    store.set_provider_models_seen(true).unwrap();
    assert!(store.provider_models_seen().unwrap());
    store.set_provider_models_seen(false).unwrap();
    assert!(!store.provider_models_seen().unwrap());
    store.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opens_and_migrates_v3_database_without_turns_context_column() {
    let dir = std::env::temp_dir().join(format!(
        "crowbar-lever-store-migrate-v3-{}-{}",
        std::process::id(),
        crate::sys::random_hex(4)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lever.db");

    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '3');
                 CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     title TEXT NOT NULL,
                     cwd TEXT NOT NULL,
                     harness_name TEXT,
                     harness_session_id TEXT,
                     model TEXT,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE turns (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL,
                     message TEXT NOT NULL,
                     answer TEXT NOT NULL DEFAULT '',
                     state TEXT NOT NULL,
                     error TEXT,
                     created_at INTEGER NOT NULL,
                     ended_at INTEGER,
                     UNIQUE(thread_id, turn_number)
                 );
                 INSERT INTO threads (id, title, cwd, harness_name, harness_session_id, model, created_at, updated_at)
                 VALUES ('t-v3', 'Old v3 thread', '/tmp', 'OpenCode', 'sess-1', 'stub/stub-1', 100, 100);
                 INSERT INTO turns (thread_id, turn_number, message, answer, state, created_at)
                 VALUES ('t-v3', 1, 'earlier', 'answer', 'done', 100);",
        )
        .unwrap();
    }

    let store = Store::open(&db, 200).unwrap();
    let rec = store.get_thread("t-v3").unwrap().expect("migrated v3 thread");
    assert_eq!(rec.turns.len(), 1);
    assert_eq!(rec.turns[0].context, None);

    let ctx = r#"{"workspace":1,"active":null,"locked":false,"windows":[],"displays":[]}"#;
    let turn2 = store.begin_turn("t-v3", None, "second", Some(ctx), None, 201).unwrap();
    assert_eq!(turn2, 2);
    let rec = store.get_thread("t-v3").unwrap().expect("updated v3 thread");
    assert_eq!(
        rec.turns[1].context.as_ref().and_then(|v| v.get("workspace")),
        Some(&serde_json::json!(1))
    );
    store.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opens_a_v4_database_with_no_modes_or_requests_and_keeps_both_after() {
    let dir = std::env::temp_dir().join(format!(
        "crowbar-lever-store-migrate-v4-{}-{}",
        std::process::id(),
        crate::sys::random_hex(4)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lever.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '4');
                 CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT NOT NULL,
                     harness_name TEXT, harness_session_id TEXT, model TEXT,
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE turns (id INTEGER PRIMARY KEY AUTOINCREMENT,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL, message TEXT NOT NULL, context TEXT,
                     answer TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, error TEXT,
                     created_at INTEGER NOT NULL, ended_at INTEGER, UNIQUE(thread_id, turn_number));
                 INSERT INTO threads (id, title, cwd, created_at, updated_at) VALUES ('t-v4', 'Old', '/tmp', 1, 1);
                 INSERT INTO turns (thread_id, turn_number, message, state, created_at) VALUES ('t-v4', 1, 'hi', 'done', 1);",
            )
            .unwrap();
    }
    let store = Store::open(&db, 2).unwrap();
    let rec = store.get_thread("t-v4").unwrap().unwrap();
    assert_eq!(rec.mode, STARTING_MODE);
    assert_eq!(rec.turns[0].mode, None);
    assert!(rec.turns[0].requests.is_empty());
    assert_eq!(store.default_mode().unwrap(), STARTING_MODE);

    store.set_thread_mode("t-v4", "plan", 3).unwrap();
    let turn = store.begin_turn("t-v4", None, "again", None, None, 3).unwrap();
    let answers = vec![("once".to_string(), "Allow once".to_string())];
    let request = store
        .insert_request(
            &NewRequest {
                thread_id: "t-v4",
                turn_number: turn,
                act: None,
                subagent: None,
                kind: "command",
                title: "ls",
                detail: Some("ls"),
                path: None,
                answers: &answers,
            },
            4,
        )
        .unwrap();
    assert!(store.list_threads().unwrap()[0].waiting);
    let (thread, turn_number, ended) = store
        .end_request(&request.id, "answered", Some("once"), 5)
        .unwrap()
        .unwrap();
    assert_eq!(
        (thread.as_str(), turn_number, ended.answer.as_deref()),
        ("t-v4", 2, Some("once"))
    );
    assert_eq!(
        store.end_request(&request.id, "withdrawn", None, 6).unwrap(),
        None,
        "it ends once"
    );
    let rec = store.get_thread("t-v4").unwrap().unwrap();
    assert_eq!(rec.turns[1].mode.as_deref(), Some("plan"));
    assert_eq!(rec.turns[1].requests[0].state, "answered");

    // A request the database takes only part of is not kept at all.
    store
        .conn
        .execute_batch("CREATE TRIGGER refused BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END")
        .unwrap();
    let partial = NewRequest {
        thread_id: "t-v4",
        turn_number: turn,
        act: None,
        subagent: None,
        kind: "command",
        title: "rm",
        detail: Some("rm"),
        path: None,
        answers: &answers,
    };
    assert!(store.insert_request(&partial, 7).is_err());
    store.conn.execute_batch("DROP TRIGGER refused").unwrap();
    let rec = store.get_thread("t-v4").unwrap().unwrap();
    assert_eq!(
        rec.turns[1].requests.len(),
        1,
        "no request is left that nobody can answer"
    );
    store.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn a_v6_database_takes_subagents_and_harness_turns_and_keeps_no_half_subagent() {
    let dir = std::env::temp_dir().join(format!(
        "crowbar-lever-store-v6-{}-{}",
        std::process::id(),
        crate::sys::random_hex(4)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lever.db");

    // The schema a Lever before subagents left: no subagent tables, no
    // `requests.subagent_id`, no `turns.origin`.
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '6');
                 CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT NOT NULL,
                     harness_name TEXT, harness_session_id TEXT, model TEXT, mode TEXT, effort TEXT,
                     archived INTEGER NOT NULL DEFAULT 0,
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE turns (id INTEGER PRIMARY KEY AUTOINCREMENT,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL, message TEXT NOT NULL, context TEXT,
                     answer TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, error TEXT, mode TEXT,
                     created_at INTEGER NOT NULL, ended_at INTEGER, UNIQUE(thread_id, turn_number));
                 CREATE TABLE requests (id TEXT PRIMARY KEY,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL, act_id TEXT, kind TEXT NOT NULL, title TEXT NOT NULL,
                     detail TEXT, path TEXT, answers TEXT NOT NULL, state TEXT NOT NULL, answer TEXT,
                     seq INTEGER NOT NULL);
                 INSERT INTO threads (id, title, cwd, mode, created_at, updated_at)
                 VALUES ('t-v6', 'Old thread', '/tmp', 'ask-first', 100, 100);
                 INSERT INTO turns (thread_id, turn_number, message, state, created_at, ended_at)
                 VALUES ('t-v6', 1, 'hello', 'done', 100, 101);
                 INSERT INTO requests (id, thread_id, turn_number, kind, title, answers, state, seq)
                 VALUES ('r-old', 't-v6', 1, 'command', 'ls', '[]', 'waiting', 1);",
        )
        .unwrap();
    }

    let store = Store::open(&db, 200).unwrap();
    let rec = store.get_thread("t-v6").unwrap().unwrap();
    assert_eq!(rec.turns[0].origin, None, "a turn from before is the person's");
    assert_eq!(rec.turns[0].requests[0].state, "withdrawn");
    assert_eq!(rec.turns[0].requests[0].subagent, None);

    let patch = SubagentPatch {
        thread_id: "t-v6",
        turn_number: 1,
        harness_session_id: "ses-child",
        title: Some("Look around"),
        state: Some("working"),
        error: None,
    };
    let (turn, sub) = store.upsert_subagent(&patch, 102).unwrap();
    assert_eq!((turn, sub.state.as_str()), (1, "working"));

    // A turn the harness began is its own, and only the newest is reopened.
    let woke = store.begin_harness_turn("t-v6", 103).unwrap();
    assert_eq!(woke, 2);
    assert_eq!(
        store.latest_turn("t-v6").unwrap(),
        Some((2, "working".to_string(), true))
    );
    store.finish_turn("t-v6", 2, "done", None, 104).unwrap();
    assert!(store.reopen_harness_turn("t-v6", 2, 105).unwrap());
    assert_eq!(
        store.latest_turn("t-v6").unwrap(),
        Some((2, "working".to_string(), true))
    );
    store.finish_turn("t-v6", 2, "done", None, 106).unwrap();
    store.begin_turn("t-v6", None, "next", None, None, 107).unwrap();
    assert!(
        !store.reopen_harness_turn("t-v6", 2, 108).unwrap(),
        "a turn with a newer one after it stays ended"
    );
    assert!(
        !store.reopen_harness_turn("t-v6", 1, 108).unwrap(),
        "the person's turn is not reopened"
    );
    let rec = store.get_thread("t-v6").unwrap().unwrap();
    assert_eq!(rec.turns[1].origin.as_deref(), Some("harness"));
    assert_eq!(rec.turns[1].message, "");

    // A subagent the database takes only part of is not kept at all,
    // nor is half a reopened turn.
    store
        .conn
        .execute_batch("CREATE TRIGGER refused BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END")
        .unwrap();
    let woke_again = store.begin_harness_turn("t-v6", 110);
    assert!(
        woke_again.is_err(),
        "the thread is not touched, so the turn is not begun"
    );
    store.conn.execute_batch("DROP TRIGGER refused").unwrap();
    let woke_again = store.begin_harness_turn("t-v6", 111).unwrap();
    store.finish_turn("t-v6", woke_again, "done", None, 112).unwrap();
    store
        .conn
        .execute_batch("CREATE TRIGGER refused BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END")
        .unwrap();
    assert!(store.reopen_harness_turn("t-v6", woke_again, 113).is_err());
    assert_eq!(
        store.latest_turn("t-v6").unwrap(),
        Some((woke_again, "done".to_string(), true)),
        "a reopen the database takes only part of leaves the turn ended"
    );
    let partial = SubagentPatch {
        harness_session_id: "ses-half",
        ..patch
    };
    assert!(store.upsert_subagent(&partial, 109).is_err());
    store.conn.execute_batch("DROP TRIGGER refused").unwrap();
    let rec = store.get_thread("t-v6").unwrap().unwrap();
    let subagents: usize = rec.turns.iter().map(|t| t.subagents.len()).sum();
    assert_eq!(subagents, 1, "no subagent is left working that Lever does not hold");
    store.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovered_threads_import_history_atomically_and_track_ignored_sessions() {
    use crate::harness::{ImportedAct, ImportedSubagent, ImportedTurn};

    let dir = std::env::temp_dir().join(format!(
        "crowbar-lever-store-discovered-{}-{}",
        std::process::id(),
        crate::sys::random_hex(4)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lever.db");

    let store = Store::open(&db, 100).unwrap();
    store
        .create_discovered_thread(
            "t-ext1",
            "Thread",
            "/tmp/work",
            Some("OpenCode"),
            "ses-ext-1",
            STARTING_MODE,
            90,
            95,
        )
        .unwrap();
    store.remember_ignored_session("ses-sub-1", "subagent", 96).unwrap();

    let (owned, ignored) = store.known_harness_sessions().unwrap();
    assert!(owned.contains("ses-ext-1"));
    assert!(ignored.contains("ses-sub-1"));

    let meta = store.thread_meta("t-ext1").unwrap().unwrap();
    assert!(meta.needs_history);
    assert_eq!(meta.harness_session_id.as_deref(), Some("ses-ext-1"));

    assert!(
        store
            .update_discovered_thread("t-ext1", Some("Terminal chat"), 98)
            .unwrap()
    );
    assert!(
        !store
            .update_discovered_thread("t-ext1", Some("Terminal chat"), 97)
            .unwrap()
    );

    let history = SessionHistory {
        title: None,
        turns: vec![ImportedTurn {
            message: "List the files".to_string(),
            answer: "Found Cargo.toml".to_string(),
            acts: vec![ImportedAct {
                harness_call_id: "call-1".to_string(),
                kind: "command".to_string(),
                title: "ls".to_string(),
                state: "done".to_string(),
                detail: Some("Cargo.toml".to_string()),
            }],
            notes: vec![],
            subagents: vec![ImportedSubagent {
                harness_session_id: "ses-sub-2".to_string(),
                title: "Helper".to_string(),
                state: "done".to_string(),
                error: None,
                prompt: Some("Inspect".to_string()),
            }],
            origin: None,
        }],
        model: Some("stub/stub-1".to_string()),
        effort: Some("high".to_string()),
    };
    store.import_thread_history("t-ext1", &history, None, 105).unwrap();

    let meta = store.thread_meta("t-ext1").unwrap().unwrap();
    assert!(!meta.needs_history);
    assert_eq!(meta.model.as_deref(), Some("stub/stub-1"));
    assert_eq!(meta.effort.as_deref(), Some("high"));
    // A Model the person chose before opening it outlives the import.
    store
        .create_discovered_thread(
            "t-ext2",
            "Thread",
            "/tmp",
            Some("OpenCode"),
            "ses-ext-2",
            "ask-first",
            90,
            96,
        )
        .unwrap();
    store.set_thread_model("t-ext2", "stub/chosen", 100).unwrap();
    store.import_thread_history("t-ext2", &history, None, 105).unwrap();
    let meta = store.thread_meta("t-ext2").unwrap().unwrap();
    assert_eq!(meta.model.as_deref(), Some("stub/chosen"));
    assert_eq!(meta.effort.as_deref(), Some("high"));
    assert_eq!(meta.mode, "ask-first", "the mode it was found with");

    let rec = store.get_thread("t-ext1").unwrap().unwrap();
    assert_eq!(rec.title, "Terminal chat");
    assert_eq!(rec.turns.len(), 1);
    assert_eq!(rec.turns[0].message, "List the files");
    assert_eq!(rec.turns[0].answer, "Found Cargo.toml");
    assert_eq!(rec.turns[0].acts.len(), 1);
    assert_eq!(rec.turns[0].acts[0].title, "ls");
    assert_eq!(rec.turns[0].subagents.len(), 1);
    assert_eq!(rec.turns[0].subagents[0].title, "Helper");

    let (_, ignored_after) = store.known_harness_sessions().unwrap();
    assert!(ignored_after.contains("ses-sub-2"));

    // A session found empty is remembered with its stamp, until it is a
    // thread or ignored.
    store
        .remember_quiet_session("ses-quiet", "2026-01-01T00:00:00.100Z")
        .unwrap();
    store
        .remember_quiet_session("ses-quiet-2", "2026-01-01T00:00:00.100Z")
        .unwrap();
    assert_eq!(
        store.quiet_sessions().unwrap().get("ses-quiet").map(String::as_str),
        Some("2026-01-01T00:00:00.100Z")
    );
    store
        .create_discovered_thread("t-quiet", "Thread", "/tmp", None, "ses-quiet", "ask-first", 90, 96)
        .unwrap();
    store.remember_ignored_session("ses-quiet-2", "subagent", 97).unwrap();
    assert!(store.quiet_sessions().unwrap().is_empty());
    // A thread and its quiet mark are written together or not at all.
    store
        .remember_quiet_session("ses-quiet-3", "2026-01-01T00:00:00.100Z")
        .unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER refused BEFORE DELETE ON quiet_sessions BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    assert!(
        store
            .create_discovered_thread("t-quiet-3", "Thread", "/tmp", None, "ses-quiet-3", "ask-first", 90, 96)
            .is_err()
    );
    assert!(
        store.thread_meta("t-quiet-3").unwrap().is_none(),
        "half a write was kept"
    );
    assert!(store.quiet_sessions().unwrap().contains_key("ses-quiet-3"));
    store.conn.execute_batch("DROP TRIGGER refused").unwrap();

    // A session a new one replaces is never taken for one started by hand.
    store.set_harness_session_id("t-ext1", "ses-ext-1b").unwrap();
    let (owned, ignored) = store.known_harness_sessions().unwrap();
    assert!(owned.contains("ses-ext-1b"));
    assert!(ignored.contains("ses-ext-1"));

    // Deleting remembers the thread's session and its subagents', whether
    // or not the harness deleted its own copy.
    store
        .conn
        .execute(
            "DELETE FROM ignored_sessions WHERE harness_session_id = 'ses-sub-2'",
            [],
        )
        .unwrap();
    store.delete_thread("t-ext1", 110).unwrap();
    let (owned, ignored) = store.known_harness_sessions().unwrap();
    assert!(!owned.contains("ses-ext-1b"));
    assert!(ignored.contains("ses-ext-1b"));
    assert!(ignored.contains("ses-sub-2"));
    assert!(store.thread_meta("t-ext1").unwrap().is_none());

    store.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn retired_sessions_respect_deletion_and_reassignment() {
    let dir = std::env::temp_dir().join(format!("lever-store-retired-{}", crate::sys::random_hex(8)));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("lever.db"), 10).unwrap();

    let new_thread = NewThread {
        title: "Move me",
        cwd: "/home/p",
        harness_name: Some("stub"),
        mode: "ask-first",
    };
    store.create_thread("t-move", &new_thread, 10).unwrap();
    store.set_harness_session_id("t-move", "ses-old-1").unwrap();
    store
        .move_thread_to_project("t-move", "/home/p/proj-a", true, Some("ses-old-1"), 20)
        .unwrap();
    assert_eq!(
        store.retired_sessions_for_thread("t-move").unwrap(),
        vec!["ses-old-1".to_string()]
    );

    // Moving again retires the second session too.
    store.set_harness_session_id("t-move", "ses-old-2").unwrap();
    store
        .move_thread_to_project("t-move", "/home/p/proj-b", true, Some("ses-old-2"), 30)
        .unwrap();
    assert_eq!(
        store.retired_sessions_for_thread("t-move").unwrap(),
        vec!["ses-old-1".to_string(), "ses-old-2".to_string()]
    );

    // Marking a retired session deleted clears its thread association so a
    // retry after partial failure will not ask the harness to delete it again.
    store.mark_session_deleted("t-move", "ses-old-1", 40).unwrap();
    assert_eq!(
        store.retired_sessions_for_thread("t-move").unwrap(),
        vec!["ses-old-2".to_string()]
    );

    // Reassigning a retired session ID to another thread invalidates its
    // retirement record so deleting `t-move` never deletes `t-other`'s live session.
    let other_thread = NewThread {
        title: "Other",
        cwd: "/home/p",
        harness_name: Some("stub"),
        mode: "ask-first",
    };
    store.create_thread("t-other", &other_thread, 50).unwrap();
    store.set_harness_session_id("t-other", "ses-old-2").unwrap();
    assert!(store.retired_sessions_for_thread("t-move").unwrap().is_empty());

    // Marking a live current session deleted clears threads.harness_session_id
    // and keeps it ignored as deleted.
    store.mark_session_deleted("t-other", "ses-old-2", 60).unwrap();
    assert_eq!(store.thread_meta("t-other").unwrap().unwrap().harness_session_id, None);
    let (owned, ignored) = store.known_harness_sessions().unwrap();
    assert!(!owned.contains("ses-old-2"));
    assert!(ignored.contains("ses-old-2"));

    store.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
