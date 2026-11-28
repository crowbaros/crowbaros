//! Unit tests for ACP formatting, dialect isolation, history replay, and session routing.

use super::dialect::{AcpDialect, GenericAcpDialect, OpenCodeAcpDialect, dialect_for_config};
use super::format::{
    extract_effort, extract_model_catalog, extract_modes, harness_commands, line_diff, redact_ids, request_detail,
    tool_detail, value_to_detail,
};
use super::history::HistoryBuilder;
use super::*;
use crate::mcp;
use crate::opencode::{ASK_FIRST_AGENT, build_harness_env, opencode_config_content};
use agent_client_protocol_schema::v1::{AvailableCommandsUpdate, SessionConfigOption, ToolCallUpdateFields, ToolKind};
use serde_json::{Value, json};
use std::path::Path;

const MAX_DETAIL_LINES: usize = 400;

#[test]
fn an_id_is_redacted_as_a_whole_word_and_the_longest_first() {
    assert_eq!(
        redact_ids("failed on call-10 after call-1", vec!["call-1", "call-10"]),
        "failed on [redacted] after [redacted]"
    );
    assert_eq!(redact_ids("vec1 failed in c1", vec!["c1"]), "vec1 failed in [redacted]");
    assert_eq!(
        redact_ids("failed after 10 seconds", vec!["1"]),
        "failed after 10 seconds"
    );
    assert_eq!(
        redact_ids("session s-9: s-9.", vec!["s-9", ""]),
        "session [redacted]: [redacted]."
    );
    assert_eq!(redact_ids("c1c1 and ac1c1", vec!["c1"]), "c1c1 and ac1c1");
    assert_eq!(redact_ids("c1 c1", vec!["c1"]), "[redacted] [redacted]");
}

#[test]
fn a_raw_value_gives_an_act_only_its_text_command_or_path() {
    assert_eq!(value_to_detail(&json!("plain")), Some("plain".to_string()));
    assert_eq!(
        value_to_detail(&json!({ "command": "ls -l", "id": "x" })),
        Some("ls -l".to_string())
    );
    assert_eq!(
        value_to_detail(&json!({ "path": "/tmp/a" })),
        Some("/tmp/a".to_string())
    );
    assert_eq!(value_to_detail(&json!({ "sessionId": "s-1", "exit": 0 })), None);
    assert_eq!(value_to_detail(&json!([{ "toolCallId": "c-1" }])), None);
    assert_eq!(value_to_detail(&json!(7)), None);
    assert_eq!(value_to_detail(&Value::Null), None);
}

#[test]
fn an_act_with_no_text_says_what_its_output_or_else_its_input_names() {
    let (input, output) = (json!({ "command": "cargo test" }), json!({ "exit": 0 }));
    assert_eq!(
        tool_detail(&[], Some(&output), Some(&input)),
        Some("cargo test".to_string())
    );
    assert_eq!(
        tool_detail(&[], Some(&json!("3 passed")), Some(&input)),
        Some("3 passed".to_string())
    );
    assert_eq!(tool_detail(&[], Some(&output), None), None);
}

#[test]
fn opencode_config_content_disables_self_update_adds_ask_first_and_preserves_other_keys() {
    let ask_first = json!({
        "mode": "primary",
        "description": "Asks before it runs a command or edits a file.",
        "permission": {
            "bash": { "*": "ask" },
            "edit": { "*": "ask" },
            "crowbar_*": { "*": "ask" },
            "crowbar_make_project": { "*": "ask" }
        },
    });
    assert_eq!(
        serde_json::from_str::<Value>(&opencode_config_content(None, &[])).unwrap(),
        json!({ "update": "disable", "default_agent": ASK_FIRST_AGENT, "agent": { ASK_FIRST_AGENT: ask_first } })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&opencode_config_content(
            Some(r#"{"model":"stub/stub-1","update":"auto","agent":{"mine":{"mode":"primary"}}}"#),
            &[]
        ))
        .unwrap(),
        json!({
            "model": "stub/stub-1",
            "update": "disable",
            "default_agent": ASK_FIRST_AGENT,
            "agent": { "mine": { "mode": "primary" }, ASK_FIRST_AGENT: ask_first },
        })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&opencode_config_content(Some("not-json"), &[])).unwrap()["update"],
        json!("disable")
    );
}

