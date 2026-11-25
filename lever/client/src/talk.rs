//! Talking with the Agent through Lever: one turn and out, a thread a line at
//! a time, listing, stopping, and checking status.
//!
//! The answer is plain text on standard output as it comes, so it reads the
//! same in foot, on a console, over SSH, and down a pipe. Acts, unrecognized
//! updates, prompts, and errors go to standard error.

use crate::connection::{Connection, Happening, Typing, refused, text};
use serde_json::{Value, json};
use std::io::{ErrorKind, IsTerminal, Result, Write, stderr};
use std::time::{SystemTime, UNIX_EPOCH};

/// How a turn ended, as the client's exit status says it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ended {
    Done,
    /// The Agent gave no answer, the turn was interrupted, or no harness is available.
    Failed,
    /// The person stopped it.
    Stopped,
    /// This thread was still working on the turn before it.
    Refused,
}

/// The answer as it is written out, which knows whether its last line has ended.
struct Answer {
    out: std::io::Stdout,
    open: bool,
}

impl Answer {
    fn new() -> Answer {
        Answer {
            out: std::io::stdout(),
            open: false,
        }
    }

    fn write(&mut self, piece: &str) -> Result<()> {
        if let Some(last) = piece.chars().last() {
            self.open = last != '\n';
            self.out.write_all(piece.as_bytes())?;
            self.out.flush()?;
        }
        Ok(())
    }

    /// Ends the line the answer left open, so what is said next starts its own.
    fn end(&mut self) -> Result<()> {
        if self.open { self.write("\n") } else { Ok(()) }
    }

    /// Makes room for a line on standard error in the middle of the answer.
    /// Only where the two are one terminal does that line land in the
    /// answer's open line. Down a pipe or into a file nothing is put in the
    /// middle of the answer.
    fn make_way(&mut self) -> Result<()> {
        if self.out.is_terminal() && stderr().is_terminal() {
            self.end()
        } else {
            Ok(())
        }
    }
}

fn current_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .filter(|p| p.is_absolute())
        .and_then(|p| p.to_str().map(str::to_string))
}

/// Prints one act to standard error with control characters neutralized.
fn show_act(answer: &mut Answer, act: &Value) -> Result<()> {
    answer.make_way()?;
    let title = text(act, "title");
    let state = text(act, "state");
    let detail = text(act, "detail");
    if detail.is_empty() {
        eprintln!("[act] {title} ({state})");
    } else {
        eprintln!("[act] {title} ({state}): {detail}");
    }
    Ok(())
}

/// Prints one unrecognized update plainly to standard error.
fn show_note(answer: &mut Answer, note: &Value) -> Result<()> {
    answer.make_way()?;
    eprintln!("[update] {}", text(note, "text"));
    Ok(())
}

/// Sends one message of the person's, and writes the answer as it comes, until
/// the turn ends. With no thread yet, the message starts one, and `thread` is
/// then its ID.
fn turn(lever: &mut Connection, thread: &mut Option<String>, message: &str) -> Result<Ended> {
    let mut req = json!({ "op": "send", "thread": thread, "text": message });
    if thread.is_none()
        && let Some(cwd) = current_cwd()
    {
        req["cwd"] = json!(cwd);
    }
    let reply = lever.request(req).inspect_err(|error| {
        // Ctrl-C before Lever said the turn began. Leaving stops no turn, and this one may yet begin.
        if error.kind() == ErrorKind::Interrupted {
            eprintln!(
                "crowbar-agent: Left before Lever answered. The message was sent, and the Agent may still answer it: `crowbar-agent --list` shows the thread if it began one, and `--resume` what became of it."
            );
        }
    })?;
    if let Some(code) = refused(&reply) {
        return Ok(if code == "working" {
            Ended::Refused
        } else {
            Ended::Failed
        });
    }
    let begun = text(&reply, "thread").to_string();
    let ended = watch(
        lever,
        &begun,
        reply.get("turn").and_then(Value::as_u64),
        Answer::new(),
        Typing::Waits,
    );
    *thread = Some(begun);
    ended.map(|ended| ended.unwrap_or(Ended::Done))
}

