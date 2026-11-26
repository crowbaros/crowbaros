//! Talking with the Agent through Lever: one turn and out, a thread a line at
//! a time, listing, stopping, and checking status.
//!
//! The answer is plain text on standard output as it comes, so it reads the
//! same in foot, on a console, over SSH, and down a pipe. Acts, unrecognized
//! updates, prompts, and errors go to standard error.

use crate::connection::{Connection, Happening, Typing, refused, text};
use serde_json::{Value, json};
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

/// Prints a request to standard error: what it asks to do, the exact command
/// or the file and the change, and, when it waits, how to answer it. With
/// `asking`, the answer is asked for here.
fn show_request(answer: &mut Answer, request: &Value, thread: &str, asking: bool) -> Result<()> {
    answer.make_way()?;
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
                eprintln!("[request] Run: {}", lines.next().unwrap_or_default());
                for line in lines {
                    eprintln!("  {line}");
                }
            } else {
                let what = match &*text(request, "kind") {
                    "edit" => "Edit",
                    "delete" => "Delete",
                    "move" => "Move",
                    _ => "Act",
                };
                let path = text(request, "path");
                let named = if path.is_empty() { text(request, "title") } else { path };
                eprintln!("[request] {what}: {named}");
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
        "answered" => eprintln!("[request] answered: {}", text(request, "answer")),
        _ => eprintln!("[request] withdrawn"),
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
    let begun = text(&reply, "thread").to_string();
    // How the turn ended, though the person leaves while a request of its waits.
    let mut last = Ended::Done;
    let ended = watch(
        lever,
        &begun,
        reply.get("turn").and_then(Value::as_u64),
        Answer::new(),
        Typing::Waits,
        Vec::new(),
        &mut last,
    );
    *thread = Some(begun);
    ended.map(|ended| ended.unwrap_or(last))
}

