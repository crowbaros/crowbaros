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

use std::{
    fs, io,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::Arc,
};

use smithay::{
    reexports::calloop::{Interest, Mode, PostAction, generic::Generic},
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
        self.loop_handle
            .insert_source(
                Generic::new(listener, Interest::READ, Mode::Level),
                move |_, listener, state| {
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => state.accept_trusted(stream, uid, &identity),
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) => {
                                tracing::warn!(%error, ?identity, "could not accept a connection");
                                break;
                            }
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(())
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
        // The table offers the security context manager to the Warden alone.
        // This is the same check again, where the listener is taken.
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
        let inserted = self.loop_handle.insert_source(source, move |stream, _, state| {
            state.accept(stream, Identity::Compartment(compartment.clone()));
        });
        if let Err(error) = inserted {
            tracing::warn!(%error, "could not listen for a compartment");
        }
    }
}
