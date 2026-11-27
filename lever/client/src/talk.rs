//! Talking with the Agent through Lever: one turn and out, a thread a line at
//! a time, listing, stopping, and checking status.
//!
//! The answer is plain text on standard output as it comes, so it reads the
//! same in foot, on a console, over SSH, and down a pipe. Acts, unrecognized
//! updates, prompts, and errors go to standard error.

use crate::connection::{Connection, Happening, Typing, refused, text};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, IsTerminal, Result, Write, stderr};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode};
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

/// The answers a request offers, as Lever names them.
fn answers_of(request: &Value) -> Vec<String> {
    request
        .get("answers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|a| text(a, "answer").into_owned())
        .filter(|a| !a.is_empty())
        .collect()
}

/// Prints a subagent's status line to standard error.
fn show_subagent(answer: &mut Answer, subagent: &Value) -> Result<()> {
    answer.make_way()?;
    let title = text(subagent, "title");
    let state = text(subagent, "state");
    let error = text(subagent, "error");
    if error.is_empty() {
        eprintln!("[subagent] {title} ({state})");
    } else {
        eprintln!("[subagent] {title} ({state}): {error}");
    }
    Ok(())
}

/// Prints one subagent act to standard error.
fn show_subagent_act(answer: &mut Answer, subagent_title: Option<&str>, act: &Value) -> Result<()> {
    answer.make_way()?;
    let who = subagent_title.filter(|s| !s.is_empty()).unwrap_or("subagent");
    let title = text(act, "title");
    let state = text(act, "state");
    let detail = text(act, "detail");
    if detail.is_empty() {
        eprintln!("[subagent: {who}] [act] {title} ({state})");
    } else {
        eprintln!("[subagent: {who}] [act] {title} ({state}): {detail}");
    }
    Ok(())
}

/// Prints a subagent's answer text to standard error.
fn show_subagent_answer(answer: &mut Answer, subagent_title: Option<&str>, text_body: &str) -> Result<()> {
    let trimmed = text_body.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    answer.make_way()?;
    let who = subagent_title.filter(|s| !s.is_empty()).unwrap_or("subagent");
    let mut lines = trimmed.lines();
    eprintln!("[subagent: {who}] {}", lines.next().unwrap_or_default());
    for line in lines {
        eprintln!("  {line}");
    }
    Ok(())
}

/// Prints a request to standard error: what it asks to do, the exact command
/// or the file and the change, and, when it waits, how to answer it. With
/// `asking`, the answer is asked for here.
fn show_request(
    answer: &mut Answer,
    request: &Value,
    thread: &str,
    asking: bool,
    subagent_title: Option<&str>,
) -> Result<()> {
    answer.make_way()?;
    let tag = match (
        request
            .get("subagent")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
        subagent_title.filter(|s| !s.is_empty()),
    ) {
        (Some(_), Some(title)) => format!("[request ({title})]"),
        (Some(_), None) => "[request (subagent)]".to_string(),
        (None, _) => "[request]".to_string(),
    };
    let detail = text(request, "detail");
    match &*text(request, "state") {
        "waiting" => {
            if text(request, "kind") == "command" {
                let command = if detail.is_empty() {
                    text(request, "title")
                } else {
                    detail.clone()
                };
                let mut lines = command.lines();
                eprintln!("{tag} Run: {}", lines.next().unwrap_or_default());
                for line in lines {
                    eprintln!("  {line}");
                }
            } else {
                let what = match &*text(request, "kind") {
                    "edit" => "Edit",
                    "delete" => "Delete",
                    "move" => "Move",
                    "read" => "Read",
                    _ => "Act",
                };
                let path = text(request, "path");
                let named = if path.is_empty() { text(request, "title") } else { path };
                eprintln!("{tag} {what}: {named}");
                for line in detail.lines() {
                    eprintln!("  {line}");
                }
            }
            let answers = answers_of(request);
            if asking {
                eprint!("Answer [{}]: ", answers.join("/"));
                let _ = stderr().flush();
            } else {
                eprintln!(
                    "  Answer with: crowbar-agent --resume {thread} --answer {} <{}>",
                    text(request, "request"),
                    answers.join("|")
                );
            }
        }
        "answered" => eprintln!("{tag} answered: {}", text(request, "answer")),
        _ => eprintln!("{tag} withdrawn"),
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
fn turn(
    lever: &mut Connection,
    thread: &mut Option<String>,
    model: Option<&str>,
    mode: Option<&str>,
    message: &str,
    subagents: &mut Subagents,
) -> Result<Ended> {
    let mut req = json!({ "op": "send", "thread": thread, "text": message });
    if thread.is_none()
        && let Some(cwd) = current_cwd()
    {
        req["cwd"] = json!(cwd);
    }
    if let Some(model) = model {
        req["model"] = json!(model);
    }
    if let Some(mode) = mode {
        req["mode"] = json!(mode);
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
    if text(&reply, "reply") == "steered" {
        eprintln!("crowbar-agent: The Agent took your message into the turn it is working on.");
    }
    let begun = text(&reply, "thread").to_string();
    // How the turn ended, though the person leaves while a request of its waits.
    let mut last = Ended::Done;
    let ended = watch(
        lever,
        &begun,
        WatchStart {
            turn: reply.get("turn").and_then(Value::as_u64),
            answer: Answer::new(),
            typing: Typing::Waits,
            waiting: Vec::new(),
        },
        subagents,
        &mut last,
    );
    *thread = Some(begun);
    ended.map(|ended| ended.unwrap_or(last))
}

fn subagent_title_for_request<'a>(request: &Value, subagent_titles: &'a HashMap<String, String>) -> Option<&'a str> {
    request
        .get("subagent")
        .and_then(Value::as_str)
        .and_then(|id| subagent_titles.get(id))
        .map(String::as_str)
}

struct WatchStart {
    turn: Option<u64>,
    answer: Answer,
    typing: Typing,
    waiting: Vec<Value>,
}

/// What is known of the subagents of the threads watched, kept from one
/// watch to the next: a subagent may outlive the turn that began it.
#[derive(Default)]
struct Subagents {
    /// Titles, by Lever ID.
    titles: HashMap<String, String>,
    /// Those still working, by Lever ID, with their thread.
    working: HashMap<String, String>,
    /// Answers written so far, shown when the subagent ends, by Lever ID.
    answers: HashMap<String, String>,
}

impl Subagents {
    fn working_on(&self, thread: &str) -> bool {
        self.working.values().any(|t| t == thread)
    }
}

/// Writes a turn's answer as the record has it, with each message the Agent
/// took into the turn at the place in the answer it took it.
fn write_answer(answer: &mut Answer, turn: &Value) -> Result<()> {
    let whole = text(turn, "answer");
    let mut from = 0;
    for steered in turn.get("steered").and_then(Value::as_array).into_iter().flatten() {
        let at = steer_place(&whole, from, steered.get("at").and_then(Value::as_u64));
        answer.write(&whole[from..at])?;
        answer.end()?;
        answer.write(&format!("> {}\n", text(steered, "text")))?;
        from = at;
    }
    answer.write(&whole[from..])
}

/// Where in `whole`, an answer written up to `from`, a message the Agent took
/// at `at` shows: there, or past what the record kept of the answer, at its
/// end, and never before what is written or inside a character.
fn steer_place(whole: &str, from: usize, at: Option<u64>) -> usize {
    at.and_then(|at| usize::try_from(at).ok())
        .map(|at| at.min(whole.len()))
        .filter(|&at| at >= from && whole.is_char_boundary(at))
        .unwrap_or(from)
}

/// The line a turn begins with: the person's message, or, for a turn the
/// harness began itself, as when a subagent reported, a mark saying so.
/// `turn` is a turn's record or its `message` event, whose text is `field`.
fn message_line(turn: &Value, field: &str) -> String {
    if text(turn, "origin") == "harness" {
        "[the Agent went on after a subagent reported]".to_string()
    } else {
        format!("> {}", text(turn, field))
    }
}

/// Writes a turn's answer and acts as they come, until the turn and any of
/// its subagents end. Ctrl-C stops the turn (and any working subagents):
/// Lever is asked to, and their end is waited for. A second Ctrl-C does not
/// wait. A request of the thread (or a subagent) is shown as it comes, and
/// on a terminal its answer is asked for, one request at a time: `waiting`
/// are those that already wait. With no turn and no working subagent, what
/// is watched is what waits, until nothing does. `last` is how the thread
/// last ended, and a turn followed here that ends changes it, though the
/// person leaves after.
fn watch(
    lever: &mut Connection,
    thread: &str,
    start: WatchStart,
    subagents: &mut Subagents,
    last: &mut Ended,
) -> Result<Option<Ended>> {
    let WatchStart {
        mut turn,
        mut answer,
        typing,
        waiting,
    } = start;
    let subagent_titles = &mut subagents.titles;
    let working_subagents = &mut subagents.working;
    let subagent_answers = &mut subagents.answers;
    let working_here = |working: &HashMap<String, String>| working.values().any(|t| t == thread);
    let prompted = std::io::stdin().is_terminal();
    // Only requests are watched here, not a turn or subagent of the person's:
    // Ctrl-C turns down the one asked for, by stopping its own turn, and the
    // prompt comes back at once, unless a turn followed goes on, which is
    // followed to its end.
    let only_requests = turn.is_none() && !working_here(working_subagents);
    let mut stopping = false;
    // Whether the whole thread was told to stop, not only the turn followed.
    let mut thread_stopped = false;
    // The request whose answer is asked for here, and the answers it offers.
    let mut asking: Option<(String, Vec<String>)> = None;
    // The request taken up here, until Lever says how it ended: no other
    // is asked for before.
    let mut taken: Option<Value> = None;
    // Requests that wait, by ID, and those still to be asked here, in order.
    let mut open: Vec<String> = Vec::new();
    let mut queued: std::collections::VecDeque<Value> = std::collections::VecDeque::new();
    // A Stop asked here ends the watch as stopped, though the turn had
    // ended before: what was stopped is its subagents.
    let stopped_or = |stopping: bool, ended: Ended| {
        if stopping && ended != Ended::Failed {
            Ended::Stopped
        } else {
            ended
        }
    };
    for request in waiting {
        open.push(text(&request, "request").into_owned());
        if prompted {
            queued.push_back(request);
        } else {
            let sub_title = subagent_title_for_request(&request, subagent_titles);
            show_request(&mut answer, &request, thread, false, sub_title)?;
        }
    }
    // How the turn ended, once it has while a request asked for here or a
    // subagent still works: those finish before the prompt comes back.
    let mut ended: Option<Ended> = None;
    // How the turn the watch began on ended: what the run says, whatever
    // turn the harness begins after it.
    let own = turn;
    let mut own_end: Option<Ended> = None;
    loop {
        if (turn.is_none() || ended.is_some()) && (!prompted || open.is_empty()) && !working_here(working_subagents) {
            answer.end()?;
            return Ok(Some(stopped_or(stopping, own_end.or(ended).unwrap_or(*last))));
        }
        if asking.is_none()
            && taken.is_none()
            && !stopping
            && let Some(request) = queued.pop_front()
        {
            let sub_title = subagent_title_for_request(&request, subagent_titles);
            show_request(&mut answer, &request, thread, true, sub_title)?;
            asking = Some((text(&request, "request").into_owned(), answers_of(&request)));
            taken = Some(request);
        }
        let now = if stopping {
            Typing::Waits
        } else if asking.is_some() {
            Typing::Wanted
        } else {
            typing
        };
        let event = match lever.next(now)? {
            Happening::Event(event) => event,
            Happening::Interrupt if stopping => return Ok(Some(Ended::Stopped)),
            Happening::Interrupt => {
                let was_asking = asking.take().is_some();
                if was_asking {
                    eprintln!();
                }
                if only_requests {
                    // The request asked for is turned down, by stopping its own turn, or
                    // else the turn followed is stopped. What else waits, and is not of
                    // the turn stopped, says how to answer it, and is asked for no more.
                    let turn_of = |r: &Value| r.get("turn").and_then(Value::as_u64);
                    let running = turn.filter(|_| ended.is_none());
                    // A request answered here, whose answer Lever has yet to confirm,
                    // is not the one Ctrl-C is for.
                    let asked = taken.as_ref().filter(|_| was_asking);
                    let stopped = asked.and_then(turn_of).or(running);
                    // With neither, there is nothing to stop.
                    if let Some(stopped) = stopped {
                        lever.stop(thread, stopped)?;
                    }
                    for request in queued.drain(..).filter(|r| turn_of(r) != stopped) {
                        let sub_title = subagent_title_for_request(&request, subagent_titles);
                        show_request(&mut answer, &request, thread, false, sub_title)?;
                    }
                    open.clear();
                    taken = None;
                    // A turn followed that goes on is followed to its end, which
                    // brings the prompt back, and when it is the turn stopped, its
                    // end is waited for as the person's own turn's is. Otherwise
                    // the prompt comes back now.
                    if running.is_some() {
                        stopping = running == stopped;
                        continue;
                    }
                    answer.end()?;
                    return Ok(Some(ended.unwrap_or(*last)));
                }
                stopping = true;
                // By its number, so a stop that comes after this turn ended does not reach the one after it.
                // Once it has, what holds the watch is the thread's subagents, of
                // whichever turn: the thread is stopped.
                match turn.filter(|_| ended.is_none()) {
                    Some(turn) => lever.stop(thread, turn)?,
                    None => {
                        lever.send(json!({ "op": "stop", "thread": thread }))?;
                        thread_stopped = true;
                    }
                }
                continue;
            }
            Happening::EndOfInput => {
                if asking.take().is_some() {
                    eprintln!();
                }
                answer.end()?;
                return Ok(None);
            }
            Happening::Line(line) => {
                if let Some((request, answers)) = &asking {
                    let chosen = line.trim();
                    if answers.iter().any(|a| a == chosen) {
                        lever
                            .send(json!({ "op": "answer", "thread": thread, "request": request, "answer": chosen }))?;
                        asking = None;
                    } else {
                        eprint!("Answer [{}]: ", answers.join("/"));
                        let _ = stderr().flush();
                    }
                }
                continue;
            }
        };
        // What Lever said of an answer sent from here. One it refused
        // leaves the request to be asked for again, unless it waits no more:
        // Lever says so of it next.
        if let Some(code) = refused(&event) {
            if code != "no-request"
                && asking.is_none()
                && let Some(request) = taken.take()
                && open.iter().any(|r| *r == text(&request, "request"))
            {
                queued.push_front(request);
            }
            continue;
        }
        if event.get("thread").and_then(Value::as_str) != Some(thread) {
            continue;
        }
        // A subagent's lifecycle, acts, and text are its thread's and may
        // outlive its turn.
        match &*text(&event, "event") {
            "subagent" => {
                let id = text(&event, "subagent").into_owned();
                let title = text(&event, "title").into_owned();
                if !title.is_empty() {
                    subagent_titles.insert(id.clone(), title.clone());
                }
                let state = text(&event, "state");
                if state == "working" {
                    let is_new = working_subagents.insert(id, thread.to_string()).is_none();
                    if is_new {
                        show_subagent(&mut answer, &event)?;
                    }
                } else {
                    working_subagents.remove(&id);
                    if let Some(body) = subagent_answers.remove(&id) {
                        show_subagent_answer(&mut answer, Some(title.as_str()), &body)?;
                    }
                    show_subagent(&mut answer, &event)?;
                }
                continue;
            }
            "subagent-text" => {
                if text(&event, "role") == "answer" {
                    let id = text(&event, "subagent").into_owned();
                    subagent_answers.entry(id).or_default().push_str(&text(&event, "text"));
                }
                continue;
            }
            "subagent-act" => {
                let id = text(&event, "subagent");
                let sub_title = subagent_titles.get(id.as_ref()).map(String::as_str);
                show_subagent_act(&mut answer, sub_title, &event)?;
                continue;
            }
            _ => {}
        }
        // A request is its thread's, whichever turn or subagent asked it.
        if text(&event, "event") == "request" {
            let id = text(&event, "request").into_owned();
            let sub_title = subagent_title_for_request(&event, subagent_titles);
            let waits = text(&event, "state") == "waiting";
            if waits {
                if !open.contains(&id) {
                    open.push(id);
                }
                // On a terminal, asked for when no other is.
                if prompted && !stopping {
                    queued.push_back(event);
                } else {
                    show_request(&mut answer, &event, thread, false, sub_title)?;
                }
                continue;
            }
            open.retain(|r| *r != id);
            queued.retain(|r| text(r, "request") != id);
            if taken.as_ref().is_some_and(|r| text(r, "request") == id) {
                taken = None;
            }
            if asking.as_ref().is_some_and(|(r, _)| *r == id) {
                asking = None;
                eprintln!();
            }
            show_request(&mut answer, &event, thread, false, sub_title)?;
            continue;
        }
        // An act is its own turn's, and may end after a later turn began:
        // the thread's acts are shown whichever turn is watched.
        if text(&event, "event") == "act" {
            show_act(&mut answer, &event)?;
            continue;
        }
        // A turn that begins while only requests are watched, as one another
        // client sends, is watched from its message on, as is one the harness
        // begins after the turn watched ended, as when a subagent reports.
        if (only_requests || text(&event, "origin") == "harness")
            && (turn.is_none() || ended.is_some())
            && text(&event, "event") == "message"
        {
            answer.end()?;
            answer.write(&format!("\n{}\n", message_line(&event, "text")))?;
            turn = event.get("turn").and_then(Value::as_u64);
            ended = None;
            continue;
        }
        if event.get("turn").and_then(Value::as_u64) != turn {
            continue;
        }
        // After the turn ended, what the harness adds to it is still shown.
        if ended.is_some() && !matches!(&*text(&event, "event"), "text" | "note") {
            continue;
        }
        let end = match &*text(&event, "event") {
            "text" => {
                answer.write(&text(&event, "text"))?;
                continue;
            }
            // A message the Agent took into the turn, where it took it.
            "steered" => {
                answer.end()?;
                answer.write(&format!("> {}\n", text(&event, "text")))?;
                continue;
            }
            "note" => {
                show_note(&mut answer, &event)?;
                continue;
            }
            "done" => {
                answer.end()?;
                Ended::Done
            }
            "stopped" => {
                answer.end()?;
                eprintln!("Stopped.");
                Ended::Stopped
            }
            "failed" => {
                answer.end()?;
                eprintln!("crowbar-agent: The Agent gave no answer: {}", text(&event, "message"));
                Ended::Failed
            }
            "interrupted" => {
                answer.end()?;
                eprintln!("Interrupted.");
                Ended::Failed
            }
            _ => continue,
        };
        *last = end;
        if own.is_some() && turn == own {
            own_end.get_or_insert(end);
        }
        // The turn followed has ended stopped while an older turn's subagent
        // still holds the watch: Ctrl-C was for all the watch shows, so the
        // thread is stopped.
        if stopping && !thread_stopped && working_here(working_subagents) {
            lever.send(json!({ "op": "stop", "thread": thread }))?;
            thread_stopped = true;
        }
        // On a terminal, a request that still waits is asked for first, and
        // any working subagent keeps the watch open until it finishes.
        if (stopping || !prompted || open.is_empty()) && !working_here(working_subagents) {
            return Ok(Some(stopped_or(stopping, own_end.unwrap_or(end))));
        }
        ended = Some(end);
    }
}

/// A thread that was resumed, as it stood.
struct Resumed {
    /// The turn that is still working, if one is.
    working: Option<u64>,
    /// Subagents of this thread that are still working, by Lever ID.
    working_subagents: HashSet<String>,
    /// Known titles of this thread's subagents, by Lever ID.
    subagent_titles: HashMap<String, String>,
    /// The answer as far as it was written.
    answer: Answer,
    /// How its newest turn ended, if it has.
    last: Ended,
    /// The requests of the thread that wait for the person.
    waiting: Vec<Value>,
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
    let mut working_subagents = HashSet::new();
    let mut subagent_titles = HashMap::new();
    for turn in &turns {
        for sub in turn.get("subagents").and_then(Value::as_array).into_iter().flatten() {
            let sid = text(sub, "subagent").into_owned();
            let title = text(sub, "title").into_owned();
            if !sid.is_empty() && !title.is_empty() {
                subagent_titles.insert(sid.clone(), title);
            }
            if !sid.is_empty() && text(sub, "state") == "working" {
                working_subagents.insert(sid);
            }
        }
    }
    for turn in &turns {
        answer.write(&format!("{}\n", message_line(turn, "message")))?;
        for act in turn.get("acts").and_then(Value::as_array).into_iter().flatten() {
            show_act(&mut answer, act)?;
        }
        for sub in turn.get("subagents").and_then(Value::as_array).into_iter().flatten() {
            let sub_title = text(sub, "title");
            show_subagent(&mut answer, sub)?;
            for act in sub.get("acts").and_then(Value::as_array).into_iter().flatten() {
                show_subagent_act(&mut answer, Some(sub_title.as_ref()), act)?;
            }
            for msg in sub.get("messages").and_then(Value::as_array).into_iter().flatten() {
                if text(msg, "role") == "answer" {
                    show_subagent_answer(&mut answer, Some(sub_title.as_ref()), &text(msg, "text"))?;
                }
            }
        }
        for request in turn.get("requests").and_then(Value::as_array).into_iter().flatten() {
            // One that waits is shown as the thread is watched, and answered there.
            if text(request, "state") != "waiting" {
                let sub_title = subagent_title_for_request(request, &subagent_titles);
                show_request(&mut answer, request, thread, false, sub_title)?;
            }
        }
        for note in turn.get("notes").and_then(Value::as_array).into_iter().flatten() {
            show_note(&mut answer, note)?;
        }
        write_answer(&mut answer, turn)?;
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
        working_subagents,
        subagent_titles,
        answer,
        last: if matches!(&*newest, "failed" | "interrupted") {
            Ended::Failed
        } else {
            Ended::Done
        },
        // Each with its turn, as Lever's events have it.
        waiting: turns
            .iter()
            .flat_map(|turn| {
                let number = turn.get("turn").cloned();
                turn.get("requests")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(move |request| {
                        let mut request = request.clone();
                        if let Some(number) = &number {
                            request["turn"] = number.clone();
                        }
                        request
                    })
            })
            .filter(|request| text(request, "state") == "waiting")
            .collect(),
    }))
}

