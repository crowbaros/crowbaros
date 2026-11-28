// SPDX-License-Identifier: MIT
//! Client for Base's unlock helper (`CROWBAR_UNLOCK_SOCKET`, ADR 0032).

use anyhow::{Context, Result, bail};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use crate::events::{Event, Sender};

#[derive(Clone, Debug, Default)]
pub struct UnlockClient {
    socket: Option<PathBuf>,
}

impl UnlockClient {
    pub fn from_env() -> Self {
        Self {
            socket: std::env::var_os("CROWBAR_UNLOCK_SOCKET").map(PathBuf::from),
        }
    }

    /// Whether an unlock helper checks the person's password. Where one does,
    /// the compositor takes its word to unlock, and not the shell's.
    pub fn unlocks_with_password(&self) -> bool {
        self.socket.is_some()
    }

    /// Sends the person's password to the unlock helper, which checks it and
    /// tells the compositor to unlock; the answer comes back as
    /// [`Event::Unlock`]. The password is not kept.
    pub fn unlock_with_password(&self, password: String, tx: Sender) {
        let socket = self.socket.clone();
        std::thread::spawn(move || {
            let result = socket
                .context("No unlock helper")
                .and_then(|path| {
                    UnixStream::connect(path).context("The unlock helper is not running")
                })
                .and_then(|stream| ask_unlock_helper(stream, password));
            log::info!("SHELL unlock ok={}", result.is_ok());
            let _ = tx.send(Event::Unlock(result.map_err(|e| e.to_string())));
        });
    }
}

/// One request to the unlock helper: the password, and its answer.
fn ask_unlock_helper(mut stream: UnixStream, password: String) -> Result<()> {
    // A wrong password costs a pause before the helper answers.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut body = serde_json::to_vec(&serde_json::json!({ "password": password }))?;
    drop(password);
    body.push(b'\n');
    let sent = stream.write_all(&body);
    body.fill(0);
    sent?;
    let mut response = String::new();
    BufReader::new(stream).take(4097).read_line(&mut response)?;
    if response.len() > 4096 || !response.ends_with('\n') {
        bail!("The unlock helper did not answer")
    }
    let reply: serde_json::Value = serde_json::from_str(&response)?;
    if reply["ok"] != true {
        bail!("{}", reply["error"].as_str().unwrap_or("Could not unlock"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock screen sends the unlock helper the password alone, and says
    /// why it did not unlock.
    #[test]
    fn the_unlock_helper_hears_the_password_and_its_answer_is_said() {
        for (reply, expected) in [
            ("{\"ok\":true}\n", Ok(())),
            (
                "{\"ok\":false,\"error\":\"Wrong password\"}\n",
                Err("Wrong password"),
            ),
            ("{\"ok\":true}", Err("The unlock helper did not answer")),
        ] {
            let (ours, theirs) = UnixStream::pair().unwrap();
            let helper = std::thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(theirs.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                (&theirs).write_all(reply.as_bytes()).unwrap();
                line
            });
            let result = ask_unlock_helper(ours, "crowbar".into()).map_err(|e| e.to_string());
            assert_eq!(result, expected.map_err(str::to_string));
            let sent: serde_json::Value = serde_json::from_str(&helper.join().unwrap()).unwrap();
            assert_eq!(sent, serde_json::json!({ "password": "crowbar" }));
        }
    }
}
