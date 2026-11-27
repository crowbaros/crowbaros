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
use crate::watcher::WatchSpec;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
/// folders via short-lived ACP `session/list`, and classifies brand-new unseen
/// sessions with OpenCode's own `opencode session list` / `export` commands so
/// subagents and empty sessions are never listed as threads. OpenCode is never
/// kept running for this.
pub struct OpenCodeSessionDiscoverer {
    config: HarnessConfig,
    acp: AcpSessionDiscoverer,
    watch_specs: Vec<WatchSpec>,
}

const OPENCODE_STORE_FILES: [&str; 4] = [
    "opencode.db",
    "opencode.db-wal",
    "opencode.db-journal",
    "opencode.db-shm",
];
/// Limit passed to `opencode session list --max-count` when classifying new
/// sessions in a project directory.
const CLI_LIST_MAX_COUNT: usize = 1000;

impl OpenCodeSessionDiscoverer {
    pub fn new(config: HarnessConfig) -> Self {
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
        }
    }

    fn run_cli_list(&self, cwd: &Path) -> Option<Vec<CliSessionEntry>> {
        let (env_set, env_remove) = acp::build_discovery_env(&self.config);
        let mut cmd = Command::new(&self.config.program);
        cmd.args([
            "session",
            "list",
            "--standalone",
            "--format",
            "json",
            "--max-count",
            &CLI_LIST_MAX_COUNT.to_string(),
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("PWD");
        for key in &env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in &env_set {
            cmd.env(key, value);
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        parse_cli_session_list(&String::from_utf8_lossy(&out.stdout))
    }

    fn run_cli_export(&self, session_id: &str) -> Option<ExportSummary> {
        let (env_set, env_remove) = acp::build_discovery_env(&self.config);
        let mut cmd = Command::new(&self.config.program);
        cmd.args(["session", "export", "--standalone", session_id])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("PWD");
        for key in &env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in &env_set {
            cmd.env(key, value);
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        parse_cli_session_export(&String::from_utf8_lossy(&out.stdout))
    }
}

impl SessionDiscoverer for OpenCodeSessionDiscoverer {
    fn watch_specs(&self) -> Vec<WatchSpec> {
        self.watch_specs.clone()
    }

    fn has_store_data(&self) -> bool {
        self.watch_specs.iter().any(spec_has_data)
    }

    fn list_sessions(
        &self,
        known_sessions: &HashSet<String>,
        known_subagents: &HashSet<String>,
    ) -> Result<Vec<ExternalSession>, String> {
        let sessions = self.acp.list_via_acp()?;
        self.classify_sessions(sessions, known_sessions, known_subagents)
    }

    fn needs_classification(&self) -> bool {
        true
    }

    fn classify_sessions(
        &self,
        mut sessions: Vec<ExternalSession>,
        known_sessions: &HashSet<String>,
        known_subagents: &HashSet<String>,
    ) -> Result<Vec<ExternalSession>, String> {
        // Group brand-new unseen sessions by their directory so we run at most
        // one `opencode session list` per directory that has unseen sessions.
        let mut unseen_by_cwd: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, session) in sessions.iter().enumerate() {
            if session.is_subagent
                || known_sessions.contains(&session.harness_session_id)
                || known_subagents.contains(&session.harness_session_id)
            {
                continue;
            }
            unseen_by_cwd.entry(session.cwd.clone()).or_default().push(idx);
        }

        let mut drop_indices: HashSet<usize> = HashSet::new();
        for (cwd, indices) in unseen_by_cwd {
            let cwd_path = Path::new(&cwd);
            let cli_entries = if cwd_path.is_dir() {
                self.run_cli_list(cwd_path)
            } else {
                None
            };
            let list_complete = cli_entries
                .as_ref()
                .is_some_and(|entries| entries.len() < CLI_LIST_MAX_COUNT);
            let by_id: HashMap<&str, &CliSessionEntry> = cli_entries
                .as_ref()
                .map(|entries| entries.iter().map(|e| (e.id.as_str(), e)).collect())
                .unwrap_or_default();

            for idx in indices {
                let sid = sessions[idx].harness_session_id.clone();
                if let Some(entry) = by_id.get(sid.as_str()) {
                    if let Some(created) = entry.created_secs {
                        sessions[idx].created = Some(created);
                    }
                    if let Some(updated) = entry.updated_secs {
                        sessions[idx].updated = Some(updated);
                    }
                    if let Some(title) = entry.title.as_ref().filter(|t| !t.trim().is_empty()) {
                        sessions[idx].title = Some(title.clone());
                    }
                    // OpenCode's `session list` only returns top-level sessions.
                    // A session with a title and activity beyond its creation
                    // timestamp already has its first message.
                    let has_activity = entry.title.as_deref().is_some_and(|t| !t.trim().is_empty())
                        && entry.updated_ms > entry.created_ms;
                    if has_activity {
                        continue;
                    }
                    // Ambiguous (no title yet or same-ms timestamps): check
                    // whether any messages exist via `session export`.
                    if let Some(summary) = self.run_cli_export(&sid) {
                        if summary.has_parent {
                            sessions[idx].is_subagent = true;
                        } else if !summary.has_messages {
                            drop_indices.insert(idx);
                        }
                    } else {
                        // Skip empty-looking sessions until a message arrives.
                        drop_indices.insert(idx);
                    }
                } else if list_complete {
                    // Absent from OpenCode's complete top-level session list for
                    // this directory: subagent (or archived) session.
                    sessions[idx].is_subagent = true;
                } else if let Some(summary) = self.run_cli_export(&sid) {
                    if summary.has_parent {
                        sessions[idx].is_subagent = true;
                    } else if !summary.has_messages {
                        drop_indices.insert(idx);
                    }
                }
            }
        }

        if !drop_indices.is_empty() {
            let mut filtered = Vec::with_capacity(sessions.len() - drop_indices.len());
            for (idx, session) in sessions.into_iter().enumerate() {
                if !drop_indices.contains(&idx) {
                    filtered.push(session);
                }
            }
            sessions = filtered;
        }
        Ok(sessions)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CliSessionEntry {
    id: String,
    title: Option<String>,
    created_ms: i64,
    updated_ms: i64,
    created_secs: Option<i64>,
    updated_secs: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
struct ExportSummary {
    has_parent: bool,
    has_messages: bool,
}

fn parse_cli_session_list(stdout: &str) -> Option<Vec<CliSessionEntry>> {
    let val: Value = serde_json::from_str(stdout.trim()).ok()?;
    let arr = val.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(id) = item.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let created_ms = item.get("created").and_then(Value::as_i64).unwrap_or(0);
        let updated_ms = item.get("updated").and_then(Value::as_i64).unwrap_or(created_ms);
        out.push(CliSessionEntry {
            id: id.to_string(),
            title,
            created_ms,
            updated_ms,
            created_secs: (created_ms > 0).then_some(created_ms / 1000),
            updated_secs: (updated_ms > 0).then_some(updated_ms / 1000),
        });
    }
    Some(out)
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
    Some(ExportSummary {
        has_parent,
        has_messages,
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
    fn parses_opencode_cli_session_list_and_export_json() {
        let list = parse_cli_session_list(
            r#"[
                {"id": "ses_1", "title": "Fix bug", "created": 1795756613112, "updated": 1795756613425},
                {"id": "ses_empty", "created": 1795756614000, "updated": 1795756614000}
            ]"#,
        )
        .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ses_1");
        assert_eq!(list[0].title.as_deref(), Some("Fix bug"));
        assert_eq!(list[0].created_secs, Some(1_795_756_613));
        assert_eq!(list[1].title, None);

        let top = parse_cli_session_export(r#"{"info":{"id":"ses_1"},"messages":[{"role":"user"}]}"#).unwrap();
        assert_eq!(
            top,
            ExportSummary {
                has_parent: false,
                has_messages: true
            }
        );
        let child =
            parse_cli_session_export(r#"{"info":{"id":"ses_child","parentID":"ses_1"},"messages":[{"role":"user"}]}"#)
                .unwrap();
        assert_eq!(
            child,
            ExportSummary {
                has_parent: true,
                has_messages: true
            }
        );
        let empty = parse_cli_session_export(r#"{"info":{"id":"ses_empty"},"messages":[]}"#).unwrap();
        assert_eq!(
            empty,
            ExportSummary {
                has_parent: false,
                has_messages: false
            }
        );
    }
}