/// Writes a turn's answer and acts as they come, until the turn ends. Ctrl-C
/// stops the turn: Lever is asked to, and the turn's own end is waited for. A
/// second Ctrl-C does not wait.
fn watch(
    lever: &mut Connection,
    thread: &str,
    turn: Option<u64>,
    mut answer: Answer,
    typing: Typing,
) -> Result<Option<Ended>> {
    let mut stopping = false;
    loop {
        let event = match lever.next(if stopping { Typing::Waits } else { typing })? {
            Happening::Event(event) => event,
            Happening::Interrupt if stopping => return Ok(Some(Ended::Stopped)),
            Happening::Interrupt => {
                stopping = true;
                // By its number, so a stop that comes after this turn ended does not reach the one after it.
                lever.send(json!({ "op": "stop", "thread": thread, "turn": turn }))?;
                continue;
            }
            Happening::EndOfInput => {
                answer.end()?;
                return Ok(None);
            }
            Happening::Line(_) => continue,
        };
        if event.get("thread").and_then(Value::as_str) != Some(thread) {
            continue;
        }
        // An act is its own turn's, and may end after a later turn began:
        // the thread's acts are shown whichever turn is watched.
        if text(&event, "event") == "act" {
            show_act(&mut answer, &event)?;
            continue;
        }
        if event.get("turn").and_then(Value::as_u64) != turn {
            continue;
        }
        match &*text(&event, "event") {
            "text" => answer.write(&text(&event, "text"))?,
            "note" => show_note(&mut answer, &event)?,
            "done" => {
                answer.end()?;
                return Ok(Some(Ended::Done));
            }
            "stopped" => {
                answer.end()?;
                eprintln!("Stopped.");
                return Ok(Some(Ended::Stopped));
            }
            "failed" => {
                answer.end()?;
                eprintln!("crowbar-agent: The Agent gave no answer: {}", text(&event, "message"));
                return Ok(Some(Ended::Failed));
            }
            "interrupted" => {
                answer.end()?;
                eprintln!("Interrupted.");
                return Ok(Some(Ended::Failed));
            }
            _ => {}
        }
    }
}

/// A thread that was resumed, as it stood.
struct Resumed {
    /// The turn that is still working, if one is.
    working: Option<u64>,
    /// The answer as far as it was written.
    answer: Answer,
    /// How its newest turn ended, if it has.
    last: Ended,
}