#[test]
fn harness_env_toggles_free_hosted_models_without_losing_extra_env() {
    let (default_off, _) = build_harness_env(&[], None, &[], false);
    assert!(default_off.contains(&("OPENCODE_DISABLE_FREE_MODELS".into(), "1".into())));
    assert!(default_off.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into())));

    let (default_on, rm_default_on) = build_harness_env(&[], None, &[], true);
    assert!(!default_on.iter().any(|(k, _)| k == "OPENCODE_DISABLE_FREE_MODELS"));
    assert!(!default_on.iter().any(|(k, _)| k == "OPENCODE_DISABLE_MODELS_FETCH"));
    assert_eq!(rm_default_on, vec!["OPENCODE_DISABLE_FREE_MODELS"]);

    let override_zero = vec![("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "0".to_string())];
    let (forced_off, _) = build_harness_env(&override_zero, None, &[], false);
    assert!(forced_off.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into())));
    assert!(!forced_off.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "0".into())));

    let extra = vec![("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string())];
    let (set_off, rm_off) = build_harness_env(&extra, None, &[], false);
    assert_eq!(
        set_off
            .iter()
            .filter(|(k, _)| k == "OPENCODE_DISABLE_MODELS_FETCH")
            .count(),
        1
    );
    assert!(set_off.contains(&("OPENCODE_DISABLE_FREE_MODELS".into(), "1".into())));
    assert!(
        set_off
            .iter()
            .any(|(k, v)| k == "OPENCODE_CONFIG_CONTENT" && v.contains("disable"))
    );
    assert!(rm_off.is_empty());

    let (set_on, rm_on) = build_harness_env(&extra, None, &[], true);
    assert!(set_on.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into())));
    assert!(!set_on.iter().any(|(k, _)| k == "OPENCODE_DISABLE_FREE_MODELS"));
    assert_eq!(rm_on, vec!["OPENCODE_DISABLE_FREE_MODELS"]);
}

#[test]
fn generic_acp_dialect_injects_no_opencode_env_or_extensions() {
    let cfg = HarnessConfig {
        id: "gemini".into(),
        name: "Gemini".into(),
        program: "/usr/bin/gemini".into(),
        args: vec!["--acp".into()],
        env: vec![("CUSTOM".into(), "1".into())],
        watch_paths: Vec::new(),
    };
    let dialect = dialect_for_config(&cfg);
    let (env_set, env_remove) = dialect.prepare_spawn_env(&cfg, HarnessLaunchOptions::default());
    assert_eq!(env_set, vec![("CUSTOM".into(), "1".into())]);
    assert!(env_remove.is_empty());
    assert!(dialect.initialize_client_meta().is_none());
    assert!(dialect.steering_method().is_none());
    assert!(dialect.child_update_notification_method().is_none());
    assert!(!dialect.holds_prompt_during_subagent_request());
    let spec = McpServerSpec::crowbar(PathBuf::from("/bin/lever"), Path::new("/tmp/t.sock"), "a", "s");
    assert!(!dialect.enrich_mcp_server(&spec).env.iter().any(|(k, _)| k == mcp::ENV_MCP_META_SESSION_KEY));

    let oc = OpenCodeAcpDialect;
    assert!(oc.holds_prompt_during_subagent_request());
    assert!(oc
        .enrich_mcp_server(&spec)
        .env
        .contains(&(mcp::ENV_MCP_META_SESSION_KEY.into(), "ai.opencode/sessionID".into())));
}

#[test]
fn no_models_error_is_recognized_by_error_name_or_message() {
    let oc = OpenCodeAcpDialect;
    assert!(oc.is_no_models_error(
        &json!({ "code": -32603, "message": "Internal error", "data": { "errorName": "CatalogNotReady" } }),
        "Internal error"
    ));
    assert!(oc.is_no_models_error(
        &json!({ "code": -32603, "message": "Internal error: No models are available" }),
        "Internal error: No models are available"
    ));
    assert!(!oc.is_no_models_error(
        &json!({ "code": -32603, "message": "Timeout" }),
        "Timeout"
    ));

    let generic = GenericAcpDialect;
    assert!(!generic.is_no_models_error(
        &json!({ "code": -32603, "message": "Internal error", "data": { "errorName": "CatalogNotReady" } }),
        "Internal error"
    ));
}

