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

use serde_json::{Map, Value};
use std::path::PathBuf;

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
/// then the person's own rules for it, in their order. A rule of theirs that
/// allows everything is the one Ask first takes the place of, and asks.
fn rules_for(documents: &[Value], keys: &[&str]) -> Value {
    let mut rules: Vec<(String, String)> = Vec::new();
    for document in documents {
        let Some(permission) = document.get("permission") else {
            continue;
        };
        // `permission` is one rule for everything, or a rule or rules for
        // each permission by its name.
        if let Some(all) = permission.as_str() {
            rules.push(("*".to_string(), all.to_string()));
            continue;
        }
        let Some(by_name) = permission.as_object() else {
            continue;
        };
        for (name, rule) in by_name {
            if !keys.contains(&name.as_str()) {
                continue;
            }
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

/// The Ask first agent's `permission`: it asks before a command or an edit,
/// except where the person's own rules say otherwise.
pub fn ask_first_permission(documents: &[Value]) -> Value {
    let mut permission = Map::new();
    permission.insert("bash".to_string(), rules_for(documents, &["bash", "shell"]));
    permission.insert("edit".to_string(), rules_for(documents, &["edit"]));
    Value::Object(permission)
}

/// Reads JSON with comments and trailing commas, as OpenCode's settings may
/// have them. What is not an object is not settings.
pub fn parse_jsonc(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(&strip_jsonc(text)).ok()?;
    value.is_object().then_some(value)
}

/// JSON with its comments taken out, and any comma before a closing bracket.
fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // A string goes through as it is, escapes and all.
            out.push(c);
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
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
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
}
