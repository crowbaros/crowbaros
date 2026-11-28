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

use crate::discovery::{Classified, KnownSessions, Listing, SessionDiscoverer, spec_has_data, watch_specs_from_paths};
use crate::harness::{ExternalSession, HarnessConfig};
use crate::sys;
use crate::watcher::WatchSpec;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The OpenCode agent that is Lever's Ask first mode.
pub const ASK_FIRST_AGENT: &str = "crowbar-ask-first";

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

/// The Ask first agent's `permission`: it asks before a command, an edit, or
/// one of Lever's own MCP tools (`crowbar_*`), except where the person's own
/// rules say otherwise. Emits the concrete tool key (`crowbar_make_project`)
/// after `crowbar_*` so a person's explicit rule for `crowbar_make_project`
/// (or a narrower pattern like `crowbar_make_*`) is never overridden by the
/// wildcard `crowbar_*` ask rule.
pub fn ask_first_permission(documents: &[Value]) -> Value {
    let mut permission = Map::new();
    permission.insert("bash".to_string(), rules_for(documents, "shell"));
    permission.insert("edit".to_string(), rules_for(documents, "edit"));
    permission.insert("crowbar_*".to_string(), rules_for(documents, "crowbar_*"));
    let make_project = format!("{}_{}", crate::mcp::MCP_SERVER_NAME, crate::mcp::TOOL_MAKE_PROJECT);
    permission.insert(make_project.clone(), rules_for(documents, &make_project));
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
    let var = |key: &str| env_var(env, key);
    var("XDG_DATA_HOME")
        .map(|x| PathBuf::from(x).join("opencode"))
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".local/share/opencode")))
}

/// A variable from the harness's environment, over Lever's own.
fn env_var(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var(key).ok())
        .filter(|v| !v.is_empty())
}

/// OpenCode's database, as a release build of OpenCode finds it:
/// `OPENCODE_DB` when set, or `opencode.db`, taken from its data directory
/// unless absolute. `None` for one kept in memory, which has no file to
/// watch.
pub fn database_path(env: &[(String, String)]) -> Option<PathBuf> {
    let name = env_var(env, "OPENCODE_DB").unwrap_or_else(|| "opencode.db".to_string());
    if name == ":memory:" {
        return None;
    }
    let path = PathBuf::from(&name);
    let path = if path.is_absolute() {
        path
    } else {
        data_dir(env)?.join(path)
    };
    Some(lexical(&path))
}

