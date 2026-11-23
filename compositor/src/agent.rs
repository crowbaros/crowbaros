//! The Agent's surface and the summon gesture (ADR 0035).
//!
//! The Agent's surface is the shell's drawer: its layer surface whose
//! namespace is [`AGENT_NAMESPACE`]. The shell draws it and holds its state:
//! whether it is up. The compositor owns the gesture that summons it,
//! Super+Space, which no client hears, and tells the shell of each one. The
//! shell then shows the surface, puts it away, or moves the keyboard, by
//! asking (see `control.rs`).

use crate::{State, json::quote, log, surface::SurfaceId};

/// The namespace of the shell's layer surface that is the Agent's surface.
pub const AGENT_NAMESPACE: &str = "crowbaros-agent";

/// What the compositor keeps for the Agent's surface. None of it is the
/// surface's own state, which is the shell's.
#[derive(Debug, Default)]
pub(crate) struct Agent {
    /// The output the pointer was on at the last summon gesture: where the
    /// surface the shell then makes goes.
    summoned_on: Option<u64>,
    /// The shell asked for the keyboard to go to the surface before the
    /// surface was shown: it takes the keyboard as it shows.
    wants_keyboard: bool,
    /// What had the keyboard when the surface took it, for the shell to
    /// give it back to. Forgotten once the keyboard goes anywhere else.
    keyboard_before: Option<SurfaceId>,
}

impl State {
    /// The summon gesture: the shell is told, with the display the pointer
    /// is on, that display's size on the desktop, and whether the Agent's
    /// surface has the keyboard. The shell decides what the gesture does.
    pub(crate) fn summon(&mut self) {
        let Some(output) = self
            .output_for_new_window()
            .and_then(|number| self.outputs.iter().find(|output| output.number == number))
        else {
            return;
        };
        let (number, (width, height)) = (output.number, output.logical_size());
        let keyboard = self.agent_has_keyboard();
        log::info!("summon output={} keyboard={keyboard}", output.name);
        let fields = format!(
            "\"output\":{},\"width\":{width},\"height\":{height},\"keyboard\":{keyboard}",
            quote(&output.name)
        );
        self.agent.summoned_on = Some(number);
        self.control_event_with("summon", &fields);
    }

    /// The output a new Agent's surface goes on, where the shell names
    /// none: the one the pointer was on at the last summon gesture, or the
    /// one it is on now if that display has gone.
    pub(crate) fn output_for_agent_surface(&self) -> Option<u64> {
        self.agent
            .summoned_on
            .filter(|number| self.outputs.iter().any(|output| output.number == *number))
            .or_else(|| self.output_for_new_window())
    }

    /// The shell's `focus_agent`: the Agent's surface takes the keyboard, as
    /// a click on it would give it. The shell asks as it makes the surface,
    /// so one not shown yet takes the keyboard when it shows.
    pub(crate) fn focus_agent(&mut self) -> Result<(), String> {
        if self.locked() {
            return Err("the desktop is locked".into());
        }
        match self.agent_surface() {
            Some(surface) => self.set_keyboard_focus(Some(surface)),
            None => self.agent.wants_keyboard = true,
        }
        Ok(())
    }

    /// The shell's `unfocus_agent`: the keyboard leaves the Agent's surface
    /// for the window that had it before, if that window is still shown,
    /// and for nothing otherwise. The shell asks as it puts the surface
    /// away, so a surface that has just gone counts as holding the keyboard.
    /// A keyboard that is in a window stays there.
    pub(crate) fn unfocus_agent(&mut self) -> Result<(), String> {
        if self.locked() {
            return Err("the desktop is locked".into());
        }
        self.agent.wants_keyboard = false;
        let before = self.agent.keyboard_before.take();
        let focus = self.seat.desktop_keyboard_focus;
        if focus.is_some() && focus != self.agent_surface() {
            return Ok(());
        }
        let window = before.filter(|before| {
            self.windows
                .iter()
                .any(|window| window.surface == *before && window.workspace == self.workspace)
        });
        self.set_keyboard_focus(window);
        Ok(())
    }

    /// The desktop's focus is about to go to `next`: what has the keyboard
    /// as the Agent's surface takes it is remembered, and forgotten when
    /// the keyboard goes anywhere else.
    pub(crate) fn note_focus_for_agent(&mut self, next: Option<SurfaceId>) {
        let agent = self.agent_surface();
        let current = self.seat.desktop_keyboard_focus;
        if next.is_none() || next != agent {
            self.agent.keyboard_before = None;
        } else if current != agent {
            self.agent.keyboard_before = current;
        }
    }

    /// A layer surface was shown: the Agent's surface takes the keyboard if
    /// the shell asked for that while making it.
    pub(crate) fn layer_shown(&mut self, surface: SurfaceId) {
        if self.agent_surface() != Some(surface) {
            return;
        }
        if let Some(layer) = self.layer_of(surface) {
            let output = self.outputs.iter().find(|output| output.number == layer.output);
            let rect = layer.rect;
            log::info!(
                "agent surface shown output={} at={},{} size={}x{}",
                output.map_or("", |output| output.name.as_str()),
                rect.x,
                rect.y,
                rect.width,
                rect.height
            );
        }
        if std::mem::take(&mut self.agent.wants_keyboard) {
            self.set_keyboard_focus(Some(surface));
        }
    }

    /// The Agent's surface, while it is shown.
    pub(crate) fn agent_surface(&self) -> Option<SurfaceId> {
        self.mapped_layers()
            .filter(|layer| layer.namespace == AGENT_NAMESPACE)
            .last()
            .map(|layer| layer.surface)
    }

    /// Whether the keyboard is in the Agent's surface.
    fn agent_has_keyboard(&self) -> bool {
        let focus = self.seat.keyboard_focus.map(|focus| self.root_of(focus));
        focus.is_some() && focus == self.agent_surface()
    }
}