/// `crowbar-agent [--resume ID] [--model M] "<request>"`: one turn, and out.
pub fn once(
    lever: &mut Connection,
    resumed: Option<&str>,
    model: Option<&str>,
    mode: Option<&str>,
    message: &str,
) -> Result<Ended> {
    // Lever says so itself when there is no such thread, so the thread's
    // record is not asked for only to be dropped.
    let mut thread = resumed.map(str::to_string);
    turn(lever, &mut thread, model, mode, message, &mut Subagents::default())
}

/// Runs OpenCode's own sign-in directly in this terminal. Lever holds no
/// credential and relays none.
fn run_opencode_login() -> ExitCode {
    match Command::new("opencode").args(["auth", "login"]).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) if status.signal() == Some(libc::SIGINT) => ExitCode::from(130),
        Ok(_) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("crowbar-agent: could not run `opencode auth login`: {err}.");
            ExitCode::FAILURE
        }
    }
}

/// `crowbar-agent login`: runs OpenCode's own sign-in in this terminal, and if
/// Lever is reachable refreshes its Model list afterwards.
pub fn login() -> ExitCode {
    let code = run_opencode_login();
    if code == ExitCode::SUCCESS
        && let Ok(mut lever) = Connection::connect()
    {
        let _ = lever.request(json!({ "op": "models", "refresh": true }));
    }
    code
}

