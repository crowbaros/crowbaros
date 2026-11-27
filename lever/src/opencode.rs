//! What Lever reads of the person's own OpenCode settings: their rules for
//! commands and edits, so the Ask first agent Lever adds keeps them.
//!
//! OpenCode gives an agent's own rules after every rule in the person's
//! settings, and the last rule that matches wins. Ask first's own rule asks
//! before every command and every edit, so it would outrank a rule of the
//! person's that refuses one. Ask first carries the person's rules again,
//! after its own, so theirs still decide what they name.
//!
//! The settings read are those OpenCode reads for every directory, in its
//! order: `opencode.json` and `opencode.jsonc` in its config directory, the
//! file `OPENCODE_CONFIG` names, then `OPENCODE_CONFIG_CONTENT`. A project's
//! own settings, which OpenCode reads for a session in that project, are not.

use crate::acp::{self, HarnessConfig};
use crate::discovery::{
    AcpSessionDiscoverer, ExternalSession, SessionDiscoverer, spec_has_data, watch_specs_from_paths,
};
use crate::sys;
use crate::watcher::WatchSpec;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

/// What a rule does.
const ASK: &str = "ask";
const ALLOW: &str = "allow";

/// The settings documents OpenCode reads for every directory, in its order,
/// with the environment the harness is given (`env`, where a key is set
/// there) over Lever's own. `content` is `OPENCODE_CONFIG_CONTENT` as the
/// harness would have it before Lever adds to it.
pub fn person_settings(env: &[(String, String)], content: Option<&str>) -> Vec<Value> {
    let var = |key: &str| {
        env.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(key).ok())
            .filter(|v| !v.is_empty())
    };
    let config_dir = var("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| var("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("opencode")))
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".config/opencode")));
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(dir) = config_dir {
        files.push(dir.join("opencode.json"));
        files.push(dir.join("opencode.jsonc"));
    }
    files.extend(var("OPENCODE_CONFIG").map(PathBuf::from));
    let mut documents: Vec<Value> = files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| parse_jsonc(&text))
        .collect();
    documents.extend(content.and_then(parse_jsonc));
    documents
}

