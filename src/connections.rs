//! Every way a client can connect.
//!
//! There are two, and the compositor has no public socket (ADR 0016):
//!
//! - **A trusted part's path socket.** The shell and the Warden each have
//!   one. Base puts each in a directory that only its two ends can enter, and
//!   the compositor checks the peer's user ID as the kernel reports it.
//! - **A compartment's listener.** The Warden makes it, registers it through
//!   the security context protocol with the principal ID as the app ID and
//!   the compartment ID as the instance ID, and binds it into that
//!   compartment's sandbox alone.
//!
//! Nothing here reads a flag or an environment variable, and there is no
//! third way.
//!
//! A listener that cannot accept, as when the compositor has no descriptor
//! left, is left alone for [`ACCEPT_RETRY`] and then tried again. It does not
//! end the compositor.

use std::{
    borrow::Cow,
    fs, io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use fulcrum_wire::protocol::security_context_v1::{wp_security_context_manager_v1, wp_security_context_v1};
use rustix::net::SocketFlags;

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    event_loop::{TimerKey, Token},
    globals::GlobalKind,
    identity::{Compartment, Identity},
    log,
};

/// How long a listener is left alone after it could not accept. The
/// connection it could not take stays ready, so without the wait the
/// compositor would try again at once, for as long as the cause lasts.
pub const ACCEPT_RETRY: Duration = Duration::from_secs(1);

/// Where a trusted part connects, and the user ID it runs as.
#[derive(Clone, Debug)]
pub struct TrustedPart {
    /// The path socket. Base makes its directory and decides who can enter.
    pub socket: PathBuf,
    /// The only user ID the compositor accepts on that socket.
    pub uid: u32,
}

/// The two trusted parts that are Wayland clients.
#[derive(Clone, Debug)]
pub struct TrustedParts {
    pub shell: TrustedPart,
    pub warden: TrustedPart,
}

pub(crate) struct TrustedListener {
    listener: UnixListener,
    uid: u32,
    identity: Identity,
}

/// A compartment's listener, as the Warden registered it.
pub(crate) struct CompartmentListener {
    compartment: Arc<Compartment>,
    listen: OwnedFd,
    /// Its hang-up means the Warden let go of the listener.
    close: OwnedFd,
    /// Not watched while waiting to accept again.
    waiting: bool,
}

/// A security context the Warden is filling in, before its commit.
#[derive(Debug)]
pub(crate) struct PendingContext {
    listen: Option<OwnedFd>,
    close: Option<OwnedFd>,
    engine: Option<String>,
    app_id: Option<String>,
    instance_id: Option<String>,
    committed: bool,
}

impl State {
    /// Starts listening for the shell and the Warden, and offers the Warden
    /// the security context manager.
    pub fn listen(&mut self, parts: &TrustedParts) -> io::Result<()> {
        self.listen_for(&parts.shell, Identity::Shell)?;
        self.listen_for(&parts.warden, Identity::Warden)?;
        self.add_global(
            &wp_security_context_manager_v1::INFO,
            1,
            GlobalKind::SecurityContextManager,
        );
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
        let index = self.trusted.len();
        self.event_loop
            .add(listener.as_fd(), Token::TrustedListener(index), false)?;
        self.trusted.push(TrustedListener {
            listener,
            uid: part.uid,
            identity,
        });
        Ok(())
    }