/// Before starting a fresh interactive thread, checks whether any Model is
/// available. When OpenCode's free hosted Models are off and no provider is
/// signed in, offers the choice on a terminal: enable free hosted Models, or
/// sign in. Non-interactive (piped) input is never read as a choice.
fn ensure_model_choice(lever: &mut Connection, interactive: bool) -> Result<Option<Ended>> {
    let reply = lever.request(json!({ "op": "models" }))?;
    if refused(&reply).is_some() {
        return Ok(Some(Ended::Failed));
    }
    let supported = reply.get("supported").and_then(Value::as_bool).unwrap_or(false);
    let has_models = reply
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|m| !m.is_empty());
    if !supported || has_models {
        return Ok(None);
    }

    if !interactive {
        eprintln!("crowbar-agent: No Model is available. Run `crowbar-agent free-models on` or `crowbar-agent login`.");
        return Ok(Some(Ended::Failed));
    }

    eprintln!("No Model is available yet. Choose how to connect:");
    eprintln!("  1. Use OpenCode's free hosted Models (requests go to OpenCode's makers)");
    eprintln!("  2. Sign in to a provider (`opencode auth login`)");
    eprint!("Choice [1/2]: ");
    let _ = stderr().flush();

    let choice = loop {
        match lever.next(Typing::Wanted)? {
            Happening::Line(line) => break line,
            Happening::EndOfInput => {
                eprintln!();
                eprintln!(
                    "crowbar-agent: No Model is available. Run `crowbar-agent free-models on` or `crowbar-agent login`."
                );
                return Ok(Some(Ended::Failed));
            }
            Happening::Interrupt => {
                eprintln!();
                return Ok(Some(Ended::Stopped));
            }
            Happening::Event(_) => continue,
        }
    };

    match choice.trim() {
        "1" => {
            let reply = lever.request(json!({ "op": "set-free-models", "enabled": true }))?;
            if refused(&reply).is_some() {
                return Ok(Some(Ended::Failed));
            }
            eprintln!("Enabled OpenCode's free hosted Models.");
            let refreshed = lever.request(json!({ "op": "models", "refresh": true }))?;
            if refused(&refreshed).is_some() {
                return Ok(Some(Ended::Failed));
            }
            let now_has_models = refreshed
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|m| !m.is_empty());
            if !now_has_models {
                eprintln!("crowbar-agent: No Model is available after enabling free hosted Models.");
                return Ok(Some(Ended::Failed));
            }
            Ok(None)
        }
        "2" => {
            let code = run_opencode_login();
            if code == ExitCode::from(130) {
                return Ok(Some(Ended::Stopped));
            }
            if code != ExitCode::SUCCESS {
                return Ok(Some(Ended::Failed));
            }
            let refreshed = lever.request(json!({ "op": "models", "refresh": true }))?;
            if refused(&refreshed).is_some() {
                return Ok(Some(Ended::Failed));
            }
            let now_has_models = refreshed
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|m| !m.is_empty());
            if !now_has_models {
                eprintln!("crowbar-agent: No Model is available after sign-in.");
                return Ok(Some(Ended::Failed));
            }
            Ok(None)
        }
        _ => {
            eprintln!("crowbar-agent: No Model chosen. Run `crowbar-agent free-models on` or `crowbar-agent login`.");
            Ok(Some(Ended::Failed))
        }
    }
}

