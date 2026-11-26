//! One connection to Lever's person socket, and everything the client waits on
//! at once: Lever, the person's typing, and Ctrl-C.
//!
//! Lever's framing (`lever/README.md`): a message is a frame, its length in
//! four bytes, the most significant first, and then that many bytes of JSON.
//! The first frame each way states the protocol version and nothing else, and
//! the client states it first.

use crate::sys;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// The version of the person socket's protocol this client speaks.
const VERSION: u32 = 1;
/// The longest frame the client takes from Lever: a long thread, resumed.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Where Lever listens for the person: in their runtime directory, which is
/// `/run/user/<user ID>` where nothing says otherwise, as on a console or over
/// SSH.
pub fn socket_path() -> PathBuf {
    let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) if !runtime.is_empty() => PathBuf::from(runtime),
        _ => PathBuf::from(format!("/run/user/{}", sys::uid())),
    };
    runtime.join("crowbar-lever/person")
}

/// Text from outside the client, as a terminal may be given it. A Model is
/// not trusted, and what it says reaches the person's terminal, where a
/// control character could move the cursor, write over what was said
/// before, or set the clipboard. Each one is shown as its picture, `␛` for
/// escape, and does nothing. Line ends and tabs stay, and a carriage return
/// is dropped.
pub fn plain(text: &str) -> Cow<'_, str> {
    let stays = |character: char| !character.is_control() || character == '\n' || character == '\t';
    if text.chars().all(stays) {
        return Cow::Borrowed(text);
    }
    let shown = text
        .chars()
        .filter(|&character| character != '\r')
        .map(|character| match character {
            character if stays(character) => character,
            // The pictures of the first 32 characters are in their order, from U+2400.
            '\0'..='\x1f' => char::from_u32(0x2400 + u32::from(character)).unwrap_or(char::REPLACEMENT_CHARACTER),
            '\x7f' => '\u{2421}',
            _ => char::REPLACEMENT_CHARACTER,
        });
    Cow::Owned(shown.collect())
}

/// A field of a message that is text, or nothing where it is not. Every word
/// of Lever's that the client shows comes through here, so it is `plain`.
pub fn text<'a>(message: &'a Value, field: &str) -> Cow<'a, str> {
    plain(message.get(field).and_then(Value::as_str).unwrap_or_default())
}

/// When a reply is an error, says it to the person, and gives its code.
pub fn refused(reply: &Value) -> Option<Cow<'_, str>> {
    if text(reply, "reply") != "error" {
        return None;
    }
    eprintln!("crowbar-agent: {}", text(reply, "message"));
    Some(text(reply, "code"))
}

/// What `next` does with the person's typing while it waits.
#[derive(Clone, Copy, PartialEq)]
pub enum Typing {
    /// It waits in the terminal, unread.
    Waits,
    /// A line of it is what is waited for.
    Wanted,
    /// It waits in the terminal, unread, and its end is what is waited
    /// for: Ctrl-D, or the end of what was piped in, with nothing typed
    /// before it. What is typed stays the terminal's, so Ctrl-C drops it as
    /// a terminal does, and its end is not seen past it.
    UntilItEnds,
}

/// What the client waited for, and got.
pub enum Happening {
    /// An event from Lever.
    Event(Value),
    /// A line the person typed, without its end.
    Line(String),
    /// The person's typing ended: Ctrl-D, or the end of what was piped in.
    EndOfInput,
    /// Ctrl-C.
    Interrupt,
}

pub struct Connection {
    stream: UnixStream,
    /// Bytes from Lever that do not make a whole frame yet.
    received: Vec<u8>,
    /// Events that came while a reply was waited for.
    events: VecDeque<Value>,
    interrupts: RawFd,
    /// Where the person types: standard input, which is never closed here.
    input: RawFd,
    /// What the person typed that does not make a whole line yet.
    typed: Vec<u8>,
    typing_ended: bool,
    /// Whether something typed was seen to wait in the terminal while only
    /// the end of typing was waited for. It is left there, and not looked
    /// at again until a line is wanted.
    typed_ahead: bool,
    /// How many replies are still to come to requests `send` did not wait
    /// for. Lever answers in the order it was asked, so they come before the
    /// reply to any request made since.
    owed: usize,
    /// Each Stop sent, until Lever replies to it: its thread and turn, and
    /// how many replies are still to come up to and including its own. A
    /// request of that turn that waits, and that Lever sent before it took
    /// the Stop, is one the Stop withdraws, and is not given.
    stopping: Vec<(String, u64, usize)>,
    /// Requests set aside so, by ID: their withdrawal, when it comes, is
    /// set aside too, as nothing was shown of them. Any other end is shown.
    set_aside: Vec<String>,
}

