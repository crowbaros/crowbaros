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

#[derive(Debug)]
pub(crate) struct Lock {
    pub locked: bool,
    /// How long without input before the session locks itself.
    idle_after: Option<Duration>,
    last_input: Instant,
    timer: Option<crate::event_loop::TimerId>,
}

impl Default for Lock {
    fn default() -> Self {
        Lock {
            locked: false,
            idle_after: None,
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
        self.refresh_keyboard_focus();
        self.seat.pointer_focus = None;
        self.seat.cursor = None;
        self.refresh_pointer_focus();
        self.control_event("locked");
        self.queue_redraw_all();
    }

    /// Unlocks the desktop, as the unlock authority, or the shell where there
    /// is none, asks. The window that had the keyboard has it back.
    pub fn unlock(&mut self) {
        if !self.lock.locked {
            return;
        }
        log::info!("session unlocked");
        self.lock.locked = false;
        let before = self
            .seat
            .desktop_keyboard_focus
            .map(|focus| self.root_of(focus))
            .filter(|window| {
                self.windows
                    .iter()
                    .any(|w| w.surface == *window && w.workspace == self.workspace)
            });
        self.set_keyboard_focus(before);
        self.refresh_pointer_focus();
        self.control_event("unlocked");
        self.input_seen();
        self.queue_redraw_all();
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
    /// timer counts from now.
    pub fn lock_after_idle(&mut self, after: Option<Duration>) {
        self.lock.idle_after = after;
        self.lock.last_input = Instant::now();
        self.arm_idle_timer();
    }

    /// The person did something: the idle timer starts again.
    pub(crate) fn input_seen(&mut self) {
        self.lock.last_input = Instant::now();
    }

    fn arm_idle_timer(&mut self) {
        if let Some(timer) = self.lock.timer.take() {
            self.event_loop.cancel_timer(timer);
        }
        if let Some(after) = self.lock.idle_after {
            let deadline = self.lock.last_input + after;
            self.lock.timer = Some(self.event_loop.set_timer(deadline, TimerKey::IdleLock));
        }
    }

    /// The idle timer ran out: the session locks if nothing happened since,
    /// unless a shown surface inhibits idle. The timer is set again.
    pub(crate) fn idle_timer(&mut self) {
        self.lock.timer = None;
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