/// How a thread the person left ends: as a failure when its newest turn got no answer.
fn left(last: Ended) -> Ended {
    if last == Ended::Failed {
        Ended::Failed
    } else {
        Ended::Done
    }
}

/// `crowbar-agent [--resume ID] [--model M]`: a thread, a line at a time. A
/// thread starts with its first message, so leaving at once leaves none behind.
/// Ctrl-D leaves, and so does Ctrl-C between turns.
pub fn repl(lever: &mut Connection, resumed: Option<&str>, model: Option<&str>, mode: Option<&str>) -> Result<Ended> {
    let prompted = std::io::stdin().is_terminal();
    let mut thread = None;
    let mut pending_model = model.map(str::to_string);
    let mut pending_mode = mode.map(str::to_string);
    let mut last = Ended::Done;
    let mut subagents = Subagents::default();
    if let Some(resumed) = resumed {
        let Some(stood) = resume(lever, resumed)? else {
            return Ok(Ended::Failed);
        };
        last = stood.last;
        subagents.titles = stood.subagent_titles;
        subagents.working = stood
            .working_subagents
            .into_iter()
            .map(|id| (id, resumed.to_string()))
            .collect();
        if stood.working.is_some() || !stood.waiting.is_empty() || subagents.working_on(resumed) {
            // Ctrl-D leaves a turn or subagent that was working when the
            // thread was resumed, as it leaves one that begins after.
            let Some(ended) = watch(
                lever,
                resumed,
                WatchStart {
                    turn: stood.working,
                    answer: stood.answer,
                    typing: Typing::UntilItEnds,
                    waiting: stood.waiting,
                },
                &mut subagents,
                &mut last,
            )?
            else {
                return Ok(left(last));
            };
            last = ended;
        }
        thread = Some(resumed.to_string());
    } else if let Some(ended) = ensure_model_choice(lever, prompted)? {
        return Ok(ended);
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
                    answer.write(&format!("\n{}\n", message_line(&event, "text")))?;
                    let turn = event.get("turn").and_then(Value::as_u64);
                    let watched = watch(
                        lever,
                        &text(&event, "thread"),
                        WatchStart {
                            turn,
                            answer,
                            typing: Typing::UntilItEnds,
                            waiting: Vec::new(),
                        },
                        &mut subagents,
                        &mut last,
                    )?;
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
                Happening::Event(event) if text(&event, "event") == "subagent" => {
                    let id = text(&event, "subagent").into_owned();
                    let title = text(&event, "title").into_owned();
                    let state = text(&event, "state").into_owned();
                    if !title.is_empty() {
                        subagents.titles.insert(id.clone(), title);
                    }
                    let mut answer = Answer::new();
                    if state == "working" {
                        subagents
                            .working
                            .insert(id.clone(), text(&event, "thread").into_owned());
                    } else {
                        subagents.working.remove(&id);
                        if let Some(buf) = subagents.answers.remove(&id) {
                            let sub_title = subagents.titles.get(&id).map(String::as_str);
                            show_subagent_answer(&mut answer, sub_title, &buf)?;
                        }
                    }
                    show_subagent(&mut answer, &event)?;
                }
                Happening::Event(event) if text(&event, "event") == "subagent-text" => {
                    if text(&event, "role") != "prompt" {
                        let id = text(&event, "subagent").into_owned();
                        subagents.answers.entry(id).or_default().push_str(&text(&event, "text"));
                    }
                }
                Happening::Event(event) if text(&event, "event") == "subagent-act" => {
                    let id = text(&event, "subagent");
                    let sub_title = subagents.titles.get(id.as_ref()).map(String::as_str);
                    let mut answer = Answer::new();
                    show_subagent_act(&mut answer, sub_title, &event)?;
                }
                // A request that comes while the prompt waits, as one after
                // its turn does, is asked for here on a terminal, as one in a
                // turn is, and the prompt comes back when nothing waits.
                Happening::Event(event)
                    if text(&event, "event") == "request" && text(&event, "state") == "waiting" && prompted =>
                {
                    eprintln!();
                    let asked = text(&event, "thread").into_owned();
                    let watched = watch(
                        lever,
                        &asked,
                        WatchStart {
                            turn: None,
                            answer: Answer::new(),
                            typing: Typing::UntilItEnds,
                            waiting: vec![event],
                        },
                        &mut subagents,
                        &mut last,
                    )?;
                    // A turn followed meanwhile ends as the thread's newest.
                    let Some(ended) = watched else {
                        return Ok(left(last));
                    };
                    last = ended;
                    eprint!("> ");
                }
                Happening::Event(event) if text(&event, "event") == "request" => {
                    let sub_title = subagent_title_for_request(&event, &subagents.titles);
                    let mut answer = Answer::new();
                    show_request(&mut answer, &event, &text(&event, "thread"), false, sub_title)?;
                }
                Happening::Event(_) => {}
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let chosen = pending_model.take();
        let mode = pending_mode.take();
        last = turn(
            lever,
            &mut thread,
            chosen.as_deref(),
            mode.as_deref(),
            &line,
            &mut subagents,
        )?;
    }
}