#[test]
fn a_change_shows_as_the_lines_it_removes_and_adds_with_a_few_around_it() {
    assert_eq!(
        line_diff(Some("alpha line\n"), "beta line\n"),
        "-alpha line\n+beta line"
    );
    assert_eq!(line_diff(None, "hello\nworld\n"), "+hello\n+world");
    let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let new = "1\n2\n3\n4\nfive\n6\n7\n8\n9\n";
    assert_eq!(line_diff(Some(old), new), " 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8");
    let long: String = (0..1000).map(|n| format!("{n}\n")).collect();
    let shown = line_diff(None, &long);
    assert_eq!(shown.lines().count(), MAX_DETAIL_LINES + 1);
    assert!(shown.ends_with("… 600 more lines put in"));

    let old: String = (0..450).map(|n| format!("old {n}\n")).collect();
    let new: String = (0..450).map(|n| format!("new {n}\n")).collect();
    let shown = line_diff(Some(&old), &new);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), MAX_DETAIL_LINES + 2);
    assert_eq!(lines[0], "-old 0");
    assert!(lines.contains(&"… 250 more lines taken out"), "{shown}");
    assert!(lines.contains(&"+new 0"), "{shown}");
    assert_eq!(lines.last(), Some(&"… 250 more lines put in"));
}

#[test]
fn a_models_provider_is_its_group_or_what_its_id_says() {
    let oc = OpenCodeAcpDialect;
    let ungrouped: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "model", "name": "Model", "category": "model", "type": "select",
          "currentValue": "opencode/big-pickle",
          "options": [
              { "value": "opencode/big-pickle", "name": "opencode/Big Pickle" },
              { "value": "openrouter/meta/llama", "name": "openrouter/Llama" },
              { "value": "acme/one", "name": "One of Acme's" },
              { "value": "plain", "name": "Plain" }
          ] }
    ]))
    .unwrap();
    let catalog = extract_model_catalog(&ungrouped, &oc).unwrap();
    let model = |id: &str, name: &str, provider: Option<(&str, &str)>| ModelOption {
        id: id.into(),
        name: name.into(),
        provider: provider.map(|(id, name)| (id.into(), name.into())),
    };
    assert_eq!(
        catalog.models,
        [
            model("opencode/big-pickle", "Big Pickle", Some(("opencode", "OpenCode"))),
            model("openrouter/meta/llama", "Llama", Some(("openrouter", "OpenRouter"))),
            model("acme/one", "One of Acme's", Some(("acme", "acme"))),
            model("plain", "Plain", None),
        ]
    );
    assert_eq!(catalog.default_model.as_deref(), Some("opencode/big-pickle"));

    let grouped: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "model", "name": "Model", "category": "model", "type": "select",
          "currentValue": "m-1",
          "options": [
              { "group": "acme", "name": "Acme AI", "options": [{ "value": "m-1", "name": "One" }] },
              { "group": "other", "name": "Other", "options": [{ "value": "o/2", "name": "Two" }] }
          ] }
    ]))
    .unwrap();
    let catalog = extract_model_catalog(&grouped, &oc).unwrap();
    assert_eq!(
        catalog.models,
        [
            ModelOption {
                id: "m-1".into(),
                name: "One".into(),
                provider: Some(("acme".into(), "Acme AI".into())),
            },
            ModelOption {
                id: "o/2".into(),
                name: "Two".into(),
                provider: Some(("other".into(), "Other".into())),
            },
        ]
    );
}

#[test]
fn effort_is_the_thought_level_option_and_a_selector_with_no_levels_is_none() {
    let options: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "model", "name": "Model", "category": "model", "type": "select",
          "currentValue": "a/b", "options": [{ "value": "a/b", "name": "a/B" }] },
        { "id": "effort", "name": "Effort", "category": "thought_level", "type": "select",
          "currentValue": "default",
          "options": [{ "value": "high", "name": "High" }, { "value": "default", "name": "Default" }] }
    ]))
    .unwrap();
    let selector = extract_effort(&options).unwrap();
    assert_eq!(selector.current, "default");
    assert!(selector.offers("high") && !selector.offers("max"));
    assert_eq!(
        selector.options,
        [
            EffortOption {
                id: "high".into(),
                name: "High".into()
            },
            EffortOption {
                id: "default".into(),
                name: "Default".into()
            },
        ]
    );
    let unmarked: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "effort", "name": "Effort", "type": "select", "currentValue": "x",
          "options": [{ "value": "x", "name": "X" }] },
        { "id": "level", "name": "Level", "category": "thought_level", "type": "select",
          "currentValue": "", "options": [] }
    ]))
    .unwrap();
    assert_eq!(extract_effort(&unmarked), None);
}

