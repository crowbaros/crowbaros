//! Who a client is.
//!
//! An identity is fixed when the compositor accepts the connection. It comes
//! from which socket the client reached and the user ID the kernel reports
//! for its peer. Nothing a client says about itself changes it (ADR 0032).

/// Who a client is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// The shell. It draws the bar, the dock, the launcher, the wallpaper,
    /// and the lock screen.
    Shell,
    /// One of the person's apps, from the apps' socket.
    App,
}