/// Why there is no connection to Lever.
pub enum Unconnected {
    /// In words for the person.
    Why(String),
    /// The person pressed Ctrl-C while Lever was waited for.
    Interrupted,
}

/// One whole frame's message taken off the front of `received`, if it holds one.
fn framed(received: &mut Vec<u8>) -> Result<Option<Value>> {
    let Some(length) = received.first_chunk::<4>() else {
        return Ok(None);
    };
    let length = u32::from_be_bytes(*length) as usize;
    if length > MAX_FRAME {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Lever sent a frame too long to read",
        ));
    }
    if received.len() < 4 + length {
        return Ok(None);
    }
    let message = serde_json::from_slice(&received[4..4 + length])?;
    received.drain(..4 + length);
    Ok(Some(message))
}

impl Connection {
    /// Connects to Lever and states the version. An error says why not, in
    /// words for the person.
    pub fn connect() -> std::result::Result<Connection, Unconnected> {
        let path = socket_path();
        let stream = UnixStream::connect(&path).map_err(|error| {
            Unconnected::Why(format!(
                "Lever is not running: nothing answers at {} ({error}).",
                path.display()
            ))
        })?;
        let interrupts =
            sys::interrupts().map_err(|error| Unconnected::Why(format!("Could not listen for Ctrl-C: {error}.")))?;
        let mut connection = Connection::over(stream, interrupts);
        let stated = json!({ "version": VERSION });
        let answered = connection.write(&stated).and_then(|()| connection.message());
        match answered {
            Ok(answer) if answer == stated => Ok(connection),
            Err(error) if error.kind() == ErrorKind::Interrupted => Err(Unconnected::Interrupted),
            // Lever closes a peer it does not serve, or whose version it does not speak, and says nothing.
            _ => Err(Unconnected::Why(format!(
                "Lever at {} closed the connection: it serves another account, or speaks another version than {VERSION}.",
                path.display()
            ))),
        }
    }

    /// A connection over `stream`, which hears Ctrl-C on `interrupts`.
    fn over(stream: UnixStream, interrupts: RawFd) -> Connection {
        Connection {
            stream,
            received: Vec::new(),
            events: VecDeque::new(),
            interrupts,
            input: 0,
            typed: Vec::new(),
            typing_ended: false,
            typed_ahead: false,
            owed: 0,
            stopping: Vec::new(),
            set_aside: Vec::new(),
        }
    }

    fn write(&mut self, message: &Value) -> Result<()> {
        let message = message.to_string();
        let length =
            u32::try_from(message.len()).map_err(|_| Error::new(ErrorKind::InvalidInput, "too long to frame"))?;
        self.stream
            .write_all(&[&length.to_be_bytes(), message.as_bytes()].concat())
    }

