// SPDX-License-Identifier: MIT
//! The shell-wide event bus: wakes GPUI only when an event arrives.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MicMute,
    BrightnessUp,
    BrightnessDown,
}

#[derive(Clone, Debug)]
pub enum Event {
    Wallpaper(Option<crate::appearance::CustomWallpaper>),
    Themes(crate::appearance::ThemeCatalog),
    Toggle,
    Picker(crate::clipboard_picker::Kind),
    ClipboardChanged,
    /// The person made the summon gesture.
    Summon(crate::compositor::Summon),
    /// A hardware volume, mute, or brightness key was pressed.
    MediaKey(MediaKey),
    /// Updated Quick Settings hardware status from the launcher service.
    QuickStatus(crate::quick_settings::QuickStatus),
    /// A transient OSD update triggered by a hardware key or Quick Settings.
    Osd {
        kind: crate::quick_settings::OsdKind,
        percent: u32,
        muted: bool,
    },
    /// The compositor locked the session, or unlocked it.
    Lock(bool),
    Quit,
    Snapshot(crate::compositor::Snapshot),
    Launch(Result<(), String>),
    /// Launching the Agent's sign-in desktop entry through Base's launcher service.
    AgentLaunch(Result<(), String>),
    /// The unlock helper's answer to the person's password: `Ok` once the
    /// compositor unlocked, or why not.
    Unlock(Result<(), String>),
    /// An event from Lever on the surface socket.
    Agent(crate::agent::AgentEvent),
}

/// The shell's events, from any thread to the interface. Sending one wakes
/// the interface at once: it does not look for them on a timer.
#[derive(Clone)]
pub struct Sender(futures::channel::mpsc::UnboundedSender<Event>);

/// Where the interface takes the shell's events.
pub type Receiver = futures::channel::mpsc::UnboundedReceiver<Event>;

impl Sender {
    /// Sends an event. An error once the interface has gone.
    pub fn send(&self, event: Event) -> Result<(), Event> {
        self.0
            .unbounded_send(event)
            .map_err(|error| error.into_inner())
    }
}

/// A new channel for the shell's events.
pub fn channel() -> (Sender, Receiver) {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    (Sender(tx), rx)
}
