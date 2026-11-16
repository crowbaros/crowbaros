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
    cell::Cell,
    fs, io,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use smithay::{
    reexports::calloop::{
        EventSource, Interest, Mode, Poll, PostAction, Readiness, RegistrationToken, Token, TokenFactory,
        generic::Generic,
        timer::{TimeoutAction, Timer},
    },
    wayland::security_context::{SecurityContext, SecurityContextHandler, SecurityContextListenerSource},
};

use crate::{
    State,
    identity::{self, ClientState, Compartment, Identity},
};

/// Where a trusted part connects, and the user ID it runs as.
#[derive(Clone, Debug)]
pub struct TrustedPart {
    /// The path socket. Base makes its directory and decides who can enter.
    pub socket: PathBuf,
    /// The only user ID the compositor accepts on that socket.
    pub uid: u32,
}

/// How long a listener is left alone after it could not accept. The
/// connection it could not take stays ready, so without the wait the
/// compositor would try again at once, for as long as the cause lasts.
const ACCEPT_RETRY: Duration = Duration::from_secs(1);

/// Where a listener's place in the event loop is kept, for the listener's
/// own callback to name it. It is set once the listener is in the loop.
type OwnToken = Rc<Cell<Option<RegistrationToken>>>;

/// The two trusted parts that are Wayland clients.
#[derive(Clone, Debug)]
pub struct TrustedParts {
    pub shell: TrustedPart,
    pub warden: TrustedPart,
}

impl State {
    /// Starts listening for the shell and the Warden.
    pub fn listen(&mut self, parts: &TrustedParts) -> io::Result<()> {
        self.listen_for(&parts.shell, Identity::Shell)?;
        self.listen_for(&parts.warden, Identity::Warden)
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
        let uid = part.uid;
        let own = OwnToken::default();
        let token = own.clone();
        let registered = self
            .loop_handle
            .insert_source(
                Generic::new(listener, Interest::READ, Mode::Level),
                move |_, listener, state| {
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => state.accept_trusted(stream, uid, &identity),
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) => {
                                tracing::warn!(%error, ?identity, "could not accept a connection");
                                return Ok(state.accept_again_later(&token));
                            }
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
        own.set(Some(registered));
        Ok(())
    }

    /// Leaves a listener that could not accept alone for [`ACCEPT_RETRY`].
    /// Returns what the listener's callback tells the event loop.
    fn accept_again_later(&mut self, listener: &OwnToken) -> PostAction {
        let Some(token) = listener.get() else {
            return PostAction::Continue;
        };
        let timer = Timer::from_duration(ACCEPT_RETRY);
        let waiting = self.loop_handle.insert_source(timer, move |_, _, state| {
            if let Err(error) = state.loop_handle.enable(&token) {
                tracing::warn!(%error, "could not listen again");
            }
            TimeoutAction::Drop
        });
        match waiting {
            Ok(_) => PostAction::Disable,
            // With no timer to end the wait, the listener goes on at once.
            Err(error) => {
                tracing::warn!(%error, "could not wait before accepting again");
                PostAction::Continue
            }
        }
    }

    fn accept_trusted(&mut self, stream: UnixStream, uid: u32, identity: &Identity) {
        match peer_uid(&stream) {
            Ok(peer) if peer == uid => self.accept(stream, identity.clone()),
            // Dropping the stream closes it. The peer learns nothing else.
            Ok(peer) => tracing::warn!(peer, ?identity, "refused a peer with the wrong user ID"),
            Err(error) => tracing::warn!(%error, ?identity, "refused a peer whose user ID the kernel did not report"),
        }
    }

    fn accept(&mut self, stream: UnixStream, identity: Identity) {
        if let Err(error) = self
            .display_handle
            .insert_client(stream, Arc::new(ClientState::new(identity)))
        {
            tracing::warn!(%error, "could not add a client");
        }
    }

    /// The live compartment with this compartment ID, if the Warden has
    /// registered its listener.
    pub fn compartment(&self, id: &str) -> Option<Arc<Compartment>> {
        self.compartments
            .iter()
            .filter_map(|compartment| compartment.upgrade())
            .find(|compartment| compartment.id() == id)
    }
}

/// The user ID of the process at the other end, as the kernel reports it.
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    Ok(rustix::net::sockopt::socket_peercred(stream)?.uid.as_raw())
}

impl SecurityContextHandler for State {
    /// The Warden registered a listener for one compartment. Returning
    /// without keeping `source` closes the listener, so it accepts nothing.
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        // The table offers the security context manager to the Warden alone,
        // so no other client can reach this while the table holds. This is
        // the same check again, where the listener is taken, and no test can
        // reach its refusal from outside.
        let from_warden = self
            .display_handle
            .backend_handle()
            .get_client_data(context.creator_client_id.clone())
            .is_ok_and(|data| matches!(identity::of_data(&*data), Some(Identity::Warden)));
        if !from_warden {
            tracing::warn!("refused a compartment listener that did not come from the Warden");
            return;
        }
        let named = |field: Option<String>| field.filter(|text| !text.is_empty());
        let (Some(principal), Some(id)) = (named(context.app_id), named(context.instance_id)) else {
            tracing::warn!("refused a compartment listener with no principal ID or no compartment ID");
            return;
        };
        if self.compartment(&id).is_some() {
            tracing::warn!(compartment = %id, "refused a second listener for a live compartment");
            return;
        }
        let compartment = Arc::new(Compartment::new(principal, id));
        self.compartments.retain(|known| known.strong_count() > 0);
        self.compartments.push(Arc::downgrade(&compartment));
        tracing::info!(principal = %compartment.principal(), compartment = %compartment.id(), "compartment listener registered");
        let own = OwnToken::default();
        let token = own.clone();
        let inserted = self
            .loop_handle
            .insert_source(CompartmentListener(source), move |accepted, _, state| match accepted {
                Ok(stream) => {
                    state.accept(stream, Identity::Compartment(compartment.clone()));
                    PostAction::Continue
                }
                Err(error) => {
                    tracing::warn!(%error, compartment = %compartment.id(), "could not accept a connection");
                    state.accept_again_later(&token)
                }
            });
        match inserted {
            Ok(registered) => own.set(Some(registered)),
            Err(error) => tracing::warn!(%error, "could not listen for a compartment"),
        }
    }
}

/// A compartment's listener. Smithay's source ends the event loop, and with
/// it the compositor, when it cannot accept. This one hands the error to its
/// callback, which says what the loop does with the listener.
struct CompartmentListener(SecurityContextListenerSource);

impl EventSource for CompartmentListener {
    type Event = io::Result<UnixStream>;
    type Metadata = ();
    type Ret = PostAction;
    type Error = io::Error;

    fn process_events<F: FnMut(Self::Event, &mut ()) -> PostAction>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> io::Result<PostAction> {
        let processed = self.0.process_events(readiness, token, |stream, _| {
            callback(Ok(stream), &mut ());
        });
        match processed {
            Ok(action) => Ok(action),
            Err(error) => Ok(callback(Err(error), &mut ())),
        }
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.0.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.0.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> smithay::reexports::calloop::Result<()> {
        self.0.unregister(poll)
    }
}
