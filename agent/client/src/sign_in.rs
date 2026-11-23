//! Signing in to a Model, signing out, and which Model the Agent uses. The
//! Harness takes every step of a sign-in and keeps the credentials. The
//! client only relays: it shows the Harness's questions, sends back what
//! the person types, and opens a browser when there is one.

use crate::connection::{Connection, Happening, NO_MODEL, refused, text};
use crate::sys;
use crate::talk::Ended;
use serde_json::{Value, json};
use std::io::Result;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Says a reply that is an error, and whether it was one.
fn is_refused(reply: &Value) -> bool {
    refused(reply).is_some()
}

/// How long an opener is given to say it could not open the address.
const OPENER_FAILS_WITHIN: Duration = Duration::from_millis(500);

/// Opens an address in the person's browser, when the client runs on a
/// desktop, and says whether it asked one to. On a console or over SSH
/// there is none, and the address is the person's to open elsewhere. Only
/// an address of the web is opened: the address is a provider's, and an
/// opener does with any other kind whatever the desktop has for it.
fn open_browser(url: &str) -> bool {
    if ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .all(|name| std::env::var_os(name).is_none_or(|value| value.is_empty()))
    {
        return false;
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return false;
    }
    [("xdg-open", vec![url]), ("gio", vec!["open", url])]
        .iter()
        .any(|(program, args)| {
            let opener = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                // A group of its own: Ctrl-C at the client's prompt is not for the browser.
                .process_group(0)
                .spawn();
            let Ok(mut opener) = opener else { return false };
            // An opener that cannot open the address says so at once. One
            // still running after a moment is the browser itself, which
            // some openers become: it is left to run, since waiting for it
            // would hold the sign-in until the person closed the browser.
            let patience = Instant::now() + OPENER_FAILS_WITHIN;
            loop {
                match opener.try_wait() {
                    Ok(Some(status)) => return status.success(),
                    Ok(None) if Instant::now() < patience => std::thread::sleep(Duration::from_millis(10)),
                    Ok(None) => {
                        std::thread::spawn(move || opener.wait());
                        return true;
                    }
                    Err(_) => return false,
                }
            }
        })
}

/// A question of the Harness's that waits for the person's answer.
struct Question {
    number: u64,
    /// What may be picked, when the question is a choice.
    options: Vec<String>,
    /// Held while a secret is typed, so the terminal does not show it.
    hidden: Option<sys::Hidden>,
}

/// The option a person's answer picks: its number in the list, or its own
/// ID.
fn picked<'a>(options: &'a [String], answer: &str) -> Option<&'a str> {
    let answer = answer.trim();
    let by_number = answer
        .parse::<usize>()
        .ok()
        .and_then(|number| options.get(number.checked_sub(1)?));
    by_number
        .or_else(|| options.iter().find(|option| *option == answer))
        .map(String::as_str)
}

fn show(event: &Value) -> Question {
    let options: Vec<&Value> = event
        .get("options")
        .and_then(Value::as_array)
        .map(|list| list.iter().collect())
        .unwrap_or_default();
    eprintln!("{}", text(event, "message"));
    for (index, option) in options.iter().enumerate() {
        let (id, label) = (text(option, "id"), text(option, "label"));
        let named = if label.is_empty() || label == id {
            id.to_string()
        } else {
            format!("{label} ({id})")
        };
        eprintln!("  {}. {named}", index + 1);
    }
    let kind = text(event, "kind");
    eprint!("{}", if kind == "select" { "Its number or name: " } else { "> " });
    Question {
        number: event.get("question").and_then(Value::as_u64).unwrap_or(0),
        options: options.iter().map(|option| text(option, "id").into_owned()).collect(),
        hidden: (kind == "secret").then(sys::Hidden::typing),
    }
}

/// `crowbar-agent login [PROVIDER]`.
pub fn login(harness: &mut Connection, provider: Option<&str>) -> Result<Ended> {
    if is_refused(&harness.request(json!({ "op": "login", "provider": provider }))?) {
        return Ok(Ended::Failed);
    }
    let mut asked: Option<Question> = None;
    let mut cancelling = false;
    loop {
        let event = match harness.next(asked.is_some())? {
            Happening::Event(event) => event,
            Happening::Line(line) => {
                let Some(question) = asked.take() else { continue };
                let answer = if question.options.is_empty() {
                    line
                } else if let Some(option) = picked(&question.options, &line) {
                    option.to_string()
                } else {
                    eprint!("Its number, from 1 to {}, or its name: ", question.options.len());
                    asked = Some(question);
                    continue;
                };
                if question.hidden.is_some() {
                    // The terminal shows typing again, and the line end the person typed, which it did not.
                    drop(question.hidden);
                    eprintln!();
                }
                harness.send(json!({ "op": "answer", "question": question.number, "text": answer }))?;
                continue;
            }
            Happening::EndOfInput | Happening::Interrupt => {
                asked = None;
                eprintln!();
                // A second one does not wait for a Harness that has not said the sign-in
                // is cancelled: it cancels the sign-in of a client that leaves.
                if cancelling {
                    return Ok(Ended::Stopped);
                }
                cancelling = true;
                harness.send(json!({ "op": "cancel" }))?;
                continue;
            }
        };
        match &*text(&event, "event") {
            "question" => asked = Some(show(&event)),
            "withdrawn" => {
                if asked
                    .as_ref()
                    .is_some_and(|question| Some(question.number) == event.get("question").and_then(Value::as_u64))
                {
                    asked = None;
                    eprintln!();
                }
            }
            "notice" => eprintln!("{}", text(&event, "message")),
            "browser" => {
                let url = text(&event, "url");
                if !text(&event, "message").is_empty() {
                    eprintln!("{}", text(&event, "message"));
                }
                if open_browser(&url) {
                    eprintln!("A browser was opened to sign in. If it shows nothing, open this address:\n\n  {url}\n");
                } else {
                    eprintln!("Open this address in a browser to sign in:\n\n  {url}\n");
                }
            }
            "code" => {
                let url = text(&event, "url");
                let opened = if open_browser(&url) {
                    "A browser was opened there."
                } else {
                    ""
                };
                eprintln!("Enter this code at {url}\n\n  {}\n\n{opened}", text(&event, "code"));
            }
            "signed-in" => {
                let model = match &*text(&event, "name") {
                    // Another provider's Model may be in use still, which `status` shows.
                    "" => " None of its Models is chosen: choose one with `crowbar-agent model`.".to_string(),
                    name => format!(" The Agent uses {name}."),
                };
                eprintln!("Signed in to {}.{model}", text(&event, "provider_name"));
                return Ok(Ended::Done);
            }
            "sign-in-failed" => {
                eprintln!("crowbar-agent: {}", text(&event, "message"));
                return Ok(if event.get("cancelled") == Some(&Value::Bool(true)) {
                    Ended::Stopped
                } else {
                    Ended::Failed
                });
            }
            _ => {}
        }
    }
}