/// `crowbar-agent --resume ID --model M`: set a thread's Model.
pub fn set_model(lever: &mut Connection, thread: &str, model: &str) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "set-model", "thread": thread, "model": model }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    eprintln!("Model for {} set to {}.", text(&reply, "thread"), text(&reply, "model"));
    Ok(Ended::Done)
}

/// `crowbar-agent --resume ID --mode M`: set a thread's mode.
pub fn set_mode(lever: &mut Connection, thread: &str, mode: &str) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "set-mode", "thread": thread, "mode": mode }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    eprintln!(
        "{} is now in {}.",
        text(&reply, "thread"),
        mode_name(&text(&reply, "mode"))
    );
    Ok(Ended::Done)
}

/// `crowbar-agent --resume ID --answer REQUEST ANSWER`: answer a request.
pub fn answer(lever: &mut Connection, thread: &str, request: &str, answer: &str) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "answer", "thread": thread, "request": request, "answer": answer }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    eprintln!("Answered {answer}.");
    Ok(Ended::Done)
}

/// `crowbar-agent default-mode [ask-first|full-access]`: show or set the mode
/// new threads start in.
pub fn default_mode(lever: &mut Connection, mode: Option<&str>) -> Result<Ended> {
    let Some(mode) = mode else {
        let reply = lever.request(json!({ "op": "status" }))?;
        if refused(&reply).is_some() {
            return Ok(Ended::Failed);
        }
        println!("New threads start in {}.", mode_name(&text(&reply, "default_mode")));
        return Ok(Ended::Done);
    };
    let reply = lever.request(json!({ "op": "set-default-mode", "mode": mode }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    eprintln!("New threads start in {}.", mode_name(&text(&reply, "mode")));
    Ok(Ended::Done)
}

/// The person's name for a mode.
fn mode_name(mode: &str) -> String {
    match mode {
        "ask-first" => "Ask first".to_string(),
        "full-access" => "Full access".to_string(),
        "plan" => "Plan".to_string(),
        other => other.to_string(),
    }
}

/// `crowbar-agent models`: list the Models the harness offers.
pub fn models(lever: &mut Connection) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "models" }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    let supported = reply.get("supported").and_then(Value::as_bool).unwrap_or(false);
    let list = reply
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !supported {
        eprintln!("The harness does not list Models.");
        return Ok(Ended::Done);
    }
    if list.is_empty() {
        eprintln!("No Models are available. Run `crowbar-agent free-models on` or `crowbar-agent login`.");
        return Ok(Ended::Done);
    }
    let default = reply.get("default").and_then(Value::as_str);
    for entry in &list {
        let id = text(entry, "id");
        let name = text(entry, "name");
        let mark = if default == Some(id.as_ref()) {
            "  (default)"
        } else {
            ""
        };
        if name.is_empty() || name == id {
            println!("{id}{mark}");
        } else {
            println!("{id}  {name}{mark}");
        }
    }
    Ok(Ended::Done)
}

