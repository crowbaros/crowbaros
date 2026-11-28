//! Lock (ADR 0032): a state of the compositor, in which it shows the shell's
//! lock screen and nothing else.
//!
//! The shell's request, the idle timer, and Super+L each lock the session.
//! While it is locked, the lock screen hears the person's keys at once, so
//! that they type their password with nothing first.
//!
//! While it is locked, the shell's lock screen, its layer surface whose
//! namespace is [`LOCK_NAMESPACE`], is the only thing drawn, on the displays
//! it is on; every other display is blank. Input goes to the lock screen
//! alone: nothing else can be under the pointer or have the keyboard, and no
//! chord of the compositor's acts.
//!
//! A `lock` on the control channel is answered once every display shows the
//! session locked: each has shown a frame drawn since, blank or the lock
//! screen. The suspend hook waits for that answer, so the machine never
//! sleeps with the desktop on screen for whoever wakes it to see. Until the
//! answer, the connection hears nothing else, so its replies keep their
//! order. A display that shows no frame within [`LOCK_SHOWN_WITHIN`], as one
//! switched off, holds the answer no longer: it comes then, still `ok`, as
//! the session is locked, and the log names the display.
//!
//! An unlock authority ends it: a privileged part of Base, at a socket and
//! user ID of its own (`--unlock-socket`, `--unlock-uid`), that checks the
//! person's password and says `unlock`. Once there is one, the shell's word
//! is not enough, and the compositor can start locked (`--start-locked`).
//! Without one, the shell's request ends it, and the compositor starts
//! unlocked. ADR 0032 accepts these, the lock screen's namespace, and the
//! `lock` and `unlock` messages (see `control.rs`).

use std::time::{Duration, Instant};

use crate::{State, event_loop::TimerKey, identity::Identity, log, surface::SurfaceId};

/// The namespace of the shell's layer surface that is its lock screen.
pub const LOCK_NAMESPACE: &str = "crowbaros-lock";

/// What a locked display shows where no lock screen is: Nightwatch Void ground
/// (`#030B13`), matching the lock screen's wallpaper edge.
pub const LOCKED_BACKGROUND: u32 = 0xff03_0b13;

/// The longest a `lock` waits for its answer: for every display to show the
/// session locked. One that shows no frame by then, as one switched off,
/// does not hold it longer.
pub const LOCK_SHOWN_WITHIN: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(crate) struct Lock {
    pub locked: bool,
    /// For each output there was when the session locked, the first draw
    /// that shows it locked.
    locked_from: Vec<(u64, u64)>,
    /// How long without input before the session locks itself.
    idle_after: Option<Duration>,
    /// Optional suspend timeout in seconds (emits `idle_suspend` to the shell).
    suspend_after: Option<Duration>,
    last_input: Instant,
    timer: Option<crate::event_loop::TimerId>,
}

impl Default for Lock {
    fn default() -> Self {
        Lock {
            locked: false,
            locked_from: Vec::new(),
            idle_after: None,
            suspend_after: None,
            last_input: Instant::now(),
            timer: None,
        }
    }
}

impl State {
    /// Whether the session is locked.
    pub fn locked(&self) -> bool {
        self.lock.locked
    }

    /// Locks the session: `by` says what asked, for the log.
    pub fn lock(&mut self, by: &str) {
        if self.lock.locked {
            return;
        }
        log::info!("session locked by={by}");
        self.grab = None;
        self.cancel_drag();
        self.seat.drag_press = None;
        self.lock.locked = true;
        self.clear_clipboard_history();
        self.arm_idle_timer();
        // Every draw from the next on shows the session locked.
        self.lock.locked_from = self
            .outputs
            .iter()
            .map(|output| (output.number, output.draws + 1))
            .collect();
        self.refresh_keyboard_focus();
        self.seat.pointer_focus = None;
        self.seat.cursor = None;
        self.refresh_pointer_focus();
        self.control_event("locked");
        self.queue_redraw_all();
    }