    pub(crate) fn accept_trusted(&mut self, index: usize) {
        loop {
            let Some(part) = self.trusted.get(index) else {
                return;
            };
            match part.listener.accept() {
                Ok((stream, _)) => {
                    let (uid, identity) = (part.uid, part.identity.clone());
                    match peer_uid(&stream) {
                        Ok(peer) if peer == uid => self.add_client(stream, identity),
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
                    self.event_loop.remove(part.listener.as_fd());
                    self.event_loop
                        .set_timer(Instant::now() + ACCEPT_RETRY, TimerKey::RetryTrusted(index));
                    return;
                }
            }
        }
    }

    pub(crate) fn retry_trusted(&mut self, index: usize) {
        if let Some(part) = self.trusted.get(index)
            && let Err(error) = self
                .event_loop
                .add(part.listener.as_fd(), Token::TrustedListener(index), false)
        {
            log::warning!("could not listen again: {error}");
        }
    }

    /// The live compartment with this compartment ID: one whose listener the
    /// Warden still holds, or one with a client still connected.
    pub fn compartment(&self, id: &str) -> Option<Arc<Compartment>> {
        self.compartment_ids
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .find(|compartment| compartment.id() == id)
    }

    /// `wp_security_context_manager_v1` requests, from the Warden.
    pub(crate) fn security_context_manager_request(
        &mut self,
        client_id: ClientId,
        request: wp_security_context_manager_v1::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            wp_security_context_manager_v1::Request::Destroy => Ok(()),
            wp_security_context_manager_v1::Request::CreateListener {
                id,
                listen_fd,
                close_fd,
            } => {
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wp_security_context_v1::NAME),
                    Some(&wp_security_context_v1::INFO),
                    1,
                    ObjectKind::SecurityContext(Box::new(PendingContext {
                        listen: Some(listen_fd),
                        close: Some(close_fd),
                        engine: None,
                        app_id: None,
                        instance_id: None,
                        committed: false,
                    })),
                )
            }
        }
    }

    /// `wp_security_context_v1` requests: the Warden names the listener and
    /// commits it.
    pub(crate) fn security_context_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wp_security_context_v1::Request,
    ) -> Result<(), ProtocolError> {
        use wp_security_context_v1::{Request, error};
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        let Some(ObjectKind::SecurityContext(context)) = client.objects.get_mut(&object).map(|object| &mut object.kind)
        else {
            return Ok(());
        };
        let fail = |code, message: &str| ProtocolError {
            object,
            code,
            message: message.to_string(),
        };
        if context.committed && !matches!(request, Request::Destroy) {
            return Err(fail(error::ALREADY_USED, "the security context was already committed"));
        }
        let set = |field: &mut Option<String>, value: String, what: &str| {
            if field.is_some() {
                return Err(fail(error::ALREADY_SET, &format!("the {what} was already set")));
            }
            *field = Some(value);
            Ok(())
        };
        match request {
            Request::Destroy => Ok(()),
            Request::SetSandboxEngine { name } => set(&mut context.engine, name, "sandbox engine"),
            Request::SetAppId { app_id } => set(&mut context.app_id, app_id, "app ID"),
            Request::SetInstanceId { instance_id } => set(&mut context.instance_id, instance_id, "instance ID"),
            Request::Commit => {
                context.committed = true;
                let (listen, close) = (context.listen.take(), context.close.take());
                let (principal, id) = (context.app_id.clone(), context.instance_id.clone());
                let from_warden = matches!(client.identity, Identity::Warden);
                if let (Some(listen), Some(close)) = (listen, close) {
                    self.register_compartment(from_warden, principal, id, listen, close);
                }
                Ok(())
            }
        }
    }

    /// Takes a committed listener, or refuses it by closing it. A refused
    /// listener accepts nothing; the Warden learns of it when its clients
    /// cannot connect.
    fn register_compartment(
        &mut self,
        from_warden: bool,
        principal: Option<String>,
        id: Option<String>,
        listen: OwnedFd,
        close: OwnedFd,
    ) {
        // The table offers the security context manager to the Warden alone,
        // so no other client can reach this while the table holds. This is
        // the same check again, where the listener is taken.
        if !from_warden {
            log::warning!("refused a compartment listener that did not come from the Warden");
            return;
        }
        let named = |field: Option<String>| field.filter(|text| !text.is_empty());
        let (Some(principal), Some(id)) = (named(principal), named(id)) else {
            log::warning!("refused a compartment listener with no principal ID or no compartment ID");
            return;
        };
        if self.compartment(&id).is_some() {
            log::warning!("refused a second listener for the live compartment {id}");
            return;
        }
        if let Err(error) = rustix::fs::fcntl_setfl(&listen, rustix::fs::OFlags::NONBLOCK) {
            log::warning!("refused a compartment listener that could not be made nonblocking: {error}");
            return;
        }
        let compartment = Arc::new(Compartment::new(principal, id));
        self.compartment_ids.retain(|known| known.strong_count() > 0);
        self.compartment_ids.push(Arc::downgrade(&compartment));
        self.next_listener += 1;
        let number = self.next_listener;
        let watched = self
            .event_loop
            .add(listen.as_fd(), Token::CompartmentListener(number), false)
            .and_then(|()| {
                self.event_loop
                    .add(close.as_fd(), Token::CompartmentClose(number), false)
            });
        if let Err(error) = watched {
            log::warning!("could not listen for compartment {}: {error}", compartment.id());
            self.event_loop.remove(listen.as_fd());
            return;
        }
        log::info!(
            "compartment listener registered principal={} compartment={}",
            compartment.principal(),
            compartment.id()
        );
        self.listeners.insert(
            number,
            CompartmentListener {
                compartment,
                listen,
                close,
                waiting: false,
            },
        );
    }

    pub(crate) fn accept_compartment(&mut self, number: u64) {
        loop {
            let Some(listener) = self.listeners.get(&number) else {
                return;
            };
            match rustix::net::accept_with(&listener.listen, SocketFlags::CLOEXEC) {
                Ok(fd) => {
                    let identity = Identity::Compartment(listener.compartment.clone());
                    listener.compartment.client_connected();
                    self.add_client(UnixStream::from(fd), identity);
                }
                Err(rustix::io::Errno::AGAIN) => return,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => {
                    log::warning!(
                        "could not accept a connection for compartment {}: {error}",
                        listener.compartment.id()
                    );
                    self.event_loop.remove(listener.listen.as_fd());
                    if let Some(listener) = self.listeners.get_mut(&number) {
                        listener.waiting = true;
                    }
                    self.event_loop
                        .set_timer(Instant::now() + ACCEPT_RETRY, TimerKey::RetryCompartment(number));
                    return;
                }
            }
        }
    }

    pub(crate) fn retry_compartment(&mut self, number: u64) {
        let Some(listener) = self.listeners.get_mut(&number) else {
            return;
        };
        listener.waiting = false;
        if let Err(error) = self
            .event_loop
            .add(listener.listen.as_fd(), Token::CompartmentListener(number), false)
        {
            log::warning!(
                "could not listen again for compartment {}: {error}",
                listener.compartment.id()
            );
        }
    }

    /// The Warden let go of a compartment's listener: it accepts nothing more.
    pub(crate) fn close_compartment(&mut self, number: u64) {
        let Some(listener) = self.listeners.remove(&number) else {
            return;
        };
        if !listener.waiting {
            self.event_loop.remove(listener.listen.as_fd());
        }
        self.event_loop.remove(listener.close.as_fd());
        log::info!("compartment listener closed compartment={}", listener.compartment.id());
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