/// `crowbar-agent efforts M`: list the effort levels of a Model.
pub fn efforts(lever: &mut Connection, model: &str) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "efforts", "model": model }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    let levels = reply
        .get("efforts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if levels.is_empty() {
        eprintln!("{model} has no effort levels.");
        return Ok(Ended::Done);
    }
    for level in &levels {
        let id = text(level, "id");
        let name = text(level, "name");
        if name.is_empty() || name == id {
            println!("{id}");
        } else {
            println!("{id}  {name}");
        }
    }
    Ok(Ended::Done)
}

/// `crowbar-agent --resume ID --effort E`: set a thread's effort.
pub fn set_effort(lever: &mut Connection, thread: &str, effort: &str) -> Result<Ended> {
    let reply = lever.request(json!({ "op": "set-effort", "thread": thread, "effort": effort }))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    eprintln!("{thread} is now at effort {}.", text(&reply, "effort"));
    Ok(Ended::Done)
}

/// `crowbar-agent commands`: list the commands the harness offers. Lever
/// has the harness say when it has not yet, so this asks again for a while.
pub fn commands(lever: &mut Connection) -> Result<Ended> {
    let mut reply = lever.request(json!({ "op": "commands" }))?;
    for _ in 0..50 {
        if refused(&reply).is_some() {
            return Ok(Ended::Failed);
        }
        if reply.get("known") == Some(&Value::Bool(true)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        reply = lever.request(json!({ "op": "commands" }))?;
    }
    let list = reply
        .get("commands")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if list.is_empty() {
        eprintln!("The harness offers no commands.");
        return Ok(Ended::Done);
    }
    for entry in &list {
        let name = text(entry, "name");
        let description = text(entry, "description");
        if description.is_empty() {
            println!("/{name}");
        } else {
            println!("/{name}  {description}");
        }
    }
    Ok(Ended::Done)
}

/// `crowbar-agent free-models [on|off]`: show or switch OpenCode's free hosted Models.
pub fn free_models(lever: &mut Connection, enabled: Option<bool>) -> Result<Ended> {
    match enabled {
        None => {
            let reply = lever.request(json!({ "op": "status" }))?;
            if refused(&reply).is_some() {
                return Ok(Ended::Failed);
            }
            let on = reply.get("free_hosted_models") == Some(&Value::Bool(true));
            println!("Free hosted Models: {}.", if on { "on" } else { "off" });
            Ok(Ended::Done)
        }
        Some(enable) => {
            let reply = lever.request(json!({ "op": "set-free-models", "enabled": enable }))?;
            if refused(&reply).is_some() {
                return Ok(Ended::Failed);
            }
            let on = reply.get("enabled") == Some(&Value::Bool(true));
            eprintln!("Free hosted Models turned {}.", if on { "on" } else { "off" });
            Ok(Ended::Done)
        }
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
    println!("New threads start in {}.", mode_name(&text(&reply, "default_mode")));
    let activity = lever.request(json!({ "op": "watch" }))?;
    if refused(&activity).is_some() {
        return Ok(Ended::Failed);
    }
    let ids = |field: &str| -> Vec<String> {
        activity
            .get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str().map(|id| crate::connection::plain(id).into_owned()))
            .collect()
    };
    let (working, waiting) = (ids("working"), ids("waiting"));
    if !working.is_empty() {
        println!("Working: {}.", working.join(", "));
    }
    if !waiting.is_empty() {
        println!("Waiting for you: {}.", waiting.join(", "));
    }
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
        let subagents = thread.get("subagents").and_then(Value::as_u64).unwrap_or(0);
        let sub_note = match subagents {
            0 => String::new(),
            1 => "  (1 subagent)".to_string(),
            n => format!("  ({n} subagents)"),
        };
        let state = text(thread, "state");
        let badge = match &*state {
            _ if thread.get("waiting") == Some(&Value::Bool(true)) => "  (waiting for you)",
            _ if thread.get("working") == Some(&Value::Bool(true)) => "  (working)",
            "working" => "  (working)",
            "failed" => "  (failed)",
            "stopped" => "  (stopped)",
            "interrupted" => "  (interrupted)",
            _ => "",
        };
        println!(
            "{}  {:<14}  {turns} turn{}  {}{sub_note}{badge}",
            text(thread, "thread"),
            ago(now.saturating_sub(updated)),
            if turns == 1 { " " } else { "s" },
            text(thread, "title"),
        );
    }
    Ok(Ended::Done)
}

/// Archive preserves the record; delete asks the harness to remove its session too.
pub fn lifecycle(lever: &mut Connection, op: &str, thread: &str) -> Result<Ended> {
    let reply = lever.request(json!({"op":op,"thread":thread}))?;
    if refused(&reply).is_some() {
        return Ok(Ended::Failed);
    }
    if let Some(warning) = reply["warning"].as_str() {
        eprintln!("{warning}");
    }
    eprintln!("Thread {}.", if op == "archive" { "archived" } else { "deleted" });
    Ok(Ended::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_taken_into_a_turn_shows_where_it_was_taken_or_after_what_the_record_kept() {
        let whole = "Looking. Took it.";
        assert_eq!(steer_place(whole, 0, Some(9)), 9);
        // Past what the record kept of the answer: at its end, not its start.
        assert_eq!(steer_place(whole, 0, Some(400)), whole.len());
        // Never before what is written, nor inside a character.
        assert_eq!(steer_place(whole, 12, Some(9)), 12);
        assert_eq!(steer_place("né", 0, Some(2)), 0);
        assert_eq!(steer_place(whole, 3, None), 3);
    }

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