    /// Reads more from Lever. An error at its end.
    fn receive(&mut self) -> Result<()> {
        let mut bytes = [0u8; 65536];
        let count = self.stream.read(&mut bytes)?;
        if count == 0 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "Lever closed the connection"));
        }
        self.received.extend_from_slice(&bytes[..count]);
        Ok(())
    }

    /// The next message from Lever, waited for. Ctrl-C ends the wait, with an
    /// error of the kind `Interrupted`: a Lever that is slow to answer, or
    /// does not, is still one the person can leave.
    fn message(&mut self) -> Result<Value> {
        loop {
            if let Some(message) = framed(&mut self.received)? {
                return Ok(message);
            }
            if sys::readable(&[self.interrupts, self.stream.as_raw_fd()])?[0] {
                return Err(Error::new(ErrorKind::Interrupted, "Ctrl-C"));
            }
            self.receive()?;
        }
    }

    /// Sends a request, and gives its reply. Events that come first are kept
    /// for `next`, in their order. So are the replies to requests `send` did
    /// not wait for, which are not this one's.
    pub fn request(&mut self, request: Value) -> Result<Value> {
        self.write(&request)?;
        loop {
            let message = self.message()?;
            if message.get("reply").is_none() {
                if !self.withdrawn_by_stop(&message) {
                    self.events.push_back(message);
                }
            } else if self.owed > 0 {
                self.owed -= 1;
                self.replied();
            } else {
                return Ok(message);
            }
        }
    }

    /// Sends Stop for one turn of a thread. Until Lever replies to it, a
    /// request of that turn that waits is one it withdraws, and is not
    /// given: Lever sent it before it took the Stop.
    pub fn stop(&mut self, thread: &str, turn: u64) -> Result<()> {
        self.send(serde_json::json!({ "op": "stop", "thread": thread, "turn": turn }))?;
        self.stopping.push((thread.to_string(), turn, self.owed));
        Ok(())
    }

    /// Takes note of a reply: one fewer is still to come before the Stop's.
    fn replied(&mut self) {
        self.stopping.retain_mut(|(_, _, left)| {
            *left -= 1;
            *left > 0
        });
    }

    /// Whether a message is a request the Stop under way withdraws, or the
    /// end of one set aside so. Either is not given.
    fn withdrawn_by_stop(&mut self, message: &Value) -> bool {
        if text(message, "event") != "request" {
            return false;
        }
        let id = text(message, "request").into_owned();
        if text(message, "state") != "waiting" {
            // Its end is set aside only as the Stop's withdrawal: an answer
            // from another terminal, which came first, is shown.
            let before = self.set_aside.len();
            self.set_aside.retain(|r| *r != id);
            return self.set_aside.len() < before && text(message, "state") == "withdrawn";
        }
        let withdrawn = self.stopping.iter().any(|(thread, turn, _)| {
            text(message, "thread") == thread.as_str() && message.get("turn").and_then(Value::as_u64) == Some(*turn)
        });
        if withdrawn {
            self.set_aside.push(id);
        }
        withdrawn
    }

    /// Sends a request whose reply is not waited for here: `next` gives it
    /// as an event would be given, so nothing is missed while it comes. If
    /// `next` is not asked before the next `request`, that drops it.
    pub fn send(&mut self, request: Value) -> Result<()> {
        self.write(&request)?;
        self.owed += 1;
        Ok(())
    }

    /// Waits for the next thing to happen: an event, Ctrl-C, or, as
    /// `typing` has it, a line from the person or the end of their typing.
    pub fn next(&mut self, typing: Typing) -> Result<Happening> {
        // A line is wanted, so whatever waits in the terminal is looked at
        // afresh: it may have been dropped since it was seen there.
        if typing == Typing::Wanted {
            self.typed_ahead = false;
        }
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(Happening::Event(event));
            }
            if let Some(message) = framed(&mut self.received)? {
                if message.get("reply").is_some() {
                    self.owed = self.owed.saturating_sub(1);
                    self.replied();
                } else if self.withdrawn_by_stop(&message) {
                    continue;
                }
                return Ok(Happening::Event(message));
            }
            if typing == Typing::Wanted {
                if let Some(end) = self.typed.iter().position(|&byte| byte == b'\n') {
                    let line: Vec<u8> = self.typed.drain(..=end).collect();
                    return Ok(Happening::Line(
                        String::from_utf8_lossy(&line[..end]).trim_end_matches('\r').to_string(),
                    ));
                }
                if self.typing_ended {
                    // What was typed before the end, with no line end after it, is a line all the same.
                    if self.typed.is_empty() {
                        return Ok(Happening::EndOfInput);
                    }
                    let line = std::mem::take(&mut self.typed);
                    return Ok(Happening::Line(String::from_utf8_lossy(&line).to_string()));
                }
            }
            if typing == Typing::UntilItEnds && self.typing_ended && self.typed.is_empty() {
                return Ok(Happening::EndOfInput);
            }
            let mut waited = vec![self.interrupts, self.stream.as_raw_fd()];
            let looked_at = match typing {
                Typing::Waits => false,
                // Not while part of a message of Lever's is here and the rest on its way:
                // it comes before anything typed, as a whole one does.
                _ if !self.received.is_empty() => false,
                Typing::Wanted => true,
                // Its end is read once, and cannot be seen past anything typed before it.
                Typing::UntilItEnds => !self.typing_ended && !self.typed_ahead && self.typed.is_empty(),
            };
            if looked_at {
                waited.push(self.input);
            }
            let ready = sys::readable(&waited)?;
            if ready[0] {
                let mut byte = [0u8; 64];
                sys::read(self.interrupts, &mut byte)?;
                return Ok(Happening::Interrupt);
            }
            // What Lever sent comes before anything typed, which stays the
            // terminal's until nothing of Lever's waits: a turn another client
            // began is then on the screen before a line is taken, and Ctrl-C
            // in it drops that line as a terminal does.
            if ready[1] {
                self.receive()?;
                continue;
            }
            if ready.get(2) == Some(&true) {
                // Something typed, and no line is wanted: it is left where it is.
                if typing == Typing::UntilItEnds && sys::waiting(self.input).is_some_and(|count| count > 0) {
                    self.typed_ahead = true;
                    continue;
                }
                let mut bytes = [0u8; 4096];
                match sys::read(self.input, &mut bytes)? {
                    0 => self.typing_ended = true,
                    count => self.typed.extend_from_slice(&bytes[..count]),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_taken_whole_however_its_bytes_arrive() {
        let bytes = b"\x00\x00\x00\x0e{\"reply\":\"ok\"}\x00\x00\x00\x10{\"event\":\"done\"}";
        let mut received = Vec::new();
        let mut messages = Vec::new();
        for byte in bytes {
            received.push(*byte);
            while let Some(message) = framed(&mut received).unwrap() {
                messages.push(message);
            }
        }
        assert_eq!(messages, [json!({"reply": "ok"}), json!({"event": "done"})]);
        assert!(received.is_empty());
    }

    fn against(requests: usize, replies: &'static [&'static str]) -> (Connection, [UnixStream; 2]) {
        against_then(requests, replies, false)
    }

    /// As `against`, and with `close`, Lever then says no more: a message
    /// waited for that does not come is an error, not a wait for ever.
    fn against_then(requests: usize, replies: &'static [&'static str], close: bool) -> (Connection, [UnixStream; 2]) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let (quiet, kept) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut length = [0u8; 4];
            for _ in 0..requests {
                theirs.read_exact(&mut length).unwrap();
                theirs
                    .read_exact(&mut vec![0u8; u32::from_be_bytes(length) as usize])
                    .unwrap();
            }
            for reply in replies {
                let framed = [&(reply.len() as u32).to_be_bytes(), reply.as_bytes()].concat();
                theirs.write_all(&framed).unwrap();
            }
            if close {
                let _ = theirs.shutdown(std::net::Shutdown::Write);
            }
            let _ = theirs.read(&mut length);
        });
        (Connection::over(ours, quiet.as_raw_fd()), [quiet, kept])
    }

    #[test]
    fn a_reply_to_a_request_that_was_not_waited_for_is_not_taken_for_the_next_requests() {
        let (mut lever, _quiet) = against(
            2,
            &[
                r#"{"reply":"ok"}"#,
                r#"{"event":"stopped","thread":"t-1","turn":1}"#,
                r#"{"reply":"sent","thread":"t-1","turn":2}"#,
            ],
        );
        lever.send(json!({"op": "stop"})).unwrap();
        assert_eq!(
            lever.request(json!({"op": "send"})).unwrap(),
            json!({"reply": "sent", "thread": "t-1", "turn": 2})
        );
        assert!(
            matches!(lever.next(Typing::Waits).unwrap(), Happening::Event(event) if event == json!({"event": "stopped", "thread": "t-1", "turn": 1}))
        );

        let (mut lever, _quiet) = against(1, &[r#"{"reply":"ok"}"#, r#"{"reply":"sent","thread":"t-1","turn":2}"#]);
        lever.send(json!({"op": "stop"})).unwrap();
        assert!(
            matches!(lever.next(Typing::Waits).unwrap(), Happening::Event(reply) if reply == json!({"reply": "ok"}))
        );
        assert_eq!(
            lever.request(json!({"op": "send"})).unwrap(),
            json!({"reply": "sent", "thread": "t-1", "turn": 2})
        );
    }

    #[test]
    fn each_stop_sets_aside_its_turns_waiting_requests_until_lever_replies_to_it() {
        let request = |id: &str, turn: u64| {
            format!(r#"{{"event":"request","thread":"t-1","turn":{turn},"request":"{id}","state":"waiting"}}"#)
        };
        let frames: Vec<String> = vec![
            request("r-2", 1),
            request("r-5", 3),
            r#"{"reply":"ok"}"#.to_string(),
            request("r-6", 1),
            r#"{"reply":"ok"}"#.to_string(),
            request("r-7", 3),
        ];
        let frames: &'static [&'static str] = Box::leak(
            frames
                .into_iter()
                .map(|f| &*Box::leak(f.into_boxed_str()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let (mut lever, _quiet) = against_then(2, frames, true);
        // Two Stops go before Lever replies to either.
        lever.stop("t-1", 1).unwrap();
        lever.stop("t-1", 3).unwrap();
        let mut given = Vec::new();
        for _ in 0..4 {
            match lever.next(Typing::Waits).unwrap() {
                Happening::Event(event) => given.push(
                    event
                        .get("request")
                        .and_then(Value::as_str)
                        .unwrap_or("reply")
                        .to_string(),
                ),
                _ => panic!("only Lever speaks here"),
            }
        }
        // What each Stop withdraws is set aside; what comes after its reply is not.
        assert_eq!(given, ["reply", "r-6", "reply", "r-7"]);
    }

    #[test]
    fn what_lever_sent_is_given_before_anything_typed_is_read() {
        let (mut lever, _quiet) = against(0, &[r#"{"event":"message","thread":"t-1","turn":2}"#]);
        let (typing, mut keys) = UnixStream::pair().unwrap();
        lever.input = typing.as_raw_fd();
        keys.write_all(b"a draft\n").unwrap();
        sys::readable(&[lever.stream.as_raw_fd()]).unwrap();
        assert!(
            matches!(lever.next(Typing::Wanted).unwrap(), Happening::Event(event) if event == json!({"event": "message", "thread": "t-1", "turn": 2}))
        );
        assert_eq!(sys::waiting(lever.input), Some(8));
        assert!(lever.typed.is_empty());
        lever
            .events
            .push_back(json!({"event": "done", "thread": "t-1", "turn": 2}));
        lever.typed_ahead = true;
        assert!(matches!(lever.next(Typing::Wanted).unwrap(), Happening::Event(_)));
        assert!(!lever.typed_ahead);
        assert!(matches!(lever.next(Typing::Wanted).unwrap(), Happening::Line(line) if line == "a draft"));
    }

    #[test]
    fn a_message_of_levers_that_comes_in_two_parts_is_given_before_anything_typed() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let (quiet, _kept) = UnixStream::pair().unwrap();
        let mut lever = Connection::over(ours, quiet.as_raw_fd());
        let (typing, mut keys) = UnixStream::pair().unwrap();
        lever.input = typing.as_raw_fd();
        let message = r#"{"event":"message","thread":"t-1","turn":2}"#;
        let framed = [&(message.len() as u32).to_be_bytes(), message.as_bytes()].concat();
        theirs.write_all(&framed[..10]).unwrap();
        keys.write_all(b"a draft\n").unwrap();
        let rest = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            theirs.write_all(&framed[10..]).unwrap();
            theirs
        });
        assert!(
            matches!(lever.next(Typing::Wanted).unwrap(), Happening::Event(event) if event == json!({"event": "message", "thread": "t-1", "turn": 2}))
        );
        assert_eq!(sys::waiting(lever.input), Some(8));
        let _theirs = rest.join().unwrap();
    }

    #[test]
    fn what_lever_says_is_shown_with_no_character_that_drives_a_terminal() {
        assert!(matches!(
            plain("Plain words,\n\tand a tab. Ünïcödé too."),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            plain("\x1b]52;c;aGk=\x07 \x1b[2J safe\rfake\r\n\u{9b}31m\x7f\0"),
            "␛]52;c;aGk=␇ ␛[2J safefake\n\u{fffd}31m␡␀"
        );
        let message = json!({"text": "a\u{1b}[1mb"});
        assert_eq!(text(&message, "text"), "a␛[1mb");
        assert_eq!(text(&message, "missing"), "");
    }

    #[test]
    fn a_frame_too_long_is_an_error_as_soon_as_its_length_is_read() {
        let mut received = vec![0xff, 0xff, 0xff, 0xff];
        assert_eq!(framed(&mut received).unwrap_err().kind(), ErrorKind::InvalidData);
    }
}