/// `crowbar-agent logout [PROVIDER]`.
pub fn logout(harness: &mut Connection, provider: Option<&str>) -> Result<Ended> {
    let reply = harness.request(json!({ "op": "logout", "provider": provider }))?;
    if is_refused(&reply) {
        return Ok(Ended::Failed);
    }
    eprintln!("Signed out of {}.", text(&reply, "provider_name"));
    Ok(Ended::Done)
}

/// `crowbar-agent status`: the Model the Agent uses, and each sign-in.
/// Fails when the Agent has no Model to use.
pub fn status(harness: &mut Connection) -> Result<Ended> {
    let reply = harness.request(json!({ "op": "status" }))?;
    if is_refused(&reply) {
        return Ok(Ended::Failed);
    }
    for signed_in in reply.get("signed_in").and_then(Value::as_array).into_iter().flatten() {
        println!(
            "Signed in to {} ({}), with {}.",
            text(signed_in, "name"),
            text(signed_in, "provider"),
            article(&text(signed_in, "how"))
        );
    }
    match reply.get("model").filter(|model| !model.is_null()) {
        Some(model) => {
            println!(
                "The Agent uses {} ({}), from {}.",
                text(model, "name"),
                text(model, "model"),
                text(model, "provider_name")
            );
            Ok(Ended::Done)
        }
        None => {
            println!("{} {NO_MODEL}", text(&reply, "missing"));
            Ok(Ended::Failed)
        }
    }
}

fn article(how: &str) -> String {
    if how == "API key" {
        "an API key".to_string()
    } else {
        how.to_string()
    }
}

/// `crowbar-agent model [PROVIDER/MODEL]`: the Models the person is signed
/// in to, or the one of them the Agent is to use.
pub fn model(harness: &mut Connection, chosen: Option<&str>) -> Result<Ended> {
    let Some(chosen) = chosen else {
        let reply = harness.request(json!({ "op": "models" }))?;
        if is_refused(&reply) {
            return Ok(Ended::Failed);
        }
        let models = reply
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if models.is_empty() {
            eprintln!("No Model is signed in. Sign in to one with `crowbar-agent login`.");
            return Ok(Ended::Failed);
        }
        for model in &models {
            let mark = if model.get("in_use") == Some(&Value::Bool(true)) {
                "*"
            } else {
                " "
            };
            println!(
                "{mark} {}/{}  {}",
                text(model, "provider"),
                text(model, "model"),
                text(model, "name")
            );
        }
        return Ok(Ended::Done);
    };
    // A Model's own name may hold a slash; a provider's does not.
    let Some((provider, model)) = chosen.split_once('/') else {
        eprintln!("crowbar-agent: Name a Model as `crowbar-agent model` lists it: PROVIDER/MODEL.");
        return Ok(Ended::Failed);
    };
    let reply = harness.request(json!({ "op": "model", "provider": provider, "model": model }))?;
    if is_refused(&reply) {
        return Ok(Ended::Failed);
    }
    if reply.get("in_use") == Some(&Value::Bool(true)) {
        eprintln!(
            "The Agent uses {}, from {}.",
            text(&reply, "name"),
            text(&reply, "provider_name")
        );
    } else {
        // An endpoint file in the Harness's state is the Model in use for as long as it is there.
        eprintln!(
            "{} is chosen. The Agent goes on using the Model `crowbar-agent status` shows, until the endpoint file in the Harness's state is removed.",
            text(&reply, "name")
        );
    }
    Ok(Ended::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_option_is_picked_by_its_number_or_its_name() {
        let options = ["oauth".to_string(), "api_key".to_string(), "3".to_string()];
        assert_eq!(picked(&options, "1"), Some("oauth"));
        assert_eq!(picked(&options, " api_key\n"), Some("api_key"));
        // A number is a place in the list before it is a name.
        assert_eq!(picked(&options, "3"), Some("3"));
        assert_eq!(picked(&options, "0"), None);
        assert_eq!(picked(&options, "4"), None);
        assert_eq!(picked(&options, "browser"), None);
        assert_eq!(picked(&options, ""), None);
    }
}