/// Follows a thread that is already there, and shows it as it stands.
/// Nothing when there is no such thread.
fn resume(lever: &mut Connection, thread: &str) -> Result<Option<Resumed>> {
    let reply = lever.request(json!({ "op": "resume", "thread": thread }))?;
    if refused(&reply).is_some() {
        return Ok(None);
    }
    let turns = reply
        .get("turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut answer = Answer::new();
    for turn in &turns {
        answer.write(&format!("> {}\n", text(turn, "message")))?;
        for act in turn.get("acts").and_then(Value::as_array).into_iter().flatten() {
            show_act(&mut answer, act)?;
        }
        for note in turn.get("notes").and_then(Value::as_array).into_iter().flatten() {
            show_note(&mut answer, note)?;
        }
        answer.write(&text(turn, "answer"))?;
        match &*text(turn, "state") {
            // Its answer goes on from here.
            "working" => break,
            "stopped" => {
                answer.end()?;
                eprintln!("(stopped)");
            }
            "interrupted" => {
                answer.end()?;
                eprintln!("(interrupted)");
            }
            "failed" => {
                answer.end()?;
                eprintln!("(the Agent gave no answer: {})", text(turn, "error"));
            }
            _ => answer.end()?,
        }
    }
    let newest = turns.last().map(|turn| text(turn, "state")).unwrap_or_default();
    Ok(Some(Resumed {
        working: turns
            .last()
            .filter(|_| newest == "working")
            .and_then(|turn| turn.get("turn").and_then(Value::as_u64)),
        answer,
        last: if matches!(&*newest, "failed" | "interrupted") {
            Ended::Failed
        } else {
            Ended::Done
        },
    }))
}

/// `crowbar-agent [--resume ID] "<request>"`: one turn, and out.
pub fn once(lever: &mut Connection, resumed: Option<&str>, message: &str) -> Result<Ended> {
    // Lever says so itself when there is no such thread, so the thread's
    // record is not asked for only to be dropped.
    let mut thread = resumed.map(str::to_string);
    turn(lever, &mut thread, message)
}

/// How a thread the person left ends: as a failure when its newest turn got no answer.
fn left(last: Ended) -> Ended {
    if last == Ended::Failed {
        Ended::Failed
    } else {
        Ended::Done
    }
}

/// `crowbar-agent [--resume ID]`: a thread, a line at a time. A thread starts
/// with its first message, so leaving at once leaves none behind. Ctrl-D
/// leaves, and so does Ctrl-C between turns.
pub fn repl(lever: &mut Connection, resumed: Option<&str>) -> Result<Ended> {
    let prompted = std::io::stdin().is_terminal();
    let mut thread = None;
    let mut last = Ended::Done;
    if let Some(resumed) = resumed {
        let Some(stood) = resume(lever, resumed)? else {
            return Ok(Ended::Failed);
        };
        last = stood.last;
        if stood.working.is_some() {
            // Ctrl-D leaves a turn that was working when the thread was
            // resumed, as it leaves one that begins after.
            let Some(ended) = watch(lever, resumed, stood.working, stood.answer, Typing::UntilItEnds)? else {
                return Ok(left(last));
            };
            last = ended;
        }
        thread = Some(resumed.to_string());
    }
    loop {
        if prompted {
            eprint!("> ");
        }
        let line = loop {
            match lever.next(Typing::Wanted)? {
                Happening::Line(line) => break line,
                Happening::EndOfInput | Happening::Interrupt => {
                    if prompted {
                        eprintln!();
                    }
                    return Ok(left(last));
                }
                Happening::Event(event) if text(&event, "event") == "message" => {
                    let mut answer = Answer::new();
                    answer.write(&format!("\n> {}\n", text(&event, "text")))?;
                    let turn = event.get("turn").and_then(Value::as_u64);
                    let watched = watch(lever, &text(&event, "thread"), turn, answer, Typing::UntilItEnds)?;
                    let Some(ended) = watched else {
                        return Ok(left(last));
                    };
                    last = ended;
                    if prompted {
                        eprint!("> ");
                    }
                }
                Happening::Event(event) if text(&event, "event") == "text" => {
                    let mut answer = Answer::new();
                    answer.write(&text(&event, "text"))?;
                    answer.end()?;
                }
                Happening::Event(event) if text(&event, "event") == "note" => {
                    let mut answer = Answer::new();
                    show_note(&mut answer, &event)?;
                }
                Happening::Event(event) if text(&event, "event") == "act" => {
                    let mut answer = Answer::new();
                    show_act(&mut answer, &event)?;
                }
                Happening::Event(_) => {}
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        last = turn(lever, &mut thread, &line)?;
    }
}

/// `crowbar-agent --stop ID`: stop a working turn in a thread.
pub fn stop(lever: &mut Connection, thread: &str) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "stop", "thread": thread }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    // Lever has taken the request. The turn's own end, which `--resume` and
    // `--list` show, says whether it stopped.
    eprintln!("Asked the Agent to stop.");
    Ok(Ended::Done)
}

/// `crowbar-agent status`: Lever's version and configured harnesses.
pub fn status(lever: &mut Connection) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "status" }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    let version = text(&reply, "version");
    let harnesses = reply
        .get("harnesses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if harnesses.is_empty() {
        println!("Lever {version} is running with no harness available.");
        return Ok(Ended::Failed);
    }
    let names: Vec<String> = harnesses
        .iter()
        .map(|h| text(h, "name").into_owned())
        .filter(|n| !n.is_empty())
        .collect();
    let running = if reply.get("harness_started") == Some(&Value::Bool(true)) {
        "running"
    } else {
        "idle"
    };
    println!("Lever {version}: {} ({running}).", names.join(", "));
    Ok(Ended::Done)
}

/// How long ago a time was, in words.
fn ago(seconds: u64) -> String {
    let count = |count: u64, unit: &str| format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" });
    match seconds {
        0..60 => "just now".to_string(),
        60..3600 => count(seconds / 60, "minute"),
        3600..86400 => count(seconds / 3600, "hour"),
        _ => count(seconds / 86400, "day"),
    }
}

/// `crowbar-agent --list`: each thread on a line, the newest first.
pub fn list(lever: &mut Connection) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "list" }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    let threads = reply
        .get("threads")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if threads.is_empty() {
        eprintln!("No threads yet.");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.as_secs())
        .unwrap_or(0);
    for thread in &threads {
        let updated = thread
            .get("updated")
            .or_else(|| thread.get("started"))
            .and_then(Value::as_u64)
            .unwrap_or(now);
        let turns = thread.get("turns").and_then(Value::as_u64).unwrap_or(0);
        let state = text(thread, "state");
        let badge = match &*state {
            "working" => "  (working)",
            "failed" => "  (failed)",
            "stopped" => "  (stopped)",
            "interrupted" => "  (interrupted)",
            _ => "",
        };
        println!(
            "{}  {:<14}  {turns} turn{}  {}{badge}",
            text(thread, "thread"),
            ago(now.saturating_sub(updated)),
            if turns == 1 { " " } else { "s" },
            text(thread, "title"),
        );
    }
    Ok(Ended::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_is_said_in_the_largest_unit_that_fits() {
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(7199), "1 hour ago");
        assert_eq!(ago(7200), "2 hours ago");
        assert_eq!(ago(3 * 86400 + 5), "3 days ago");
    }
}