/// A path with `.` and `..` taken out by the path alone, whether or not
/// what they name is there, as OpenCode resolves its database's.
fn lexical(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// Where a database that is a link leads, even when the file it leads to is
/// not there yet: SQLite makes it there, and its sidecars beside it. Each
/// link on the way is followed from the folder it is really in, as the
/// system follows it.
fn link_target(db: &Path) -> Option<PathBuf> {
    if let Ok(target) = std::fs::canonicalize(db) {
        return Some(target);
    }
    // As many links as the system follows before it gives up.
    const MAX_LINKS: usize = 40;
    let mut current = db.to_path_buf();
    for _ in 0..MAX_LINKS {
        let Ok(link) = std::fs::read_link(&current) else {
            break;
        };
        let joined = if link.is_absolute() {
            link
        } else {
            let dir = current.parent()?;
            std::fs::canonicalize(dir)
                .unwrap_or_else(|_| dir.to_path_buf())
                .join(link)
        };
        // The folder it names, where it is there, is taken as it really is.
        current = match (
            joined.parent().and_then(|dir| std::fs::canonicalize(dir).ok()),
            joined.file_name(),
        ) {
            (Some(dir), Some(name)) => dir.join(name),
            _ => lexical(&joined),
        };
    }
    (current != db).then_some(current)
}

/// Watches a SQLite database and the files beside it its writes change.
fn database_watch(db: &Path) -> Option<WatchSpec> {
    let (dir, name) = (db.parent()?, db.file_name()?.to_str()?);
    let mut filenames = vec![name.to_string()];
    filenames.extend(SQLITE_SIDECARS.iter().map(|side| format!("{name}{side}")));
    Some(WatchSpec::files(dir.to_path_buf(), filenames))
}

/// OpenCode's [`SessionDiscoverer`]: watches OpenCode's own database
/// ([`database_path`] and its WAL/journal sidecars), lists sessions across all
/// folders via short-lived ACP `session/list`, and classifies each brand-new
/// unseen session with OpenCode's own `opencode session export` so subagents
/// and empty sessions are never listed as threads. OpenCode is never kept
/// running for this.
///
/// A session becomes a thread only on positive evidence that it is a
/// top-level session with a message: its export has messages and no parent. A
/// session whose export fails is left for a later pass, tried again after a
/// wait that grows each time, and only an export that names a parent marks it
/// a subagent for good.
/// Builds OpenCode's spawn environment with self-update disabled, the Ask first
/// agent injected via `OPENCODE_CONFIG_CONTENT`, and free hosted Models gated
/// behind `free_hosted_models`.
pub fn build_harness_env(
    extra: &[(String, String)],
    existing_config_content: Option<&str>,
    person: &[Value],
    free_hosted_models: bool,
) -> (Vec<(String, String)>, Vec<&'static str>) {
    let mut env_set: Vec<(String, String)> = extra
        .iter()
        .filter(|(k, _)| {
            k != "OPENCODE_CONFIG_CONTENT"
                && k != "OPENCODE_DISABLE_FREE_MODELS"
                && (free_hosted_models || k != "OPENCODE_DISABLE_MODELS_FETCH")
        })
        .cloned()
        .collect();
    env_set.push((
        "OPENCODE_CONFIG_CONTENT".to_string(),
        opencode_config_content(existing_config_content, person),
    ));
    let mut env_remove = Vec::new();
    if free_hosted_models {
        env_remove.push("OPENCODE_DISABLE_FREE_MODELS");
    } else {
        env_set.push(("OPENCODE_DISABLE_FREE_MODELS".to_string(), "1".to_string()));
        env_set.push(("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string()));
    }
    (env_set, env_remove)
}

/// Produces the `OPENCODE_CONFIG_CONTENT` JSON string: OpenCode's
/// self-update switched off, and the Ask first agent, built from OpenCode's
/// own permission settings, added and made the agent a session starts with.
/// Ask first keeps the person's own rules for commands and edits (`person`,
/// their settings documents). It merges into any existing JSON object from
/// the environment, keeping the other agents there, and touches no file in
/// the person's home.
pub fn opencode_config_content(existing: Option<&str>, person: &[Value]) -> String {
    let mut map = existing
        .and_then(parse_jsonc)
        .and_then(|val| val.as_object().cloned())
        .unwrap_or_default();
    map.insert("update".to_string(), json!("disable"));
    map.insert("default_agent".to_string(), json!(ASK_FIRST_AGENT));
    let mut agents = map.get("agent").and_then(Value::as_object).cloned().unwrap_or_default();
    agents.insert(
        ASK_FIRST_AGENT.to_string(),
        json!({
            "mode": "primary",
            "description": "Asks before it runs a command or edits a file.",
            "permission": ask_first_permission(person),
        }),
    );
    map.insert("agent".to_string(), Value::Object(agents));
    Value::Object(map).to_string()
}

/// Builds a sanitized environment for one-shot OpenCode discovery/export
/// commands: never enables free hosted Models, never contacts
/// `models.opencode.ai`, disables self-update, and removes `PWD` so
/// `.current_dir(...)` is honored.
pub fn build_discovery_env(config: &HarnessConfig) -> (Vec<(String, String)>, Vec<&'static str>) {
    let existing_config = config
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("OPENCODE_CONFIG_CONTENT").ok());
    let mut env_set: Vec<(String, String)> = config
        .env
        .iter()
        .filter(|(k, _)| {
            k != "OPENCODE_CONFIG_CONTENT"
                && k != "OPENCODE_DISABLE_FREE_MODELS"
                && k != "OPENCODE_DISABLE_MODELS_FETCH"
                && k != "PWD"
        })
        .cloned()
        .collect();
    let mut cfg_map = existing_config
        .as_deref()
        .and_then(parse_jsonc)
        .and_then(|val| val.as_object().cloned())
        .unwrap_or_default();
    cfg_map.insert("update".to_string(), json!("disable"));
    env_set.push((
        "OPENCODE_CONFIG_CONTENT".to_string(),
        Value::Object(cfg_map).to_string(),
    ));
    env_set.push(("OPENCODE_DISABLE_FREE_MODELS".to_string(), "1".to_string()));
    env_set.push(("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string()));
    (env_set, vec!["PWD"])
}

/// Computes the watch specs for OpenCode's database and configured extra watch paths.
pub fn watch_specs_for(config: &HarnessConfig) -> Vec<WatchSpec> {
    let mut watch_specs = watch_specs_from_paths(&config.watch_paths);
    if let Some(db) = database_path(&config.env) {
        watch_specs.extend(database_watch(&db));
        // A database that is a link is written where it leads, and its
        // sidecars are kept there.
        if let Some(target) = link_target(&db)
            && target != db
        {
            watch_specs.extend(database_watch(&target));
        }
    }
    watch_specs
}

pub struct OpenCodeSessionDiscoverer {
    config: HarnessConfig,
    lister: Box<dyn SessionDiscoverer>,
    watch_specs: Vec<WatchSpec>,
    cli_timeout: Duration,
    /// Sessions whose export failed.
    failures: Mutex<HashMap<String, Failures>>,
}

/// The files beside an SQLite database that its writes also change.
const SQLITE_SIDECARS: [&str; 3] = ["-wal", "-journal", "-shm"];
/// How long one `opencode session` command may run before it is ended.
const CLI_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a pass starts exports for; what is left goes to the next pass.
const CLASSIFY_BUDGET: Duration = Duration::from_secs(2);
/// The least time left of a pass's budget that a later export starts with.
const EXPORT_FLOOR: Duration = Duration::from_millis(500);
/// How long a session whose export failed waits before it is tried again,
/// doubling each time it fails, up to [`EXPORT_RETRY_MAX`].
const EXPORT_RETRY: Duration = Duration::from_secs(5);
const EXPORT_RETRY_MAX: Duration = Duration::from_secs(300);
/// The most a `session export` may print.
const CLI_MAX_STDOUT: usize = 64 * 1024 * 1024;

impl OpenCodeSessionDiscoverer {
    pub fn new(config: HarnessConfig, watch_specs: Vec<WatchSpec>, lister: Box<dyn SessionDiscoverer>) -> Self {
        Self::with_cli_timeout(config, watch_specs, lister, CLI_TIMEOUT)
    }

    fn with_cli_timeout(
        config: HarnessConfig,
        watch_specs: Vec<WatchSpec>,
        lister: Box<dyn SessionDiscoverer>,
        cli_timeout: Duration,
    ) -> Self {
        Self {
            config,
            lister,
            watch_specs,
            cli_timeout,
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// Runs `opencode session <args>`, bounded in time and output.
    fn run_cli(&self, args: &[&str], timeout: Duration) -> Ran {
        let (env_set, env_remove) = build_discovery_env(&self.config);
        let mut cmd = Command::new(&self.config.program);
        cmd.arg("session").args(args).env_remove("PWD");
        for key in &env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in &env_set {
            cmd.env(key, value);
        }
        match sys::run_bounded(cmd, timeout, CLI_MAX_STDOUT) {
            Ok(out) if out.success => Ran::Printed(String::from_utf8_lossy(&out.stdout).into_owned()),
            Err(err) if err == sys::TIMED_OUT => Ran::TimedOut,
            _ => Ran::Failed,
        }
    }

    fn run_cli_export(&self, session_id: &str, timeout: Duration) -> Result<ExportSummary, Ran> {
        match self.run_cli(&["export", "--standalone", session_id], timeout) {
            Ran::Printed(out) => parse_cli_session_export(&out).ok_or(Ran::Failed),
            other => Err(other),
        }
    }

    fn failures(&self) -> std::sync::MutexGuard<'_, HashMap<String, Failures>> {
        self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// When a session whose export failed lately may be tried again, while
    /// it may not yet. One the harness has changed since may be at once.
    fn backing_off(&self, session: &ExternalSession) -> Option<Instant> {
        self.failures()
            .get(&session.harness_session_id)
            .filter(|failed| failed.stamp == session.stamp)
            .map(|failed| failed.retry)
            .filter(|retry| Instant::now() < *retry)
    }

    /// A session's export failed: it waits longer each time before the
    /// next. Returns when it may be tried again.
    fn failed(&self, session: &ExternalSession) -> Instant {
        let mut failures = self.failures();
        let failed = failures.entry(session.harness_session_id.clone()).or_insert(Failures {
            count: 0,
            retry: Instant::now(),
            stamp: None,
        });
        // A session the harness has changed since starts its count again.
        if failed.stamp != session.stamp {
            failed.count = 0;
        }
        failed.count = failed.count.saturating_add(1);
        let wait = EXPORT_RETRY
            .saturating_mul(1 << failed.count.clamp(1, 8).saturating_sub(1))
            .min(EXPORT_RETRY_MAX);
        failed.retry = Instant::now() + wait;
        failed.stamp = session.stamp.clone();
        failed.retry
    }
}

impl SessionDiscoverer for OpenCodeSessionDiscoverer {
    fn watch_specs(&self) -> Vec<WatchSpec> {
        self.watch_specs.clone()
    }

    fn has_store_data(&self) -> bool {
        self.watch_specs.iter().any(spec_has_data)
    }

    fn list_sessions(&self) -> Result<Listing, String> {
        self.lister.list_sessions()
    }

    fn needs_classification(&self) -> bool {
        true
    }

    fn classify_sessions(&self, sessions: Vec<ExternalSession>, known: &KnownSessions) -> Classified {
        let started = Instant::now();
        let mut found = Classified::default();
        let (mut unseen, seen): (Vec<_>, Vec<_>) = sessions.into_iter().partition(|s| known.unseen(s));
        // One found empty or archived, unchanged since, is no thread.
        found.sessions = seen.into_iter().filter(|s| !known.is_quiet(s)).collect();
        // The newest first, so a session started just now is not held back
        // behind older ones Lever has yet to look at.
        unseen.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.stamp.cmp(&a.stamp)));
        for mut session in unseen {
            let sid = session.harness_session_id.clone();
            if let Some(retry) = self.backing_off(&session) {
                found.retry_at = Some(found.retry_at.map_or(retry, |at| at.min(retry)));
                continue;
            }
            let left = CLASSIFY_BUDGET.saturating_sub(started.elapsed());
            // Too little left for an export to finish is left for the next pass.
            if found.changed_after && left < EXPORT_FLOOR {
                found.more = true;
                break;
            }
            // The first export has its full time; the rest only what is left
            // of the pass's, so a slow old one does not hold back the newest.
            let first = !found.changed_after;
            let timeout = if first {
                self.cli_timeout
            } else {
                left.min(self.cli_timeout)
            };
            found.changed_after = true;
            // Only the session's own export says it has a message and no
            // parent: a title and a later `updated` come from a rename too.
            let summary = match self.run_cli_export(&sid, timeout) {
                Ok(summary) => summary,
                Err(Ran::TimedOut) if !first => {
                    // Out of the pass's time: first in line on the next.
                    found.more = true;
                    break;
                }
                Err(_) => {
                    // Unknown: never published, and tried again after a wait.
                    let retry = self.failed(&session);
                    found.retry_at = Some(found.retry_at.map_or(retry, |at| at.min(retry)));
                    continue;
                }
            };
            self.failures().remove(&sid);
            if summary.has_parent {
                session.is_subagent = true;
                found.sessions.push(session);
            } else if summary.archived || !summary.has_messages {
                if let Some(stamp) = session.stamp.clone() {
                    found.quiet.push((sid, stamp));
                }
            } else {
                session.created = summary.created_secs.or(session.created);
                if session.title.is_none() {
                    session.title = summary.title;
                }
                found.sessions.push(session);
            }
        }
        found
    }
}

/// How a `session` command ran.
#[derive(Debug, PartialEq, Eq)]
enum Ran {
    Printed(String),
    TimedOut,
    Failed,
}

/// A session whose export failed: how often in a row, when it may be tried
/// again, and its stamp then.
#[derive(Debug)]
struct Failures {
    count: u32,
    retry: Instant,
    stamp: Option<String>,
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
        // With no settings of the person's, it asks before every command, edit,
        // and CrowbarOS MCP tool.
        assert_eq!(
            ask_first_permission(&[]),
            json!({
                "bash": { "*": "ask" },
                "edit": { "*": "ask" },
                "crowbar_*": { "*": "ask" },
                "crowbar_make_project": { "*": "ask" }
            })
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
                "edit": { "*": "ask", "secret/**": "deny" },
                "crowbar_*": { "*": "ask" },
                "crowbar_make_project": { "*": "ask" }
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

        // A rule for every action is one for commands, edits, and CrowbarOS MCP
        // tools too, where the person put it, and the newer list of rules is
        // read as well.
        let every = json!({ "permission": { "*": "deny", "read": "allow", "bash": { "git status": "allow" } } });
        assert_eq!(
            ask_first_permission(&[every]),
            json!({
                "bash": { "*": "deny", "git status": "allow" },
                "edit": { "*": "deny" },
                "crowbar_*": { "*": "deny" },
                "crowbar_make_project": { "*": "deny" }
            })
        );
        let listed = json!({ "permissions": [
            { "action": "shell", "resource": "rm *", "effect": "deny" },
            { "action": "edit", "resource": "secret/**", "effect": "deny" },
            { "action": "read", "resource": "*", "effect": "deny" }
        ] });
        assert_eq!(
            ask_first_permission(&[listed]),
            json!({
                "bash": { "*": "ask", "rm *": "deny" },
                "edit": { "*": "ask", "secret/**": "deny" },
                "crowbar_*": { "*": "ask" },
                "crowbar_make_project": { "*": "ask" }
            })
        );

        // An explicit refusal of `crowbar_make_project` (or a narrower pattern)
        // stays refused even though Ask first adds `crowbar_*: ask`.
        let deny_make = json!({ "permission": { "crowbar_make_project": "deny" } });
        let perm_deny_make = ask_first_permission(&[deny_make]);
        assert_eq!(perm_deny_make["crowbar_*"], json!({ "*": "ask" }));
        assert_eq!(perm_deny_make["crowbar_make_project"], json!({ "*": "deny" }));

        let deny_pattern = json!({ "permissions": [
            { "action": "crowbar_make_*", "resource": "*", "effect": "deny" }
        ] });
        assert_eq!(
            ask_first_permission(&[deny_pattern])["crowbar_make_project"],
            json!({ "*": "deny" })
        );

        // An action is a pattern, as OpenCode has it.
        let patterned = json!({
            "permission": { "sh*": { "rm *": "deny" } },
            "permissions": [{ "action": "ed?t", "resource": "secret/**", "effect": "deny" }]
        });
        assert_eq!(
            ask_first_permission(&[patterned]),
            json!({
                "bash": { "*": "ask", "rm *": "deny" },
                "edit": { "*": "ask", "secret/**": "deny" },
                "crowbar_*": { "*": "ask" },
                "crowbar_make_project": { "*": "ask" }
            })
        );
        // Older names are renamed whole before any pattern is matched: `ba*`
        // names no command, and `write` is an edit.
        let older = json!({
            "permission": { "ba*": { "ls": "allow" }, "write": { "secret/**": "deny" }, "bash": { "rm *": "deny" } }
        });
        assert_eq!(
            ask_first_permission(&[older]),
            json!({
                "bash": { "*": "ask", "rm *": "deny" },
                "edit": { "*": "ask", "secret/**": "deny" },
                "crowbar_*": { "*": "ask" },
                "crowbar_make_project": { "*": "ask" }
            })
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
    fn the_database_watched_is_the_one_opencode_uses() {
        let env = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect()
        };
        let data = [("XDG_DATA_HOME", "/data"), ("OPENCODE_DB", "")];
        assert_eq!(
            database_path(&env(&data)),
            Some(PathBuf::from("/data/opencode/opencode.db"))
        );
        let named = [("XDG_DATA_HOME", "/data"), ("OPENCODE_DB", "profile.db")];
        assert_eq!(
            database_path(&env(&named)),
            Some(PathBuf::from("/data/opencode/profile.db"))
        );
        let absolute = [("XDG_DATA_HOME", "/data"), ("OPENCODE_DB", "/elsewhere/work.db")];
        assert_eq!(
            database_path(&env(&absolute)),
            Some(PathBuf::from("/elsewhere/work.db"))
        );
        assert_eq!(database_path(&env(&[("OPENCODE_DB", ":memory:")])), None);
        // `..` is taken out though the folder it climbs out of is not there.
        let climbing = [("XDG_DATA_HOME", "/data"), ("OPENCODE_DB", "retired/../profile.db")];
        assert_eq!(
            database_path(&env(&climbing)),
            Some(PathBuf::from("/data/opencode/profile.db"))
        );

        let config = HarnessConfig {
            id: "opencode".into(),
            name: "OpenCode".into(),
            program: "opencode".into(),
            args: vec!["acp".into()],
            env: env(&named),
            watch_paths: Vec::new(),
        };
        let specs = watch_specs_for(&config);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].dir, PathBuf::from("/data/opencode"));
        assert_eq!(
            specs[0].filenames,
            ["profile.db", "profile.db-wal", "profile.db-journal", "profile.db-shm"]
        );
    }

    #[test]
    fn a_database_that_is_a_link_is_watched_where_it_leads() {
        let root = std::env::temp_dir().join(format!("lever-db-link-{}", sys::random_hex(4)));
        let data = root.join("data").join("opencode");
        let moved = root.join("moved");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&moved).unwrap();
        let watched = |target_exists: bool| {
            let _ = std::fs::remove_file(data.join("opencode.db"));
            let _ = std::fs::remove_file(moved.join("real.db"));
            if target_exists {
                std::fs::write(moved.join("real.db"), "").unwrap();
            }
            std::os::unix::fs::symlink(moved.join("real.db"), data.join("opencode.db")).unwrap();
            let config = HarnessConfig {
                id: "opencode".into(),
                name: "OpenCode".into(),
                program: "opencode".into(),
                args: vec!["acp".into()],
                env: vec![
                    ("XDG_DATA_HOME".into(), root.join("data").to_string_lossy().into_owned()),
                    ("OPENCODE_DB".into(), String::new()),
                ],
                watch_paths: Vec::new(),
            };
            watch_specs_for(&config)
        };
        let target_dir = std::fs::canonicalize(&moved).unwrap();
        // The file it leads to is there, and when it is not yet: SQLite
        // makes it, and its sidecars, where the link leads.
        for target_exists in [true, false] {
            let specs = watched(target_exists);
            assert!(specs.iter().any(|spec| spec.dir == data), "{specs:?}");
            let target = specs
                .iter()
                .find(|spec| spec.dir == target_dir)
                .unwrap_or_else(|| panic!("the target is not watched: {specs:?}"));
            assert_eq!(
                target.filenames,
                ["real.db", "real.db-wal", "real.db-journal", "real.db-shm"]
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_database_link_is_followed_as_the_system_follows_it() {
        let root = std::env::temp_dir().join(format!("lever-db-links-{}", sys::random_hex(4)));
        let real = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(root.file_name().unwrap());
        let watched_dirs = |data_home: &Path| -> Vec<(PathBuf, Vec<String>)> {
            let config = HarnessConfig {
                id: "opencode".into(),
                name: "OpenCode".into(),
                program: "opencode".into(),
                args: vec!["acp".into()],
                env: vec![
                    ("XDG_DATA_HOME".into(), data_home.to_string_lossy().into_owned()),
                    ("OPENCODE_DB".into(), String::new()),
                ],
                watch_paths: Vec::new(),
            };
            watch_specs_for(&config)
                .into_iter()
                .map(|spec| (spec.dir, spec.filenames))
                .collect()
        };

        // The data folder is a link, and the database a relative link out of
        // it to a file not made yet: `..` is taken from where the link
        // really is.
        std::fs::create_dir_all(root.join("disk/oc")).unwrap();
        std::fs::create_dir_all(root.join("disk/shared")).unwrap();
        std::fs::create_dir_all(root.join("home/share")).unwrap();
        std::os::unix::fs::symlink(root.join("disk/oc"), root.join("home/share/opencode")).unwrap();
        std::os::unix::fs::symlink("../shared/real.db", root.join("disk/oc/opencode.db")).unwrap();
        let specs = watched_dirs(&root.join("home/share"));
        assert!(
            specs
                .iter()
                .any(|(dir, names)| *dir == real.join("disk/shared") && names[0] == "real.db"),
            "{specs:?}"
        );

        // A link to a link to a file not made yet is followed to the end.
        std::fs::create_dir_all(root.join("chain/opencode")).unwrap();
        std::fs::create_dir_all(root.join("chain/middle")).unwrap();
        std::fs::create_dir_all(root.join("chain/end")).unwrap();
        std::os::unix::fs::symlink(root.join("chain/end/last.db"), root.join("chain/middle/next.db")).unwrap();
        std::os::unix::fs::symlink(
            root.join("chain/middle/next.db"),
            root.join("chain/opencode/opencode.db"),
        )
        .unwrap();
        let specs = watched_dirs(&root.join("chain"));
        assert!(
            specs
                .iter()
                .any(|(dir, names)| *dir == real.join("chain/end") && names[0] == "last.db"),
            "{specs:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
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
            struct NoopLister;
            impl SessionDiscoverer for NoopLister {
                fn watch_specs(&self) -> Vec<WatchSpec> {
                    Vec::new()
                }
                fn has_store_data(&self) -> bool {
                    false
                }
                fn list_sessions(&self) -> Result<Listing, String> {
                    Ok(Listing::default())
                }
            }
            let watch_specs = watch_specs_for(&config);
            OpenCodeSessionDiscoverer::with_cli_timeout(config, watch_specs, Box::new(NoopLister), timeout)
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
        let mut known = KnownSessions {
            owned: ["ses_known".to_string()].into(),
            ..KnownSessions::default()
        };

        let found = discoverer.classify_sessions(sessions(first), &known);
        assert!(found.changed_after);
        let kept_ids = by_id(&found.sessions);
        assert_eq!(kept_ids.get("ses_root"), Some(&false));
        let root = found
            .sessions
            .iter()
            .find(|s| s.harness_session_id == "ses_root")
            .unwrap();
        assert_eq!(
            root.title.as_deref(),
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
        // Its export failed: neither a thread nor a subagent.
        assert!(!kept_ids.contains_key("ses_unknown"));
        assert!(!fake.calls().iter().any(|c| c.ends_with("ses_known")));
        let mut quiet: Vec<_> = found.quiet.iter().map(|(sid, _)| sid.as_str()).collect();
        quiet.sort_unstable();
        assert_eq!(quiet, ["ses_archived", "ses_empty", "ses_renamed"]);

        // Lever keeps what was quiet; a failed export waits before it is
        // tried again.
        known.quiet = found.quiet.into_iter().collect();
        known.owned.extend(["ses_root".to_string()]);
        known.ignored.extend(["ses_child".to_string()]);
        let calls_before = fake.calls().len();
        let again = discoverer.classify_sessions(sessions(first), &known);
        assert!(fake.calls()[calls_before..].is_empty(), "{:?}", fake.calls());
        assert!(!again.changed_after, "nothing was exported");
        let again_ids = by_id(&again.sessions);
        for quiet in ["ses_empty", "ses_archived", "ses_renamed"] {
            assert!(!again_ids.contains_key(quiet), "{quiet} was handed on as a thread");
        }
        assert!(again.retry_at.is_some(), "the failed export's retry is asked for");

        // Its first message comes within the same second: a new stamp all the
        // same, so it is looked at again.
        fake.write(
            "export-ses_empty.json",
            r#"{"info":{"id":"ses_empty"},"messages":[{}]}"#,
        );
        let found = discoverer.classify_sessions(sessions("2026-01-01T00:00:00.500Z"), &known);
        assert_eq!(by_id(&found.sessions).get("ses_empty"), Some(&false));
    }

    #[test]
    fn a_classification_command_that_hangs_is_ended_and_tried_again_later() {
        let fake = FakeOpenCode::new();
        fake.write("export-ses_slow.json", r#"{"info":{"id":"ses_slow"},"messages":[{}]}"#);
        fake.write("hang-ses_slow", "");
        let discoverer = fake.discoverer(Duration::from_millis(300));
        let session = || vec![fake.session("ses_slow", "2026-01-01T00:00:01Z")];
        let started = Instant::now();
        let found = discoverer.classify_sessions(session(), &KnownSessions::default());
        assert!(found.sessions.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );

        std::fs::remove_file(fake.dir.join("hang-ses_slow")).unwrap();
        // Not straight away: it waits before it is tried again.
        let found = discoverer.classify_sessions(session(), &KnownSessions::default());
        assert!(found.sessions.is_empty() && !found.changed_after);
        assert!(found.retry_at.is_some());
        // Once the harness has changed it, it is tried at once.
        let changed = vec![fake.session("ses_slow", "2026-01-01T00:00:02Z")];
        let found = discoverer.classify_sessions(changed, &KnownSessions::default());
        assert_eq!(by_id(&found.sessions).get("ses_slow"), Some(&false));
    }

    #[test]
    fn a_session_the_harness_changed_starts_its_failures_again() {
        let fake = FakeOpenCode::new();
        let discoverer = fake.discoverer(Duration::from_secs(5));
        // No export answers for it.
        let found = discoverer.classify_sessions(
            vec![fake.session("ses_bad", "2026-01-01T00:00:01Z")],
            &KnownSessions::default(),
        );
        assert!(found.sessions.is_empty());
        discoverer.failures().get_mut("ses_bad").unwrap().retry = Instant::now();
        discoverer.classify_sessions(
            vec![fake.session("ses_bad", "2026-01-01T00:00:01Z")],
            &KnownSessions::default(),
        );
        assert_eq!(discoverer.failures()["ses_bad"].count, 2);
        discoverer.classify_sessions(
            vec![fake.session("ses_bad", "2026-01-01T00:00:02Z")],
            &KnownSessions::default(),
        );
        assert_eq!(discoverer.failures()["ses_bad"].count, 1, "its wait is short again");
    }

    #[test]
    fn the_newest_session_is_classified_first_and_the_rest_wait_for_the_next_pass() {
        let fake = FakeOpenCode::new();
        for id in ["ses_old", "ses_new"] {
            fake.write(
                &format!("export-{id}.json"),
                &format!(r#"{{"info":{{"id":"{id}"}},"messages":[{{}}]}}"#),
            );
        }
        // The old one's export would take far longer than the pass has.
        fake.write("hang-ses_old", "");
        let discoverer = fake.discoverer(Duration::from_secs(10));
        let sessions = vec![
            fake.session("ses_old", "2026-01-01T00:00:01Z"),
            fake.session("ses_new", "2026-01-02T00:00:01Z"),
            fake.session("ses_older", "2025-01-01T00:00:01Z"),
        ];
        let started = Instant::now();
        let found = discoverer.classify_sessions(sessions, &KnownSessions::default());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(by_id(&found.sessions).get("ses_new"), Some(&false));
        assert_eq!(fake.calls().first().map(String::as_str), Some("export ses_new"));
        assert!(found.more, "what is left waits for the next pass");
        assert!(!fake.calls().iter().any(|c| c.ends_with("ses_older")));
        assert!(
            discoverer.failures().get("ses_old").is_none(),
            "running out of the pass's time is no failure"
        );
    }
}