#[test]
fn a_command_keeps_its_name_what_the_harness_says_of_it_and_its_hint() {
    let update: AvailableCommandsUpdate = serde_json::from_value(json!({
        "availableCommands": [
            { "name": "review", "description": "review changes", "input": { "hint": "commit, branch, or pr" } },
            { "name": "compact", "description": "Compact the session" },
            { "name": " ", "description": "a command with no name" },
            { "name": "init", "description": "", "input": { "hint": " " } }
        ]
    }))
    .unwrap();
    assert_eq!(
        harness_commands(update.available_commands),
        [
            HarnessCommand {
                name: "review".into(),
                description: "review changes".into(),
                hint: Some("commit, branch, or pr".into()),
            },
            HarnessCommand {
                name: "compact".into(),
                description: "Compact the session".into(),
                hint: None,
            },
            HarnessCommand {
                name: "init".into(),
                description: String::new(),
                hint: None,
            },
        ]
    );
}

#[test]
fn modes_are_lever_s_own_and_only_those_the_harness_reports() {
    let oc = OpenCodeAcpDialect;
    let options: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "m",
          "options": [{ "value": "m", "name": "M" }] },
        { "id": "mode", "name": "Session Mode", "category": "mode", "type": "select",
          "currentValue": "crowbar-ask-first",
          "options": [
            { "value": "crowbar-ask-first", "name": "crowbar-ask-first" },
            { "value": "build", "name": "Build" },
            { "value": "plan", "name": "Plan" },
            { "value": "persons-own", "name": "Theirs" }
          ] }
    ]))
    .unwrap();
    let selector = extract_modes(&options, &oc).unwrap();
    assert_eq!(selector.config_id, "mode");
    assert_eq!(selector.modes, vec!["ask-first", "full-access", "plan"]);
    assert_eq!(selector.current.as_deref(), Some("ask-first"));

    let build_only: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "persons-own",
          "options": [{ "value": "build", "name": "Build" }, { "value": "persons-own", "name": "Theirs" }] }
    ]))
    .unwrap();
    let selector = extract_modes(&build_only, &oc).unwrap();
    assert_eq!(selector.modes, vec!["full-access"]);
    assert_eq!(selector.current, None);

    let others: Vec<SessionConfigOption> = serde_json::from_value(json!([
        { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "default",
          "options": [{ "value": "default", "name": "Default" }, { "value": "yolo", "name": "Yolo" }] }
    ]))
    .unwrap();
    assert_eq!(extract_modes(&others, &oc), None);
}

#[test]
fn a_request_shows_the_exact_command_or_the_file_and_the_change() {
    let command: ToolCallUpdateFields = serde_json::from_value(json!({
        "kind": "execute", "title": "echo hi",
        "rawInput": { "command": "echo hi && id -un", "cwd": "/tmp" }
    }))
    .unwrap();
    let home = Path::new("/home/person");
    assert_eq!(
        request_detail(Some(ToolKind::Execute), &command, home),
        (Some("echo hi && id -un".to_string()), None)
    );
    let edit: ToolCallUpdateFields = serde_json::from_value(json!({
        "kind": "edit", "title": "/w/seed.txt",
        "rawInput": { "path": "/w/seed.txt", "oldString": "alpha", "newString": "beta" },
        "content": [{ "type": "diff", "path": "/w/seed.txt", "oldText": "alpha line\n", "newText": "beta line\n" }]
    }))
    .unwrap();
    assert_eq!(
        request_detail(Some(ToolKind::Edit), &edit, home),
        (
            Some("-alpha line\n+beta line".to_string()),
            Some("/w/seed.txt".to_string())
        )
    );
    let raw_edit: ToolCallUpdateFields = serde_json::from_value(json!({
        "kind": "edit", "rawInput": { "filePath": "/w/new.txt", "content": "hello\n" }
    }))
    .unwrap();
    assert_eq!(
        request_detail(Some(ToolKind::Edit), &raw_edit, home),
        (Some("+hello".to_string()), Some("/w/new.txt".to_string()))
    );
    let deletion: ToolCallUpdateFields = serde_json::from_value(json!({
        "kind": "edit", "rawInput": { "filePath": "/w/old.txt", "oldString": "remove me\n", "newString": "" }
    }))
    .unwrap();
    assert_eq!(
        request_detail(Some(ToolKind::Edit), &deletion, home),
        (Some("-remove me".to_string()), Some("/w/old.txt".to_string()))
    );
}