/// Writes a turn's answer and acts as they come, until the turn ends. Ctrl-C
/// stops the turn: Lever is asked to, and the turn's own end is waited for. A
/// second Ctrl-C does not wait. A request of the thread is shown as it comes,
/// and on a terminal its answer is asked for, one request at a time:
/// `waiting` are those that already wait. With no turn, what is watched is
/// what waits, until nothing does. `last` is how the thread last ended, and
/// a turn followed here that ends changes it, though the person leaves
/// after.
fn watch(
    lever: &mut Connection,
    thread: &str,
    mut turn: Option<u64>,
    mut answer: Answer,
    typing: Typing,
    waiting: Vec<Value>,
    last: &mut Ended,
) -> Result<Option<Ended>> {
    let prompted = std::io::stdin().is_terminal();
    // Only requests are watched here, not a turn of the person's: Ctrl-C
    // turns down the one asked for, by stopping its own turn, and the
    // prompt comes back at once.
    let only_requests = turn.is_none();
    let mut stopping = false;
    // The request whose answer is asked for here, and the answers it offers.
    let mut asking: Option<(String, Vec<String>)> = None;
    // The request taken up here, until Lever says how it ended: no other
    // is asked for before.
    let mut taken: Option<Value> = None;
    // Requests that wait, by ID, and those still to be asked here, in order.
    let mut open: Vec<String> = Vec::new();
    let mut queued: std::collections::VecDeque<Value> = std::collections::VecDeque::new();
    for request in waiting {
        open.push(text(&request, "request").into_owned());
        if prompted {
            queued.push_back(request);
        } else {
            show_request(&mut answer, &request, thread, false)?;
        }
    }
    // How the turn ended, once it has while a request asked for here still
    // waits: the request is answered here before the prompt comes back.
    let mut ended: Option<Ended> = None;
    loop {
        if (turn.is_none() || ended.is_some()) && open.is_empty() {
            answer.end()?;
            return Ok(Some(ended.unwrap_or(*last)));
        }
        if asking.is_none()
            && taken.is_none()
            && !stopping
            && let Some(request) = queued.pop_front()
        {
            show_request(&mut answer, &request, thread, true)?;
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
                if asking.take().is_some() {
                    eprintln!();
                }
                if only_requests {
                    // The request asked for is turned down, by stopping its own turn, or
                    // else the turn followed is stopped. What else waits, and is not of
                    // the turn stopped, says how to answer it, and is asked for no more.
                    let turn_of = |r: &Value| r.get("turn").and_then(Value::as_u64);
                    let running = turn.filter(|_| ended.is_none());
                    let stopped = taken.as_ref().and_then(turn_of).or(running);
                    lever.send(json!({ "op": "stop", "thread": thread, "turn": stopped }))?;
                    for request in queued.drain(..).filter(|r| turn_of(r) != stopped) {
                        show_request(&mut answer, &request, thread, false)?;
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
                lever.send(json!({ "op": "stop", "thread": thread, "turn": turn }))?;
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
        // A request is its thread's, whichever turn asked it.
        if text(&event, "event") == "request" {
            let id = text(&event, "request").into_owned();
            let waits = text(&event, "state") == "waiting";
            if waits {
                if !open.contains(&id) {
                    open.push(id);
                }
                // On a terminal, asked for when no other is.
                if prompted && !stopping {
                    queued.push_back(event);
                } else {
                    show_request(&mut answer, &event, thread, false)?;
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
            show_request(&mut answer, &event, thread, false)?;
            continue;
        }
        // An act is its own turn's, and may end after a later turn began:
        // the thread's acts are shown whichever turn is watched.
        if text(&event, "event") == "act" {
            show_act(&mut answer, &event)?;
            continue;
        }
        // A turn that begins while only requests are watched, as one another
        // client sends, is watched from its message on.
        if only_requests && (turn.is_none() || ended.is_some()) && text(&event, "event") == "message" {
            answer.end()?;
            answer.write(&format!("\n> {}\n", text(&event, "text")))?;
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
        // On a terminal, a request that still waits is asked for first,
        // unless the person is stopping.
        if !prompted || stopping || open.is_empty() {
            return Ok(Some(end));
        }
        ended = Some(end);
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
    for turn in &turns {
        answer.write(&format!("> {}\n", text(turn, "message")))?;
        for act in turn.get("acts").and_then(Value::as_array).into_iter().flatten() {
            show_act(&mut answer, act)?;
        }
        for request in turn.get("requests").and_then(Value::as_array).into_iter().flatten() {
            // One that waits is shown as the thread is watched, and answered there.
            if text(request, "state") != "waiting" {
                show_request(&mut answer, request, thread, false)?;
            }
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
    turn(lever, &mut thread, model, mode, message)
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
    if let Some(resumed) = resumed {
        let Some(stood) = resume(lever, resumed)? else {
            return Ok(Ended::Failed);
        };
        last = stood.last;
        if stood.working.is_some() || !stood.waiting.is_empty() {
            // Ctrl-D leaves a turn that was working when the thread was
            // resumed, as it leaves one that begins after.
            let Some(ended) = watch(
                lever,
                resumed,
                stood.working,
                stood.answer,
                Typing::UntilItEnds,
                stood.waiting,
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
                    answer.write(&format!("\n> {}\n", text(&event, "text")))?;
                    let turn = event.get("turn").and_then(Value::as_u64);
                    let watched = watch(
                        lever,
                        &text(&event, "thread"),
                        turn,
                        answer,
                        Typing::UntilItEnds,
                        Vec::new(),
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
                        None,
                        Answer::new(),
                        Typing::UntilItEnds,
                        vec![event],
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
                    let mut answer = Answer::new();
                    show_request(&mut answer, &event, &text(&event, "thread"), false)?;
                }
                Happening::Event(_) => {}
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let chosen = pending_model.take();
        let mode = pending_mode.take();
        last = turn(lever, &mut thread, chosen.as_deref(), mode.as_deref(), &line)?;
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
        let state = text(thread, "state");
        let badge = match &*state {
            _ if thread.get("waiting") == Some(&Value::Bool(true)) => "  (waiting for you)",
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
