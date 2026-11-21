//! The unlock helper, `crowbar-unlock`.
//!
//! It is the compositor's unlock authority (ADR 0032): the compositor listens
//! for it at a socket of its own, checked against root's user ID, and once it
//! does, the shell's word no longer unlocks.
//!
//! The shell's lock screen sends the password the person typed, with nothing
//! before it. The helper checks it against the person's account, the one
//! with user ID 1003, in /etc/shadow, through libcrypt, and only then tells
//! the compositor to unlock. A wrong password costs a pause before the next
//! one is heard, one at a time.
//!
//! `crowbar-unlock lock` asks the compositor to lock, as the suspend hook
//! does. `crowbar-unlock hash PASSWORD` prints a hash for /etc/shadow, as the
//! image build sets the lab's password.

use crate::{crypt, request, sys};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

const SOCKET: &str = "/run/crowbar-unlock/socket";
const COMPOSITOR: &str = "/run/crowbar-trusted/unlock/compositor";
const SHELL: &str = "crowbar-shell";
const WRONG_PAUSE: Duration = Duration::from_secs(2);

pub const MAX_PASSWORD: usize = 1024;

/// What the helper asks of the system, so a test can stand in for it.
pub trait Authority {
    /// The person's password hash from /etc/shadow, or None when there is no
    /// person's account.
    fn stored_hash(&self) -> Option<Vec<u8>>;
    /// Sends `lock` or `unlock` to the compositor.
    fn tell_compositor(&mut self, op: &str) -> Result<(), String>;
    /// The wait after a wrong password, before the next is heard.
    fn pause(&mut self);
}

/// Whether the password matches the stored hash. A locked or empty field
/// matches nothing: `!`, `*`, or nothing at all.
pub fn password_matches(password: &str, stored: Option<&[u8]>) -> bool {
    match stored {
        Some(stored) if !stored.is_empty() && !stored.starts_with(b"!") && !stored.starts_with(b"*") => {
            crypt::matches(password, stored)
        }
        _ => false,
    }
}

/// The hash /etc/shadow's contents hold for this account.
pub fn shadow_hash(shadow: &[u8], account: &str) -> Option<Vec<u8>> {
    shadow.split(|&b| b == b'\n').find_map(|line| {
        let mut fields = line.split(|&b| b == b':');
        (fields.next()? == account.as_bytes())
            .then(|| fields.next())
            .flatten()
            .map(<[u8]>::to_vec)
    })
}

/// The reply to one connection from a peer with this user ID. Only the
/// shell's is read.
pub fn answer(peer: u32, shell_uid: u32, stream: &mut impl Read, authority: &mut impl Authority) -> Value {
    let reply = (|| {
        if peer != shell_uid {
            return Err("Only the shell may ask to unlock".to_string());
        }
        let request = request::parse_object(&request::read_line(stream, request::MAX_REQUEST)?)?;
        let password = match (request.len(), request.get("password")) {
            (1, Some(Value::String(password))) => password,
            _ => return Err("A request is the password, and nothing else".into()),
        };
        if password.chars().count() > MAX_PASSWORD || !password_matches(password, authority.stored_hash().as_deref()) {
            authority.pause();
            return Err("Wrong password".into());
        }
        authority.tell_compositor("unlock")
    })();
    match reply {
        Ok(()) => json!({"ok": true}),
        Err(error) => request::refusal(error),
    }
}

/// Answers one connection on the helper's socket, and logs whether it
/// unlocked.
pub fn serve_one(listener: &UnixListener, shell_uid: u32, authority: &mut impl Authority) {
    let Ok((mut connection, _)) = listener.accept() else {
        return;
    };
    let _ = connection.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = connection.set_write_timeout(Some(Duration::from_secs(2)));
    let reply = match sys::peer_uid(&connection) {
        Ok(peer) => answer(peer, shell_uid, &mut connection, authority),
        Err(error) => request::refusal(error),
    };
    println!("UNLOCK ok={}", reply["ok"]);
    let _ = connection.write_all(format!("{reply}\n").as_bytes());
}

/// The system itself: /etc/shadow, the compositor's unlock socket, and a
/// real pause.
struct System;

impl Authority for System {
    fn stored_hash(&self) -> Option<Vec<u8>> {
        let person = sys::account_by_uid(crate::PERSON_UID).ok()??;
        shadow_hash(&std::fs::read("/etc/shadow").ok()?, &person.name)
    }
    fn tell_compositor(&mut self, op: &str) -> Result<(), String> {
        tell_compositor(Path::new(COMPOSITOR), op)
    }
    fn pause(&mut self) {
        std::thread::sleep(WRONG_PAUSE);
    }
}

