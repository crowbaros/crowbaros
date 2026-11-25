//! Talking with the Agent: one turn and out, or a conversation a line at a
//! time. The answer is plain text on standard output as it comes, so it
//! reads the same in foot, on a console, over SSH, and down a pipe. All else
//! the client says goes to standard error.

use crate::connection::{Connection, Happening, Typing, refused, text};
use serde_json::{Value, json};
use std::io::{ErrorKind, IsTerminal, Result, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// How a turn ended, as the client's exit status says it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ended {
    Done,
    /// The Model gave no answer, or there is no Model to ask.
    Failed,
    /// The person stopped it.
    Stopped,
    /// The Agent was still at work on the message before it.
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
}

/// Sends one message of the person's, and writes the answer as it comes,
/// until the turn ends. With no conversation yet, the message starts one,
/// and `conversation` is then its ID.
fn turn(harness: &mut Connection, conversation: &mut Option<String>, message: &str) -> Result<Ended> {
    let reply = harness
        .request(json!({ "op": "send", "conversation": conversation, "text": message }))
        .inspect_err(|error| {
            // Ctrl-C before the Harness said the turn began. Leaving stops no turn, and this one may yet begin.
            if error.kind() == ErrorKind::Interrupted {
                eprintln!(
                    "crowbar-agent: Left before the Harness answered. The message was sent, and the Agent may still answer it: `crowbar-agent --list` shows the conversation if it began one, and `--resume` what became of it."
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
    let begun = text(&reply, "conversation").to_string();
    let ended = watch(
        harness,
        &begun,
        reply.get("turn").and_then(Value::as_u64),
        Answer::new(),
        Typing::Waits,
    );
    *conversation = Some(begun);
    // The person's typing waits, so the turn was watched to its end.
    ended.map(|ended| ended.unwrap_or(Ended::Done))
}

/// Writes a turn's answer as it comes, until the turn ends. Ctrl-C stops
/// the turn: the Harness is asked to, and the turn's own end is waited for.
/// A second Ctrl-C does not wait.
///
/// `typing` is `Waits`, or `UntilItEnds` for a turn the person did not
/// begin: the end of their typing then leaves the turn to go on, and gives
/// nothing. Once the person has asked for the turn to stop, its end is
/// waited for whatever they type.
fn watch(
    harness: &mut Connection,
    conversation: &str,
    turn: Option<u64>,
    mut answer: Answer,
    typing: Typing,
) -> Result<Option<Ended>> {
    let mut stopping = false;
    loop {
        let event = match harness.next(if stopping { Typing::Waits } else { typing })? {
            Happening::Event(event) => event,
            Happening::Interrupt if stopping => return Ok(Some(Ended::Stopped)),
            Happening::Interrupt => {
                stopping = true;
                // By its number, so a stop that comes after this turn ended does not reach the one after it.
                harness.send(json!({ "op": "stop", "conversation": conversation, "turn": turn }))?;
                continue;
            }
            Happening::EndOfInput => {
                answer.end()?;
                return Ok(None);
            }
            Happening::Line(_) => continue,
        };
        if event.get("turn").and_then(Value::as_u64) != turn {
            continue;
        }
        match &*text(&event, "event") {
            "text" => answer.write(&text(&event, "text"))?,
            "start-over" => {
                answer.end()?;
                eprintln!("The Model is asked again, and its answer starts over.");
            }
            "retry" => {
                answer.end()?;
                let seconds = event.get("seconds").and_then(Value::as_u64).unwrap_or(0);
                eprintln!(
                    "The Model did not answer: {}. Trying again in {seconds} seconds.",
                    text(&event, "message")
                );
            }
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
                eprintln!("crowbar-agent: The Model gave no answer: {}", text(&event, "message"));
                return Ok(Some(Ended::Failed));
            }
            _ => {}
        }
    }
}

/// A conversation that was resumed, as it stood.
struct Resumed {
    /// The turn that is still working, if one is.
    working: Option<u64>,
    /// The answer as far as it was written.
    answer: Answer,
    /// How its newest turn ended, if it has.
    last: Ended,
}

/// Follows a conversation that is already there, and shows it as it
/// stands, when `shown`. Nothing when there is no such conversation.
fn resume(harness: &mut Connection, conversation: &str, shown: bool) -> Result<Option<Resumed>> {
    let reply = harness.request(json!({ "op": "resume", "conversation": conversation }))?;
    if refused(&reply).is_some() {
        return Ok(None);
    }
    let turns = reply
        .get("turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut answer = Answer::new();
    if shown {
        if reply.get("earlier") == Some(&Value::Bool(true)) {
            eprintln!("(earlier turns are not shown)");
        }
        for turn in &turns {
            answer.write(&format!("> {}\n", text(turn, "message")))?;
            answer.write(&text(turn, "answer"))?;
            match &*text(turn, "state") {
                // Its answer goes on from here.
                "working" => break,
                "stopped" => {
                    answer.end()?;
                    eprintln!("(stopped)");
                }
                "failed" => {
                    answer.end()?;
                    eprintln!("(the Model gave no answer: {})", text(turn, "error"));
                }
                _ => answer.end()?,
            }
        }
    }
    let newest = turns.last().map(|turn| text(turn, "state")).unwrap_or_default();
    Ok(Some(Resumed {
        working: turns
            .last()
            .filter(|_| newest == "working")
            .and_then(|turn| turn.get("turn").and_then(Value::as_u64)),
        answer,
        last: if newest == "failed" { Ended::Failed } else { Ended::Done },
    }))
}

/// `crowbar-agent [--resume ID] "<request>"`: one turn, and out.
pub fn once(harness: &mut Connection, resumed: Option<&str>, message: &str) -> Result<Ended> {
    let mut conversation = None;
    if let Some(resumed) = resumed {
        match resume(harness, resumed, false)? {
            Some(_) => conversation = Some(resumed.to_string()),
            None => return Ok(Ended::Failed),
        }
    }
    turn(harness, &mut conversation, message)
}

/// How a conversation the person left ends: as a failure when its newest turn got no answer.
fn left(last: Ended) -> Ended {
    if last == Ended::Failed {
        Ended::Failed
    } else {
        Ended::Done
    }
}

/// `crowbar-agent [--resume ID]`: a conversation, a line at a time. A
/// conversation starts with its first message, so leaving at once leaves
/// none behind. Ctrl-D leaves, and so does Ctrl-C between turns. It ends as
/// a failure when its newest turn got no answer.
pub fn repl(harness: &mut Connection, resumed: Option<&str>) -> Result<Ended> {
    let prompted = std::io::stdin().is_terminal();
    let mut conversation = None;
    // How the newest turn ended, which is how the client ends: a failed turn
    // at the end of what was piped in is a failure, and so is one that had
    // failed before the conversation was resumed.
    let mut last = Ended::Done;
    if let Some(resumed) = resumed {
        let Some(stood) = resume(harness, resumed, true)? else {
            return Ok(Ended::Failed);
        };
        last = stood.last;
        // A turn another client left working is this client's to watch, and to stop.
        if stood.working.is_some() {
            last = watch(harness, resumed, stood.working, stood.answer, Typing::Waits)?.unwrap_or(last);
        }
        conversation = Some(resumed.to_string());
    }
    loop {
        if prompted {
            eprint!("> ");
        }
        let line = loop {
            match harness.next(Typing::Wanted)? {
                Happening::Line(line) => break line,
                Happening::EndOfInput | Happening::Interrupt => {
                    if prompted {
                        eprintln!();
                    }
                    return Ok(left(last));
                }
                // What another client of this conversation says begins a turn
                // this client shows, so it is this client's to watch, and
                // Ctrl-C stops it, as it stops any turn on the screen. Ctrl-D
                // leaves, as it does at the prompt, and the turn goes on. What
                // is left of a turn this client stopped watching is not shown.
                Happening::Event(event) if text(&event, "event") == "message" => {
                    let mut answer = Answer::new();
                    answer.write(&format!("\n> {}\n", text(&event, "text")))?;
                    let turn = event.get("turn").and_then(Value::as_u64);
                    let watched = watch(
                        harness,
                        &text(&event, "conversation"),
                        turn,
                        answer,
                        Typing::UntilItEnds,
                    )?;
                    // Ctrl-D while it was shown: the client leaves as it does at the prompt, on a line the answer has ended.
                    let Some(ended) = watched else {
                        return Ok(left(last));
                    };
                    last = ended;
                    if prompted {
                        eprint!("> ");
                    }
                }
                Happening::Event(_) => {}
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        last = turn(harness, &mut conversation, &line)?;
    }
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

/// `crowbar-agent --list`: each conversation on a line, the newest first.
pub fn list(harness: &mut Connection) -> Result<Ended> {
    let reply = harness.request(json!({ "op": "list" }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    let conversations = reply
        .get("conversations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if conversations.is_empty() {
        eprintln!("No conversations yet.");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.as_secs())
        .unwrap_or(0);
    for conversation in &conversations {
        let started = conversation.get("started").and_then(Value::as_u64).unwrap_or(now);
        let turns = conversation.get("turns").and_then(Value::as_u64).unwrap_or(0);
        let working = if conversation.get("working") == Some(&Value::Bool(true)) {
            "  (working)"
        } else {
            ""
        };
        println!(
            "{}  {:<14}  {turns} turn{}  {}{working}",
            text(conversation, "conversation"),
            ago(now.saturating_sub(started)),
            if turns == 1 { " " } else { "s" },
            text(conversation, "title"),
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