#[test]
fn transport_retry_failure_triggers_only_for_repeated_transport_errors() {
    let oc = OpenCodeAcpDialect;
    let early = json!({
        "sessionUpdate": "session_info_update",
        "_meta": {
            "opencode/retry": {
                "attempt": 2,
                "error": { "type": "provider.transport", "message": "ECONNREFUSED: Unable to connect." }
            }
        }
    });
    assert_eq!(oc.extract_transport_retry_failure(&early), None);

    let rate_limit = json!({
        "sessionUpdate": "session_info_update",
        "_meta": {
            "opencode/retry": {
                "attempt": 4,
                "error": { "type": "provider.rate_limit", "message": "429 Too Many Requests" }
            }
        }
    });
    assert_eq!(oc.extract_transport_retry_failure(&rate_limit), None);

    let unreachable = json!({
        "sessionUpdate": "session_info_update",
        "_meta": {
            "opencode/retry": {
                "attempt": 3,
                "error": { "type": "provider.transport", "message": "ECONNREFUSED: Unable to connect." }
            }
        }
    });
    assert_eq!(
        oc.extract_transport_retry_failure(&unreachable).as_deref(),
        Some("ECONNREFUSED: Unable to connect.")
    );
}

#[test]
fn child_title_prefix_and_opencode_subagent_metadata_are_normalized() {
    let oc = OpenCodeAcpDialect;
    assert_eq!(
        oc.strip_child_title_prefix("Inspect files: Read .env", Some("Inspect files")),
        "Read .env"
    );
    assert_eq!(
        oc.strip_child_title_prefix("Read .env", Some("Inspect files")),
        "Read .env"
    );
    assert_eq!(
        oc.strip_child_title_prefix("Inspect files: ", Some("Inspect files")),
        "Inspect files: "
    );

    let meta_val = json!({
        "_meta": {
            "opencode/child-session": {
                "id": "ses_child_1",
                "parentID": "ses_root_1",
                "depth": 1,
                "title": "Inspect files"
            }
        }
    });
    assert_eq!(
        oc.extract_child_meta(&meta_val),
        Some(("ses_child_1".to_string(), Some("Inspect files".to_string())))
    );

    let input = json!({
        "description": "Inspect files",
        "prompt": "Read .env and report",
        "agent": "general"
    });
    let output = json!({
        "metadata": {
            "sessionID": "ses_child_1",
            "status": "running"
        }
    });
    assert!(oc.is_subagent_tool("subagent", ToolKind::Think, Some(&input), &json!({}), false));
    assert!(oc.is_subagent_tool("task", ToolKind::Think, None, &json!({}), true));
    assert!(!oc.is_subagent_tool("Task", ToolKind::Think, None, &json!({}), false));
    assert!(!oc.is_subagent_tool(
        "task",
        ToolKind::Think,
        Some(&input),
        &json!({ "_meta": { "toolName": "task" } }),
        false
    ));
    assert_eq!(
        oc.extract_subagent_call_fields(Some(&input), Some(&output)),
        (
            Some("Inspect files".to_string()),
            Some("Read .env and report".to_string()),
            Some("ses_child_1".to_string()),
        )
    );
    assert_eq!(
        oc.extract_child_error(Some(&json!({ "message": "boom" }))).as_deref(),
        Some("boom")
    );
}

#[test]
fn history_builder_keeps_consecutive_user_messages_as_their_own_turns() {
    let oc = OpenCodeAcpDialect;
    let mut b = HistoryBuilder::new(true);
    for (id, text) in [("u1", "First "), ("u1", "part"), ("u2", "Second")] {
        b.push_update(
            &json!({
                "sessionUpdate": "user_message_chunk",
                "messageId": id,
                "content": { "type": "text", "text": text }
            }),
            &oc,
        );
    }
    b.push_update(
        &json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "a1",
            "content": { "type": "text", "text": "Answer" }
        }),
        &oc,
    );
    let (history, _, _) = b.finish(None, None);
    assert_eq!(history.turns.len(), 2);
    assert_eq!(history.turns[0].message, "First part");
    assert_eq!(history.turns[0].answer, "");
    assert_eq!(history.turns[1].message, "Second");
    assert_eq!(history.turns[1].answer, "Answer");
}