/// Sends `lock` or `unlock` to the compositor at `path`, and returns its
/// refusal, if any.
pub fn tell_compositor(path: &Path, op: &str) -> Result<(), String> {
    let reply = (|| -> std::io::Result<Vec<u8>> {
        let mut connection = UnixStream::connect(path)?;
        connection.set_read_timeout(Some(Duration::from_secs(5)))?;
        connection.set_write_timeout(Some(Duration::from_secs(5)))?;
        connection.write_all(format!("{}\n", json!({ "op": op })).as_bytes())?;
        let mut line = Vec::new();
        std::io::BufRead::read_until(
            &mut std::io::BufReader::new(connection.take(request::MAX_REQUEST as u64 + 1)),
            b'\n',
            &mut line,
        )?;
        Ok(line)
    })()
    .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_slice(&reply).map_err(|e| e.to_string())?;
    if reply["reply"] == "ok" {
        return Ok(());
    }
    Err(reply["message"]
        .as_str()
        .filter(|m| !m.is_empty())
        .unwrap_or("The compositor refused")
        .into())
}

/// `crowbar-unlock [lock | hash PASSWORD]`.
pub fn main(args: &[String]) -> Result<(), String> {
    match args {
        [] => {
            let shell = sys::require(sys::account_by_name(SHELL), SHELL)?;
            let listener = sys::listen_as(Path::new(SOCKET), 0, shell.gid).map_err(|e| e.to_string())?;
            loop {
                serve_one(&listener, shell.uid, &mut System);
            }
        }
        [op] if op == "lock" => tell_compositor(Path::new(COMPOSITOR), "lock"),
        [op, password] if op == "hash" => {
            let hashed = crypt::hash(password).ok_or("libcrypt made no hash")?;
            println!("{}", String::from_utf8_lossy(&hashed));
            Ok(())
        }
        _ => Err("usage: crowbar-unlock [lock | hash PASSWORD]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypt;
    use serde_json::json;

    const SHELL_UID: u32 = 1001;

    /// Stands in for /etc/shadow, the compositor, and the pause.
    struct Fake {
        stored: Option<Vec<u8>>,
        told: Vec<String>,
        pauses: usize,
        sleep: std::time::Duration,
    }

    impl Fake {
        fn new(password: &str) -> Fake {
            Fake {
                stored: crypt::hash(password),
                told: vec![],
                pauses: 0,
                sleep: Default::default(),
            }
        }
    }

    impl Authority for Fake {
        fn stored_hash(&self) -> Option<Vec<u8>> {
            self.stored.clone()
        }
        fn tell_compositor(&mut self, op: &str) -> Result<(), String> {
            self.told.push(op.into());
            Ok(())
        }
        fn pause(&mut self) {
            self.pauses += 1;
            std::thread::sleep(self.sleep);
        }
    }

    fn ask(fake: &mut Fake, password: &str) -> serde_json::Value {
        let line = format!("{}\n", json!({ "password": password }));
        answer(SHELL_UID, SHELL_UID, &mut line.as_bytes(), fake)
    }

    #[test]
    fn the_password_unlocks_and_nothing_else_does() {
        let mut fake = Fake::new("crowbar");
        assert_eq!(ask(&mut fake, "wrong"), json!({"ok": false, "error": "Wrong password"}));
        assert_eq!(ask(&mut fake, ""), json!({"ok": false, "error": "Wrong password"}));
        assert!(fake.told.is_empty());
        assert_eq!(ask(&mut fake, "crowbar"), json!({"ok": true}));
        assert_eq!(fake.told, ["unlock"]);
    }

    #[test]
    fn a_wrong_password_costs_a_pause_and_the_right_one_none() {
        let mut fake = Fake::new("crowbar");
        ask(&mut fake, "wrong");
        ask(&mut fake, "also wrong");
        assert_eq!(fake.pauses, 2);
        ask(&mut fake, "crowbar");
        assert_eq!(fake.pauses, 2);
    }

    #[test]
    fn a_locked_or_empty_account_matches_nothing() {
        let stored = crypt::hash("crowbar").unwrap();
        let locked = [b"!".as_slice(), &stored].concat();
        for stored in [Some(locked), Some(b"*".to_vec()), Some(vec![]), None] {
            assert!(!password_matches("crowbar", stored.as_deref()), "{stored:?}");
            assert!(!password_matches("", stored.as_deref()), "{stored:?}");
        }
    }

    #[test]
    fn an_account_from_the_account_file_does_not_take_the_lab_password() {
        let mut fake = Fake::new("their own");
        assert_eq!(
            ask(&mut fake, "crowbar"),
            json!({"ok": false, "error": "Wrong password"})
        );
        assert_eq!(ask(&mut fake, "their own"), json!({"ok": true}));
    }

    #[test]
    fn with_no_person_account_nothing_matches() {
        let mut fake = Fake::new("crowbar");
        fake.stored = None;
        assert_eq!(ask(&mut fake, "crowbar")["ok"], false);
        assert!(fake.told.is_empty());
    }

    #[test]
    fn a_request_is_the_password_and_nothing_else() {
        let mut fake = Fake::new("crowbar");
        for line in [
            "{}\n",
            "{\"password\":1}\n",
            "{\"password\":\"crowbar\",\"account\":\"root\"}\n",
            "[\"crowbar\"]\n",
            "{\"password\":\"crowbar\"}",
        ] {
            let reply = answer(SHELL_UID, SHELL_UID, &mut line.as_bytes(), &mut fake);
            assert_eq!(reply["ok"], false, "{line}");
            assert_ne!(reply["error"], "Wrong password", "{line}");
        }
        let long = "x".repeat(MAX_PASSWORD + 1);
        assert_eq!(ask(&mut fake, &long), json!({"ok": false, "error": "Wrong password"}));
        let huge = format!("{}\n", json!({ "password": "x".repeat(request::MAX_REQUEST) }));
        assert_eq!(
            answer(SHELL_UID, SHELL_UID, &mut huge.as_bytes(), &mut fake)["ok"],
            false
        );
        assert!(fake.told.is_empty());
    }

    #[test]
    fn only_the_shell_may_ask_to_unlock() {
        let mut fake = Fake::new("crowbar");
        for peer in [0, 1000, 1002, 1003] {
            let reply = answer(
                peer,
                SHELL_UID,
                &mut b"{\"password\":\"crowbar\"}\n".as_slice(),
                &mut fake,
            );
            assert_eq!(reply, json!({"ok": false, "error": "Only the shell may ask to unlock"}));
        }
        assert!(fake.told.is_empty());
    }

    #[test]
    fn the_persons_hash_is_found_by_name_in_shadow() {
        let shadow = b"root:!:19000::::::\njim:$6$salt$hash:19000:0:99999:7:::\njimmy:*:1::::::\n";
        assert_eq!(shadow_hash(shadow, "jim").as_deref(), Some(b"$6$salt$hash".as_slice()));
        assert_eq!(shadow_hash(shadow, "jimmy").as_deref(), Some(b"*".as_slice()));
        assert_eq!(shadow_hash(shadow, "ji"), None);
        assert_eq!(shadow_hash(shadow, "alex"), None);
    }

    /// Asks a helper serving at `path` with one password, as the shell does.
    /// None when the helper hung up without a reply: a refused peer's request
    /// goes unread, and closing with it unread may reset the connection.
    fn ask_socket(path: &std::path::Path, password: &str) -> Option<serde_json::Value> {
        let mut stream = UnixStream::connect(path).unwrap();
        writeln!(stream, "{}", json!({ "password": password })).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        Some(serde_json::from_str(&reply).unwrap())
    }

    #[test]
    fn over_its_socket_the_helper_hears_one_attempt_at_a_time() {
        let dir = crate::sys::tests::TempDir::new();
        let path = dir.0.join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut fake = Fake::new("crowbar");
        fake.sleep = std::time::Duration::from_millis(300);
        let start = std::time::Instant::now();
        let askers: Vec<_> = (0..3)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || (ask_socket(&path, "wrong"), start.elapsed()))
            })
            .collect();
        for _ in 0..3 {
            serve_one(&listener, crate::sys::uid(), &mut fake);
        }
        let mut waits: Vec<_> = askers.into_iter().map(|a| a.join().unwrap()).collect();
        waits.sort_by_key(|(_, wait)| *wait);
        assert!(
            waits
                .iter()
                .all(|(reply, _)| reply.as_ref().unwrap()["error"] == "Wrong password")
        );
        // Each wrong password is paused for before the next is heard.
        assert!(waits[2].1 >= std::time::Duration::from_millis(900), "{waits:?}");
        assert_eq!(fake.pauses, 3);
    }

    #[test]
    fn over_its_socket_the_helper_refuses_anyone_but_the_shell() {
        let dir = crate::sys::tests::TempDir::new();
        let path = dir.0.join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut fake = Fake::new("crowbar");
        let asker = std::thread::spawn(move || ask_socket(&path, "crowbar"));
        serve_one(&listener, crate::sys::uid() + 1, &mut fake);
        if let Some(reply) = asker.join().unwrap() {
            assert_eq!(reply, json!({"ok": false, "error": "Only the shell may ask to unlock"}));
        }
        assert!(fake.told.is_empty());
    }
}