    /// Unlocks the desktop, as the unlock authority, or the shell where there
    /// is none, asks. The window that had the keyboard has it back, or the
    /// Agent's surface if it had it.
    pub fn unlock(&mut self) {
        if !self.lock.locked {
            return;
        }
        log::info!("session unlocked");
        self.lock.locked = false;
        self.lock.locked_from.clear();
        let before = self
            .seat
            .desktop_keyboard_focus
            .map(|focus| self.root_of(focus))
            .filter(|focus| self.agent_surface() == Some(*focus) || self.window_is_shown(*focus));
        self.set_keyboard_focus(before);
        self.refresh_pointer_focus();
        self.control_event("unlocked");
        self.input_seen();
        self.arm_idle_timer();
        self.queue_redraw_all();
    }

    /// Whether every display shows the session locked, or it is not locked:
    /// each output there was when it locked has shown a frame drawn since,
    /// blank or the lock screen. An output added since draws nothing else.
    pub(crate) fn lock_shown(&self) -> bool {
        self.lock_not_shown_on().is_empty()
    }

    /// The outputs that do not yet show the session locked, by name.
    pub(crate) fn lock_not_shown_on(&self) -> Vec<String> {
        self.lock
            .locked_from
            .iter()
            .filter_map(|(number, draw)| {
                self.outputs
                    .iter()
                    .find(|output| output.number == *number && output.presented < *draw)
                    .map(|output| output.name.clone())
            })
            .collect()
    }

    /// Whether a surface belongs to the lock screen: the shell's layer
    /// surface of the lock namespace, or one of its subsurfaces.
    pub(crate) fn is_lock_screen(&self, surface: SurfaceId) -> bool {
        let root = self.root_of(surface);
        let from_shell = self
            .surfaces
            .get(&root)
            .and_then(|surface| self.clients.get(&surface.client))
            .is_some_and(|client| matches!(client.identity, Identity::Shell));
        from_shell
            && self
                .layer_of(root)
                .is_some_and(|layer| layer.namespace == LOCK_NAMESPACE)
    }

    /// Keep the focused lock screen, or use the first mapped one.
    pub(crate) fn lock_screen(&self) -> Option<SurfaceId> {
        self.mapped_layers()
            .filter(|layer| self.is_lock_screen(layer.surface))
            .min_by_key(|layer| self.seat.keyboard_focus != Some(layer.surface))
            .map(|layer| layer.surface)
    }

    /// Locks the session after this long without input, or never. The idle
    /// timer counts from now, or from the unlock while the session is locked.
    pub fn lock_after_idle(&mut self, after: Option<Duration>) {
        self.lock.idle_after = after;
        self.lock.last_input = Instant::now();
        self.arm_idle_timer();
    }

    pub(crate) fn power_timeouts(&self) -> (u32, u32) {
        let lock_secs = self
            .lock
            .idle_after
            .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32);
        let suspend_secs = self
            .lock
            .suspend_after
            .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32);
        (lock_secs, suspend_secs)
    }

    pub(crate) fn configure_power(&mut self, lock_after_secs: u32, suspend_after_secs: u32) {
        self.lock.idle_after = (lock_after_secs > 0).then(|| Duration::from_secs(u64::from(lock_after_secs)));
        self.lock.suspend_after = (suspend_after_secs > 0).then(|| Duration::from_secs(u64::from(suspend_after_secs)));
        self.lock.last_input = Instant::now();
        self.arm_idle_timer();
    }

    /// The person did something: the idle timer starts again.
    pub(crate) fn input_seen(&mut self) {
        self.lock.last_input = Instant::now();
    }

    /// Sets the idle timer in place of the one there was, or none while the
    /// session is locked: there is nothing left for it to do until unlock.
    fn arm_idle_timer(&mut self) {
        if let Some(timer) = self.lock.timer.take() {
            self.event_loop.cancel_timer(timer);
        }
        if self.lock.locked {
            return;
        }
        if let Some(after) = self.lock.idle_after {
            let deadline = self.lock.last_input + after;
            self.lock.timer = Some(self.event_loop.set_timer(deadline, TimerKey::IdleLock));
        }
    }

    /// The idle timer ran out: the session locks if nothing happened since,
    /// unless a shown surface inhibits idle. The timer is set again while
    /// the session is not locked.
    pub(crate) fn idle_timer(&mut self) {
        let Some(after) = self.lock.idle_after else {
            return;
        };
        if self.lock.last_input.elapsed() >= after {
            if self.idle_inhibited() {
                self.lock.last_input = Instant::now();
            } else {
                self.lock("idle");
            }
        }
        self.arm_idle_timer();
    }
}