#[test]
fn history_builder_reconstructs_multi_turn_replay_with_acts_and_subagents() {
    let oc = OpenCodeAcpDialect;
    let mut b = HistoryBuilder::new(true);
    b.push_update(
        &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "Hello " } }),
        &oc,
    );
    b.push_update(
        &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "world" } }),
        &oc,
    );
    b.push_update(
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "echo hi",
            "kind": "execute",
            "status": "pending",
            "rawInput": { "command": "echo hi" }
        }),
        &oc,
    );
    b.push_update(
        &json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "hi\n" } }]
        }),
        &oc,
    );
    b.push_update(
        &json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": { "type": "text", "text": "Done: hi" } }),
        &oc,
    );
    b.push_update(
        &json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "Spawn helper" } }),
        &oc,
    );
    b.push_update(
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_sub",
            "name": "subagent",
            "title": "subagent",
            "kind": "think",
            "status": "pending",
            "rawInput": { "description": "Child helper", "prompt": "Check logs" }
        }),
        &oc,
    );
    b.push_update(
        &json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_sub",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "Working (sessionID: ses_child_1)" } }]
        }),
        &oc,
    );
    b.push_update(
        &json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m2", "content": { "type": "text", "text": "Helper finished." } }),
        &oc,
    );
    b.push_update(
        &json!({ "sessionUpdate": "session_info_update", "title": "Terminal session" }),
        &oc,
    );

    let (history, tool_ids, sub_ids) = b.finish(Some("stub/stub-1".into()), Some("default".into()));
    assert_eq!(history.title.as_deref(), Some("Terminal session"));
    assert_eq!(history.model.as_deref(), Some("stub/stub-1"));
    assert_eq!(history.turns.len(), 2);
    assert_eq!(history.turns[0].message, "Hello world");
    assert_eq!(history.turns[0].answer, "Done: hi");
    assert_eq!(history.turns[0].acts.len(), 1);
    assert_eq!(history.turns[0].acts[0].title, "echo hi");
    assert_eq!(history.turns[0].acts[0].state, "done");
    assert_eq!(history.turns[0].acts[0].detail.as_deref(), Some("hi\n"));
    assert_eq!(history.turns[1].message, "Spawn helper");
    assert_eq!(history.turns[1].answer, "Helper finished.");
    assert!(history.turns[1].acts.is_empty(), "subagent launcher is not a raw act");
    assert_eq!(history.turns[1].subagents.len(), 1);
    assert_eq!(history.turns[1].subagents[0].harness_session_id, "ses_child_1");
    assert_eq!(history.turns[1].subagents[0].title, "Child helper");
    assert_eq!(history.turns[1].subagents[0].prompt.as_deref(), Some("Check logs"));
    assert!(tool_ids.contains(&"call_1".to_string()));
    assert_eq!(sub_ids, vec!["ses_child_1".to_string()]);
}

#[test]
fn late_update_for_known_child_does_not_stamp_retired_turn() {
    let cfg = HarnessConfig {
        id: "cat-test".into(),
        name: "cat-test".into(),
        program: "/bin/cat".into(),
        args: Vec::new(),
        env: Vec::new(),
        watch_paths: Vec::new(),
    };
    let mut harness =
        AcpHarness::spawn_with_dialect(&cfg, HarnessLaunchOptions::default(), Box::new(OpenCodeAcpDialect)).unwrap();
    assert!(harness.bind_session("t1", "ses-root-1"));

    let mut events = Vec::new();
    harness.handle_child_update_notification(
        Some(&json!({
            "childSessionId": "ses-child-t1",
            "rootSessionId": "ses-root-1",
            "parentSessionId": "ses-root-1",
            "title": "Earlier child",
            "type": "status",
            "status": "completed"
        })),
        &mut events,
    );
    assert_eq!(harness.retired_turn_of("ses-child-t1"), None);

    harness.detach_session("t1", Some(2));

    harness.handle_child_update_notification(
        Some(&json!({
            "childSessionId": "ses-child-t1",
            "rootSessionId": "ses-root-1",
            "parentSessionId": "ses-root-1",
            "title": "Earlier child",
            "type": "status",
            "status": "completed"
        })),
        &mut events,
    );
    assert_eq!(harness.retired_turn_of("ses-child-t1"), None);

    harness.handle_child_update_notification(
        Some(&json!({
            "childSessionId": "ses-child-t2",
            "rootSessionId": "ses-root-1",
            "parentSessionId": "ses-root-1",
            "title": "Delayed child",
            "type": "status",
            "status": "running"
        })),
        &mut events,
    );
    assert_eq!(harness.retired_turn_of("ses-child-t2"), Some(2));

    Box::new(harness).kill_tree();
}