/// Ask first's rules for one of OpenCode's permissions: ask before all,
/// then the person's own rules for it, in their order. `action` is its name
/// as OpenCode has it now (`shell`, `edit`), and a rule whose action is a
/// pattern that matches it (`*`, `sh*`) is one for it too. Settings give
/// rules as `permission` (a rule for everything, or a rule or rules by
/// pattern for each action by its name, older names renamed as OpenCode
/// renames them) or as `permissions` (a list of action, resource, and
/// effect). A rule of the person's that allows
/// everything is the one Ask first takes the place of, and asks.
fn rules_for(documents: &[Value], action: &str) -> Value {
    // An action is matched as OpenCode matches it: `*` is any run of
    // characters and `?` any one.
    let named = |name: &str| matches_wildcard(action, name);
    let mut rules: Vec<(String, String)> = Vec::new();
    for document in documents {
        match document.get("permission") {
            Some(Value::String(all)) => rules.push(("*".to_string(), all.clone())),
            Some(Value::Object(by_name)) => {
                for (_, rule) in by_name.iter().filter(|(name, _)| named(legacy_action(name))) {
                    match rule {
                        Value::String(effect) => rules.push(("*".to_string(), effect.clone())),
                        Value::Object(patterns) => rules.extend(
                            patterns
                                .iter()
                                .filter_map(|(pattern, effect)| Some((pattern.clone(), effect.as_str()?.to_string()))),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        for rule in document
            .get("permissions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |key: &str| rule.get(key).and_then(Value::as_str);
            if let (Some(action), Some(effect)) = (field("action"), field("effect"))
                && named(action)
            {
                rules.push((field("resource").unwrap_or("*").to_string(), effect.to_string()));
            }
        }
    }
    // A pattern given again is where it was given last: that is the one
    // that counts.
    let mut last: Vec<(String, String)> = Vec::new();
    for (pattern, effect) in rules {
        last.retain(|(p, _)| *p != pattern);
        last.push((pattern, effect));
    }
    let mut out = Map::new();
    if !last.iter().any(|(pattern, _)| pattern == "*") {
        out.insert("*".to_string(), Value::from(ASK));
    }
    for (pattern, effect) in last {
        let effect = if pattern == "*" && effect == ALLOW {
            ASK.to_string()
        } else {
            effect
        };
        out.insert(pattern, Value::from(effect));
    }
    Value::Object(out)
}

/// The action a key of `permission` names, as OpenCode 2.4.1 renames its
/// older names before it matches: only these names, and only whole.
fn legacy_action(key: &str) -> &str {
    match key {
        "bash" => "shell",
        "write" | "patch" | "apply_patch" => "edit",
        other => other,
    }
}

/// Whether `input` matches `pattern`, in which `*` stands for any run of
/// characters and `?` for any one character, and nothing else is special.
fn matches_wildcard(input: &str, pattern: &str) -> bool {
    let (text, pat): (Vec<char>, Vec<char>) = (input.chars().collect(), pattern.chars().collect());
    // The last `*` seen, and where in the input it began to match.
    let (mut t, mut p, mut star, mut from) = (0, 0, None, 0);
    while t < text.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            t += 1;
            p += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            from = t;
            p += 1;
        } else if let Some(at) = star {
            p = at + 1;
            from += 1;
            t = from;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|c| *c == '*')
}

/// The Ask first agent's `permission`: it asks before a command or an edit,
/// except where the person's own rules say otherwise.
pub fn ask_first_permission(documents: &[Value]) -> Value {
    let mut permission = Map::new();
    permission.insert("bash".to_string(), rules_for(documents, "shell"));
    permission.insert("edit".to_string(), rules_for(documents, "edit"));
    Value::Object(permission)
}

/// The person's name for a provider OpenCode has Models from, which it gives
/// only an ID: the provider's own spelling for those known here, and the ID
/// as it is for any other.
pub fn provider_name(id: &str) -> String {
    const NAMES: [(&str, &str); 12] = [
        ("opencode", "OpenCode"),
        ("anthropic", "Anthropic"),
        ("openai", "OpenAI"),
        ("google", "Google"),
        ("github-copilot", "GitHub Copilot"),
        ("openrouter", "OpenRouter"),
        ("amazon-bedrock", "Amazon Bedrock"),
        ("azure", "Azure"),
        ("xai", "xAI"),
        ("ollama", "Ollama"),
        ("lmstudio", "LM Studio"),
        ("mistral", "Mistral"),
    ];
    NAMES
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Whether `config` invokes OpenCode.
pub fn is_opencode_config(config: &HarnessConfig) -> bool {
    let program_name = Path::new(&config.program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    program_name.eq_ignore_ascii_case("opencode") || config.name.eq_ignore_ascii_case("opencode")
}

/// OpenCode's data directory (`$XDG_DATA_HOME/opencode`, or
/// `$HOME/.local/share/opencode`), respecting `env` overrides over Lever's own.
pub fn data_dir(env: &[(String, String)]) -> Option<PathBuf> {
    let var = |key: &str| {
        env.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(key).ok())
            .filter(|v| !v.is_empty())
    };
    var("XDG_DATA_HOME")
        .map(|x| PathBuf::from(x).join("opencode"))
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".local/share/opencode")))
}

/// OpenCode's [`SessionDiscoverer`]: watches OpenCode's own store directory
/// (`opencode.db` and its WAL/journal sidecars), lists sessions across all
/// folders via short-lived ACP `session/list`, and classifies each brand-new
/// unseen session with OpenCode's own `opencode session export` so subagents
/// and empty sessions are never listed as threads. OpenCode is never kept
/// running for this.
///
/// A session becomes a thread only on positive evidence that it is a
/// top-level session with a message: its export has messages and no parent. A
/// session whose export fails is left for a later pass, and only an export
/// that names a parent marks it a subagent for good.
pub struct OpenCodeSessionDiscoverer {
    config: HarnessConfig,
    acp: AcpSessionDiscoverer,
    watch_specs: Vec<WatchSpec>,
    cli_timeout: Duration,
    /// Sessions an export showed empty or archived, by the `updated` stamp the
    /// list gave then: they are not exported again until that stamp changes.
    not_threads: Mutex<HashMap<String, String>>,
}

const OPENCODE_STORE_FILES: [&str; 4] = [
    "opencode.db",
    "opencode.db-wal",
    "opencode.db-journal",
    "opencode.db-shm",
];
/// How long one `opencode session` command may run before it is ended.
const CLI_TIMEOUT: Duration = Duration::from_secs(15);
/// The most a `session list` or `session export` may print.
const CLI_MAX_STDOUT: usize = 64 * 1024 * 1024;

impl OpenCodeSessionDiscoverer {
    pub fn new(config: HarnessConfig) -> Self {
        Self::with_cli_timeout(config, CLI_TIMEOUT)
    }

    fn with_cli_timeout(config: HarnessConfig, cli_timeout: Duration) -> Self {
        let mut watch_specs = watch_specs_from_paths(&config.watch_paths);
        if let Some(dir) = data_dir(&config.env) {
            let filenames = OPENCODE_STORE_FILES.iter().map(|s| (*s).to_string()).collect();
            watch_specs.push(WatchSpec::files(dir, filenames));
        }
        let acp = AcpSessionDiscoverer::new(config.clone(), watch_specs.clone());
        Self {
            config,
            acp,
            watch_specs,
            cli_timeout,
            not_threads: Mutex::new(HashMap::new()),
        }
    }

    /// Runs `opencode session <args>`, bounded in time and output.
    fn run_cli(&self, args: &[&str]) -> Option<String> {
        let (env_set, env_remove) = acp::build_discovery_env(&self.config);
        let mut cmd = Command::new(&self.config.program);
        cmd.arg("session").args(args).env_remove("PWD");
        for key in &env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in &env_set {
            cmd.env(key, value);
        }
        let out = sys::run_bounded(cmd, self.cli_timeout, CLI_MAX_STDOUT).ok()?;
        out.success.then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_cli_export(&self, session_id: &str) -> Option<ExportSummary> {
        parse_cli_session_export(&self.run_cli(&["export", "--standalone", session_id])?)
    }

    fn not_threads(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.not_threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SessionDiscoverer for OpenCodeSessionDiscoverer {
    fn watch_specs(&self) -> Vec<WatchSpec> {
        self.watch_specs.clone()
    }

    fn has_store_data(&self) -> bool {
        self.watch_specs.iter().any(spec_has_data)
    }

    fn list_sessions(&self) -> Result<Vec<ExternalSession>, String> {
        self.acp.list_via_acp()
    }

    fn needs_classification(&self) -> bool {
        true
    }

    fn classify_sessions(
        &self,
        sessions: Vec<ExternalSession>,
        known_sessions: &HashSet<String>,
        known_subagents: &HashSet<String>,
    ) -> Result<Vec<ExternalSession>, String> {
        let mut kept = Vec::new();
        for mut session in sessions {
            let sid = session.harness_session_id.clone();
            if session.is_subagent || known_sessions.contains(&sid) || known_subagents.contains(&sid) {
                kept.push(session);
                continue;
            }
            // An empty or archived session is looked at again only once the
            // harness has changed it.
            if session.stamp.is_some() && self.not_threads().get(&sid) == session.stamp.as_ref() {
                continue;
            }
            // Only the session's own export says it has a message and no
            // parent: a title and a later `updated` come from a rename too.
            match self.run_cli_export(&sid) {
                Some(summary) if summary.has_parent => {
                    session.is_subagent = true;
                    kept.push(session);
                }
                Some(summary) if summary.archived || !summary.has_messages => {
                    if let Some(stamp) = session.stamp.clone() {
                        self.not_threads().insert(sid, stamp);
                    }
                }
                Some(summary) => {
                    session.created = summary.created_secs.or(session.created);
                    if session.title.is_none() {
                        session.title = summary.title;
                    }
                    kept.push(session);
                }
                // Unknown: left for a later pass, never published.
                None => {}
            }
        }
        Ok(kept)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ExportSummary {
    has_parent: bool,
    has_messages: bool,
    archived: bool,
    title: Option<String>,
    created_secs: Option<i64>,
}

fn parse_cli_session_export(stdout: &str) -> Option<ExportSummary> {
    let val: Value = serde_json::from_str(stdout.trim()).ok()?;
    let info = val.get("info")?;
    let has_parent = ["parentID", "parentId", "parent_id"].iter().any(|k| {
        info.get(*k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    });
    let has_messages = val
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|m| !m.is_empty());
    let archived = info
        .get("time")
        .and_then(|t| t.get("archived"))
        .is_some_and(|a| !a.is_null());
    let title = info
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let created_secs = info
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(Value::as_i64)
        .filter(|ms| *ms > 0)
        .map(|ms| ms / 1000);
    Some(ExportSummary {
        has_parent,
        has_messages,
        archived,
        title,
        created_secs,
    })
}

/// Reads JSON with comments and trailing commas, as OpenCode's settings may
/// have them. What is not an object is not settings.
pub fn parse_jsonc(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(&strip_jsonc(text)).ok()?;
    value.is_object().then_some(value)
}

/// JSON with its comments taken out, then any comma before a closing
/// bracket: a comment may come between the two.
fn strip_jsonc(text: &str) -> String {
    without_trailing_commas(&without_comments(text))
}

/// Calls `outside` for each character outside a string, and passes each one
/// inside a string, quotes and escapes too, to `out` as it is.
fn each_outside_strings(text: &str, mut outside: impl FnMut(&[char], &mut usize, &mut String)) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if chars[i - 1] == '"' {
                    break;
                }
            }
        } else {
            outside(&chars, &mut i, &mut out);
        }
    }
    out
}

/// The text with its `//` and `/* */` comments taken out.
fn without_comments(text: &str) -> String {
    each_outside_strings(text, |chars, i, out| {
        if chars[*i] == '/' && chars.get(*i + 1) == Some(&'/') {
            while *i < chars.len() && chars[*i] != '\n' {
                *i += 1;
            }
        } else if chars[*i] == '/' && chars.get(*i + 1) == Some(&'*') {
            *i += 2;
            while *i < chars.len() && !(chars[*i] == '*' && chars.get(*i + 1) == Some(&'/')) {
                *i += 1;
            }
            *i += 2;
            out.push(' ');
        } else {
            out.push(chars[*i]);
            *i += 1;
        }
    })
}

/// The text with no comma just before a closing bracket.
fn without_trailing_commas(text: &str) -> String {
    each_outside_strings(text, |chars, i, out| {
        let closes = chars[*i] == ',' && matches!(chars[*i + 1..].iter().find(|c| !c.is_whitespace()), Some('}' | ']'));
        if !closes {
            out.push(chars[*i]);
        }
        *i += 1;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_with_comments_and_trailing_commas_are_read() {
        let text = r#"{
            // the person's own
            "permission": { "bash": { "rm *": "deny", }, /* and edits */ "edit": "ask", },
            "url": "https://example.org/a//b",
            "quote": "a \" // not a comment",
        }"#;
        assert_eq!(
            parse_jsonc(text),
            Some(json!({
                "permission": { "bash": { "rm *": "deny" }, "edit": "ask" },
                "url": "https://example.org/a//b",
                "quote": "a \" // not a comment",
            }))
        );
        // A comment after a trailing comma, or in place of the last entry.
        let commented = "{ \"permission\": { \"bash\": {\n  \"rm *\": \"deny\", // keep\n  // \"ls\": \"allow\"\n} } }";
        assert_eq!(
            parse_jsonc(commented),
            Some(json!({ "permission": { "bash": { "rm *": "deny" } } }))
        );
        assert_eq!(parse_jsonc("{\"a\": [1, /* two */],}"), Some(json!({ "a": [1] })));
        assert_eq!(parse_jsonc("[1, 2]"), None);
        assert_eq!(parse_jsonc("not json"), None);
    }

    #[test]
    fn ask_first_asks_before_all_and_keeps_the_persons_own_rules_after_it() {
        // With no settings of the person's, it asks before every command and edit.
        assert_eq!(
            ask_first_permission(&[]),
            json!({ "bash": { "*": "ask" }, "edit": { "*": "ask" } })
        );

        // Their refusals and their own allowances still decide what they name.
        let theirs = json!({
            "permission": {
                "bash": { "*": "allow", "git status": "allow", "rm *": "deny" },
                "edit": { "secret/**": "deny" },
                "webfetch": "deny"
            }
        });
        let permission = ask_first_permission(&[theirs]);
        assert_eq!(
            permission,
            json!({
                "bash": { "*": "ask", "git status": "allow", "rm *": "deny" },
                "edit": { "*": "ask", "secret/**": "deny" }
            })
        );
        // Order is what decides, so it is kept.
        let bash: Vec<&String> = permission["bash"].as_object().unwrap().keys().collect();
        assert_eq!(bash, ["*", "git status", "rm *"]);

        // A refusal of everything stays a refusal, where the person put it.
        let deny_all = json!({ "permission": { "bash": { "git *": "allow", "*": "deny" }, "edit": "deny" } });
        let permission = ask_first_permission(&[deny_all]);
        let bash: Vec<(&String, &Value)> = permission["bash"].as_object().unwrap().iter().collect();
        assert_eq!(
            bash,
            [
                (&"git *".to_string(), &json!("allow")),
                (&"*".to_string(), &json!("deny"))
            ]
        );
        assert_eq!(permission["edit"], json!({ "*": "deny" }));

        // A rule for every action is one for commands and edits too, where
        // the person put it, and the newer list of rules is read as well.
        let every = json!({ "permission": { "*": "deny", "read": "allow", "bash": { "git status": "allow" } } });
        assert_eq!(
            ask_first_permission(&[every]),
            json!({ "bash": { "*": "deny", "git status": "allow" }, "edit": { "*": "deny" } })
        );
        let listed = json!({ "permissions": [
            { "action": "shell", "resource": "rm *", "effect": "deny" },
            { "action": "edit", "resource": "secret/**", "effect": "deny" },
            { "action": "read", "resource": "*", "effect": "deny" }
        ] });
        assert_eq!(
            ask_first_permission(&[listed]),
            json!({ "bash": { "*": "ask", "rm *": "deny" }, "edit": { "*": "ask", "secret/**": "deny" } })
        );

        // An action is a pattern, as OpenCode has it.
        let patterned = json!({
            "permission": { "sh*": { "rm *": "deny" } },
            "permissions": [{ "action": "ed?t", "resource": "secret/**", "effect": "deny" }]
        });
        assert_eq!(
            ask_first_permission(&[patterned]),
            json!({ "bash": { "*": "ask", "rm *": "deny" }, "edit": { "*": "ask", "secret/**": "deny" } })
        );
        // Older names are renamed whole before any pattern is matched: `ba*`
        // names no command, and `write` is an edit.
        let older = json!({
            "permission": { "ba*": { "ls": "allow" }, "write": { "secret/**": "deny" }, "bash": { "rm *": "deny" } }
        });
        assert_eq!(
            ask_first_permission(&[older]),
            json!({ "bash": { "*": "ask", "rm *": "deny" }, "edit": { "*": "ask", "secret/**": "deny" } })
        );
        assert!(matches_wildcard("shell", "*"));
        assert!(matches_wildcard("shell", "s*l"));
        assert!(!matches_wildcard("shell", "sh"));
        assert!(!matches_wildcard("bash", "edit"));

        // Later settings come after earlier ones, a pattern given again where
        // it was given last, and `shell` is `bash` by its newer name.
        let first = json!({ "permission": { "bash": { "rm *": "deny", "ls": "allow" } } });
        let second = json!({ "permission": { "shell": { "rm *": "ask" } } });
        assert_eq!(
            ask_first_permission(&[first, second])["bash"],
            json!({ "*": "ask", "ls": "allow", "rm *": "ask" })
        );
        let order: Vec<String> = ask_first_permission(&[
            json!({ "permission": { "bash": { "rm *": "deny", "ls": "allow" } } }),
            json!({ "permission": { "bash": { "rm *": "deny" } } }),
        ])["bash"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(order, ["*", "ls", "rm *"]);
    }

    #[test]
    fn the_persons_settings_are_read_where_opencode_reads_them() {
        let dir = std::env::temp_dir().join(format!("lever-opencode-{}", std::process::id()));
        let config = dir.join("xdg/opencode");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("opencode.json"), r#"{"permission":{"bash":{"a":"deny"}}}"#).unwrap();
        std::fs::write(
            config.join("opencode.jsonc"),
            "{ // c\n \"permission\": {\"bash\": {\"b\": \"deny\"}} }",
        )
        .unwrap();
        let named = dir.join("named.json");
        std::fs::write(&named, r#"{"permission":{"bash":{"c":"deny"}}}"#).unwrap();
        let env = vec![
            ("XDG_CONFIG_HOME".to_string(), dir.join("xdg").display().to_string()),
            ("OPENCODE_CONFIG".to_string(), named.display().to_string()),
            ("OPENCODE_CONFIG_DIR".to_string(), String::new()),
        ];
        let documents = person_settings(&env, Some(r#"{"permission":{"bash":{"d":"deny"}}}"#));
        assert_eq!(
            ask_first_permission(&documents)["bash"],
            json!({ "*": "ask", "a": "deny", "b": "deny", "c": "deny", "d": "deny" })
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_opencode_cli_session_export_json() {
        let top = parse_cli_session_export(
            r#"{"info":{"id":"ses_1","title":"Fix bug","time":{"created":1795756613112}},"messages":[{"role":"user"}]}"#,
        )
        .unwrap();
        assert_eq!(
            top,
            ExportSummary {
                has_parent: false,
                has_messages: true,
                archived: false,
                title: Some("Fix bug".into()),
                created_secs: Some(1_795_756_613),
            }
        );
        let child =
            parse_cli_session_export(r#"{"info":{"id":"ses_child","parentID":"ses_1"},"messages":[{"role":"user"}]}"#)
                .unwrap();
        assert_eq!(
            child,
            ExportSummary {
                has_parent: true,
                has_messages: true,
                archived: false,
                title: None,
                created_secs: None,
            }
        );
        let empty = parse_cli_session_export(r#"{"info":{"id":"ses_empty"},"messages":[]}"#).unwrap();
        assert_eq!(
            empty,
            ExportSummary {
                has_parent: false,
                has_messages: false,
                archived: false,
                title: None,
                created_secs: None,
            }
        );
        let archived =
            parse_cli_session_export(r#"{"info":{"id":"ses_a","time":{"archived":1795756614000}},"messages":[{}]}"#)
                .unwrap();
        assert!(archived.archived);
    }

    /// A stand-in for `opencode session export`, answering from
    /// files in its directory and logging each call.
    struct FakeOpenCode {
        dir: PathBuf,
    }

    impl FakeOpenCode {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("lever-fake-opencode-{}", sys::random_hex(6)));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("opencode");
            std::fs::write(
                &script,
                "#!/bin/sh\n\
                 d=\"$FAKE_OPENCODE_DIR\"\n\
                 echo \"$2 $4\" >> \"$d/calls\"\n\
                 case \"$2\" in\n\
                 export) [ -f \"$d/hang-$4\" ] && exec sleep 30; [ -f \"$d/export-$4.json\" ] || exit 1; cat \"$d/export-$4.json\" ;;\n\
                 esac\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            Self { dir }
        }

        fn discoverer(&self, timeout: Duration) -> OpenCodeSessionDiscoverer {
            let config = HarnessConfig {
                id: "opencode".into(),
                name: "OpenCode".into(),
                program: self.dir.join("opencode").to_string_lossy().into_owned(),
                args: vec!["acp".into()],
                env: vec![
                    ("FAKE_OPENCODE_DIR".into(), self.dir.to_string_lossy().into_owned()),
                    (
                        "XDG_DATA_HOME".into(),
                        self.dir.join("data").to_string_lossy().into_owned(),
                    ),
                ],
                watch_paths: Vec::new(),
            };
            OpenCodeSessionDiscoverer::with_cli_timeout(config, timeout)
        }

        fn write(&self, name: &str, content: &str) {
            std::fs::write(self.dir.join(name), content).unwrap();
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn session(&self, id: &str, stamp: &str) -> ExternalSession {
            ExternalSession {
                harness_session_id: id.into(),
                cwd: self.dir.to_string_lossy().into_owned(),
                title: None,
                created: None,
                updated: crate::discovery::parse_rfc3339_unix(stamp),
                stamp: Some(stamp.into()),
                is_subagent: false,
            }
        }
    }

    impl Drop for FakeOpenCode {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn by_id(sessions: &[ExternalSession]) -> HashMap<&str, bool> {
        sessions
            .iter()
            .map(|s| (s.harness_session_id.as_str(), s.is_subagent))
            .collect()
    }

    #[test]
    fn only_positive_evidence_makes_a_session_a_thread_or_a_subagent() {
        let fake = FakeOpenCode::new();
        fake.write(
            "export-ses_root.json",
            r#"{"info":{"id":"ses_root","title":"Fix bug"},"messages":[{}]}"#,
        );
        fake.write("export-ses_empty.json", r#"{"info":{"id":"ses_empty"},"messages":[]}"#);
        // Renamed by hand before its first message: a title and a later
        // `updated`, and still nothing in it.
        fake.write(
            "export-ses_renamed.json",
            r#"{"info":{"id":"ses_renamed","title":"Named"},"messages":[]}"#,
        );
        fake.write(
            "export-ses_child.json",
            r#"{"info":{"id":"ses_child","parentID":"ses_root"},"messages":[{}]}"#,
        );
        fake.write(
            "export-ses_archived.json",
            r#"{"info":{"id":"ses_archived","time":{"archived":4000}},"messages":[{}]}"#,
        );
        let discoverer = fake.discoverer(Duration::from_secs(10));
        let ids = [
            "ses_root",
            "ses_empty",
            "ses_renamed",
            "ses_child",
            "ses_archived",
            "ses_unknown",
            "ses_known",
        ];
        let first = "2026-01-01T00:00:00.100Z";
        let sessions = |stamp: &str| ids.iter().map(|id| fake.session(id, stamp)).collect::<Vec<_>>();
        let known: HashSet<String> = ["ses_known".to_string()].into();

        let kept = discoverer
            .classify_sessions(sessions(first), &known, &HashSet::new())
            .unwrap();
        let kept_ids = by_id(&kept);
        assert_eq!(kept_ids.get("ses_root"), Some(&false));
        assert_eq!(
            kept[0].title.as_deref(),
            Some("Fix bug"),
            "the export's title fills a missing one"
        );
        assert_eq!(kept_ids.get("ses_child"), Some(&true));
        assert_eq!(
            kept_ids.get("ses_known"),
            Some(&false),
            "a session Lever knows is not exported"
        );
        assert!(!kept_ids.contains_key("ses_empty"));
        assert!(!kept_ids.contains_key("ses_renamed"), "a rename is not a message");
        assert!(!kept_ids.contains_key("ses_archived"));
        // Its export failed: neither a thread nor a subagent, so it is tried again.
        assert!(!kept_ids.contains_key("ses_unknown"));
        assert!(!fake.calls().iter().any(|c| c.ends_with("ses_known")));

        let exports_before = fake.calls().len();
        let again = discoverer
            .classify_sessions(sessions(first), &known, &HashSet::new())
            .unwrap();
        let new_calls = fake.calls()[exports_before..].to_vec();
        assert!(
            !new_calls
                .iter()
                .any(|c| c.ends_with("ses_empty") || c.ends_with("ses_archived"))
        );
        assert!(new_calls.iter().any(|c| c.ends_with("ses_unknown")));
        assert!(!by_id(&again).contains_key("ses_unknown"));

        // Its first message comes within the same second: a new stamp all the
        // same, so it is looked at again.
        fake.write(
            "export-ses_empty.json",
            r#"{"info":{"id":"ses_empty"},"messages":[{}]}"#,
        );
        let kept = discoverer
            .classify_sessions(sessions("2026-01-01T00:00:00.500Z"), &known, &HashSet::new())
            .unwrap();
        assert_eq!(by_id(&kept).get("ses_empty"), Some(&false));
    }

    #[test]
    fn a_classification_command_that_hangs_is_ended_and_tried_again() {
        let fake = FakeOpenCode::new();
        fake.write("export-ses_slow.json", r#"{"info":{"id":"ses_slow"},"messages":[{}]}"#);
        fake.write("hang-ses_slow", "");
        let discoverer = fake.discoverer(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let kept = discoverer
            .classify_sessions(
                vec![fake.session("ses_slow", "2026-01-01T00:00:01Z")],
                &HashSet::new(),
                &HashSet::new(),
            )
            .unwrap();
        assert!(kept.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );

        std::fs::remove_file(fake.dir.join("hang-ses_slow")).unwrap();
        let kept = discoverer
            .classify_sessions(
                vec![fake.session("ses_slow", "2026-01-01T00:00:01Z")],
                &HashSet::new(),
                &HashSet::new(),
            )
            .unwrap();
        assert_eq!(by_id(&kept).get("ses_slow"), Some(&false));
    }
}
