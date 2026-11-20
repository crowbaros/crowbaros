//! Every way a client can connect.
//!
//! There are two path sockets, one for each kind of client, and nothing else
//! (ADR 0032):
//!
//! - **The shell's socket.** Base puts it in a directory that only the
//!   compositor and the shell can enter.
//! - **The apps' socket.** Every app connects here, as the person's account.
//!   Base links it into the person's runtime directory, where
//!   `WAYLAND_DISPLAY` names it.
//!
//! On each, the compositor checks the peer's user ID as the kernel reports
//! it, and refuses any other. Nothing here reads a flag or an environment
//! variable, and there is no third way.
//!
//! A socket that cannot accept, as when the compositor has no descriptor
//! left, is left alone for [`ACCEPT_RETRY`] and then tried again. It does not
//! end the compositor.

use std::{
    fs, io,
    os::{
        fd::AsFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::{
    State,
    event_loop::{TimerKey, Token},
    identity::Identity,
    log,
};

/// How long a socket is left alone after it could not accept. The
/// connection it could not take stays ready, so without the wait the
/// compositor would try again at once, for as long as the cause lasts.
pub const ACCEPT_RETRY: Duration = Duration::from_secs(1);

/// Where one part connects, and the only user ID accepted there.
#[derive(Clone, Debug)]
pub struct TrustedPart {
    /// The path socket. Base makes its directory and decides who can enter.
    pub socket: PathBuf,
    /// The only user ID the compositor accepts on that socket.
    pub uid: u32,
}

/// The two sockets Wayland clients connect to.
#[derive(Clone, Debug)]
pub struct TrustedParts {
    /// The shell's socket, and the shell's user ID.
    pub shell: TrustedPart,
    /// The apps' socket, and the person's user ID.
    pub apps: TrustedPart,
}

pub(crate) struct PathListener {
    listener: UnixListener,
    uid: u32,
    identity: Identity,
}

impl State {
    /// Starts listening for the shell and for the person's apps.
    pub fn listen(&mut self, parts: &TrustedParts) -> io::Result<()> {
        self.listen_for(&parts.shell, Identity::Shell)?;
        self.listen_for(&parts.apps, Identity::App)?;
        Ok(())
    }

    fn listen_for(&mut self, part: &TrustedPart, identity: Identity) -> io::Result<()> {
        match fs::remove_file(&part.socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&part.socket)?;
        // The owner and the group of the socket's directory may connect. The
        // check that counts is the peer's user ID, below.
        fs::set_permissions(&part.socket, fs::Permissions::from_mode(0o660))?;
        listener.set_nonblocking(true)?;
        let index = self.listeners.len();
        self.event_loop.add(listener.as_fd(), Token::Listener(index), false)?;
        self.listeners.push(PathListener {
            listener,
            uid: part.uid,
            identity,
        });
        Ok(())
    }

    pub(crate) fn accept(&mut self, index: usize) {
        loop {
            let Some(part) = self.listeners.get(index) else {
                return;
            };
            match part.listener.accept() {
                Ok((stream, _)) => {
                    let (uid, identity) = (part.uid, part.identity);
                    match peer_uid(&stream) {
                        Ok(peer) if peer == uid => {
                            if let Err(over) = self.may_connect(identity) {
                                // Dropping the stream closes it, so it waits
                                // in no queue. The client sees its
                                // connection end.
                                log::warning!("refused a connection ({identity:?}): {over}");
                                continue;
                            }
                            self.add_client(stream, identity);
                        }
                        // Dropping the stream closes it. The peer learns
                        // nothing else.
                        Ok(peer) => log::warning!("refused a peer with the wrong user ID {peer} ({identity:?})"),
                        Err(error) => {
                            log::warning!(
                                "refused a peer whose user ID the kernel did not report ({identity:?}): {error}"
                            )
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    log::warning!("could not accept a connection ({:?}): {error}", part.identity);
                    self.failed_accepts += 1;
                    self.event_loop.remove(part.listener.as_fd());
                    self.event_loop
                        .set_timer(Instant::now() + ACCEPT_RETRY, TimerKey::RetryListener(index));
                    return;
                }
            }
        }
    }

    pub(crate) fn retry_listener(&mut self, index: usize) {
        if let Some(part) = self.listeners.get(index)
            && let Err(error) = self
                .event_loop
                .add(part.listener.as_fd(), Token::Listener(index), false)
        {
            log::warning!("could not listen again: {error}");
        }
    }

    fn add_client(&mut self, stream: UnixStream, identity: Identity) {
        self.next_client += 1;
        let id = self.next_client;
        let client = match crate::client::Client::new(id, stream, identity) {
            Ok(client) => client,
            Err(error) => {
                log::warning!("could not add a client: {error}");
                return;
            }
        };
        if let Err(error) = self
            .event_loop
            .add(client.conn.stream().as_fd(), Token::Client(id), false)
        {
            log::warning!("could not watch a client: {error}");
            return;
        }
        log::debug!("client {id} connected as {:?}", client.identity);
        self.clients.insert(id, client);
    }
}

/// The user ID of the process at the other end, as the kernel reports it.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    Ok(rustix::net::sockopt::socket_peercred(stream)?.uid.as_raw())
}
