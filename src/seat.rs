//! The seat: one keyboard and one pointer (ADR 0020).
//!
//! - **The keyboard goes where the person clicks.** A click on a window gives
//!   it the keyboard and raises it. A click on a layer surface that takes
//!   keys gives the keyboard to it. A click on anything else leaves the
//!   keyboard where it was. A new window takes the keyboard in two cases
//!   only: its parent has it, or it is the first window after the shell says
//!   the person launched an app (see `windows.rs`). Any other new window wants
//!   attention, which the shell shows. When the window with the keyboard
//!   goes, no other window is handed it.
//! - **The pointer goes to what is under it**, and stays with the surface a
//!   button was pressed on until every button is let go.
//! - **One keymap, compiled by libxkbcommon**, shared with every client
//!   through a sealed memfd. The compositor keeps the key and modifier state.

use std::borrow::Cow;

use fulcrum_sys::xkb::{Keyboard, KeymapNames, Modifiers};
use fulcrum_wire::{
    Fixed, ObjectId,
    protocol::wayland::{wl_keyboard, wl_pointer, wl_seat},
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    log,
    surface::{Role, SurfaceId},
};

/// The `wl_seat` version Fulcrum offers.
pub const SEAT_VERSION: u32 = 7;
/// Key repeat: keys a second, and the wait before the first repeat in
/// milliseconds.
pub const REPEAT: (i32, i32) = (25, 200);

/// The keyboard and pointer, and who has them.
/// The Super keys, as evdev codes: left and right.
const SUPER: [u32; 2] = [125, 126];
const SHIFT: [u32; 2] = [42, 54];
/// Super, Shift, Ctrl, and Alt, each side: held as part of a chord, never
/// its key.
const MODIFIERS: [u32; 8] = [125, 126, 42, 54, 29, 97, 56, 100];
/// The keys 1 and 9 of the top row.
const KEY_1: u32 = 2;
const KEY_9: u32 = 10;
const KEY_F: u32 = 33;
const KEY_L: u32 = 38;
const KEY_UP: u32 = 103;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_DOWN: u32 = 108;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

pub(crate) struct Seat {
    keyboard: Option<Keyboard>,
    pub keyboard_focus: Option<SurfaceId>,
    /// The surface under the pointer, and where its corner is on the
    /// desktop.
    pub pointer_focus: Option<(SurfaceId, (f64, f64))>,
    /// Keys held, as evdev codes.
    keys: Vec<u32>,
    /// Buttons held. While any is, the pointer stays with its surface.
    buttons: Vec<u32>,
    /// Keys the compositor took: pressed with Super held. Their releases are
    /// taken too, and no client ever hears of them.
    withheld: Vec<u32>,
    /// Super is held, and nothing else was pressed since it went down.
    super_alone: bool,
    /// The image a client set, if it has the pointer and set one.
    pub(crate) cursor: Option<crate::cursor::Cursor>,
}

impl Seat {
    pub fn new() -> Self {
        let keyboard = match Keyboard::new(&KeymapNames::default()) {
            Ok(keyboard) => Some(keyboard),
            Err(error) => {
                log::error!("no keymap: {error}; no client will get a keyboard");
                None
            }
        };
        Seat {
            keyboard,
            keyboard_focus: None,
            pointer_focus: None,
            keys: Vec::new(),
            withheld: Vec::new(),
            super_alone: false,
            cursor: None,
            buttons: Vec::new(),
        }
    }

    fn modifiers(&self) -> Modifiers {
        self.keyboard.as_ref().map(Keyboard::modifiers).unwrap_or_default()
    }

    fn keys_array(&self) -> Vec<u8> {
        self.keys.iter().flat_map(|key| key.to_ne_bytes()).collect()
    }
}

impl State {
    /// A client bound `wl_seat`: tells it what the seat has.
    pub(crate) fn seat_bound(&mut self, client_id: ClientId, object: u32) {
        let has_keyboard = self.seat.keyboard.is_some();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        let version = client.objects.get(&object).map_or(1, |object| object.version);
        let mut capabilities = wl_seat::capability::POINTER;
        if has_keyboard {
            capabilities |= wl_seat::capability::KEYBOARD;
        }
        client.send(object, wl_seat::Event::Capabilities { capabilities });
        if version >= 2 {
            client.send(object, wl_seat::Event::Name { name: "seat0".into() });
        }
    }

    pub(crate) fn seat_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        request: wl_seat::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            wl_seat::Request::GetKeyboard { id } => {
                let Some((file, size)) = self.seat.keyboard.as_ref().and_then(|keyboard| keyboard.keymap().ok()) else {
                    return Err(ProtocolError {
                        object,
                        code: wl_seat::error::MISSING_CAPABILITY,
                        message: "the seat has no keyboard".into(),
                    });
                };
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wl_keyboard::NAME),
                    Some(&wl_keyboard::INFO),
                    version,
                    ObjectKind::Keyboard,
                )?;
                client.send(
                    id.0,
                    wl_keyboard::Event::Keymap {
                        format: wl_keyboard::keymap_format::XKB_V1,
                        fd: file,
                        size,
                    },
                );
                if version >= 4 {
                    client.send(
                        id.0,
                        wl_keyboard::Event::RepeatInfo {
                            rate: REPEAT.0,
                            delay: REPEAT.1,
                        },
                    );
                }
                // A keyboard made while its client has the keyboard hears so.
                if let Some(focus) = self.seat.keyboard_focus
                    && self
                        .surfaces
                        .get(&focus)
                        .is_some_and(|surface| surface.client == client_id)
                {
                    let serial = self.next_serial();
                    self.send_keyboard_enter(client_id, id.0, focus, serial);
                }
                Ok(())
            }
            wl_seat::Request::GetPointer { id } => {
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wl_pointer::NAME),
                    Some(&wl_pointer::INFO),
                    version,
                    ObjectKind::Pointer,
                )
            }
            wl_seat::Request::GetTouch { .. } => Err(ProtocolError {
                object,
                code: wl_seat::error::MISSING_CAPABILITY,
                message: "the seat has no touch device".into(),
            }),
            wl_seat::Request::Release => Ok(()),
        }
    }

    pub(crate) fn pointer_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wl_pointer::Request,
    ) -> Result<(), ProtocolError> {
        let wl_pointer::Request::SetCursor {
            surface,
            hotspot_x,
            hotspot_y,
            ..
        } = request
        else {
            return Ok(());
        };
        // Only the client whose surface the pointer is on sets its image.
        let focused = self
            .seat
            .pointer_focus
            .and_then(|(focus, _)| self.surfaces.get(&focus))
            .is_some_and(|focus| focus.client == client_id);
        if !focused {
            return Ok(());
        }
        let cursor = match surface {
            None => crate::cursor::Cursor::Hidden,
            Some(surface) => {
                let id = self
                    .surface_of(client_id, surface)
                    .expect("the dispatch checked the argument's interface");
                let role = self.surfaces[&id].role;
                if role != Role::None && role != Role::Cursor {
                    return Err(ProtocolError {
                        object,
                        code: wl_pointer::error::ROLE,
                        message: format!("wl_surface#{} already has a role", surface.0),
                    });
                }
                if let Some(state) = self.surfaces.get_mut(&id) {
                    state.role = Role::Cursor;
                }
                crate::cursor::Cursor::Surface {
                    surface: id,
                    hotspot: (hotspot_x, hotspot_y),
                }
            }
        };
        self.seat.cursor = Some(cursor);
        self.queue_redraw_all();
        Ok(())
    }

    /// The objects of one kind a client holds.
    fn objects_of(&self, client_id: ClientId, keyboard: bool) -> Vec<u32> {
        self.clients.get(&client_id).map_or(Vec::new(), |client| {
            client
                .objects
                .iter()
                .filter(|(_, object)| {
                    if keyboard {
                        matches!(object.kind, ObjectKind::Keyboard)
                    } else {
                        matches!(object.kind, ObjectKind::Pointer)
                    }
                })
                .map(|(id, _)| *id)
                .collect()
        })
    }

    fn send_keyboard_enter(&mut self, client_id: ClientId, keyboard: u32, surface: SurfaceId, serial: u32) {
        let Some(object) = self.surfaces.get(&surface).map(|surface| surface.object) else {
            return;
        };
        let (keys, modifiers) = (self.seat.keys_array(), self.seat.modifiers());
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(
                keyboard,
                wl_keyboard::Event::Enter {
                    serial,
                    surface: ObjectId(object),
                    keys,
                },
            );
            client.send(
                keyboard,
                wl_keyboard::Event::Modifiers {
                    serial,
                    mods_depressed: modifiers.depressed,
                    mods_latched: modifiers.latched,
                    mods_locked: modifiers.locked,
                    group: modifiers.group,
                },
            );
        }
    }

    /// The surface that has the keyboard.
    pub(crate) fn keyboard_focus_surface(&self) -> Option<SurfaceId> {
        self.seat.keyboard_focus
    }

    /// Gives the keyboard to a surface, or to none. The client that loses it
    /// hears `leave`, and the one that gets it `enter` with the keys held.
    /// A toplevel's activated state follows.
    pub(crate) fn set_keyboard_focus(&mut self, surface: Option<SurfaceId>) {
        // While the session is locked, only the lock screen has it.
        if self.locked() && surface.is_some_and(|surface| !self.is_lock_screen(surface)) {
            return;
        }
        let old = self.seat.keyboard_focus;
        if old == surface {
            return;
        }
        self.seat.keyboard_focus = surface;
        // A window with the keyboard wants no attention.
        if let Some(root) = surface.map(|focus| self.root_of(focus))
            && let Some(window) = self.windows.iter_mut().find(|window| window.surface == root)
        {
            window.attention = false;
        }
        if let Some(old) = old
            && let Some((client_id, object)) = self.surfaces.get(&old).map(|s| (s.client, s.object))
        {
            let serial = self.next_serial();
            for keyboard in self.objects_of(client_id, true) {
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.send(
                        keyboard,
                        wl_keyboard::Event::Leave {
                            serial,
                            surface: ObjectId(object),
                        },
                    );
                }
            }
        }
        if let Some(new) = surface
            && let Some(client_id) = self.surfaces.get(&new).map(|s| s.client)
        {
            // A client is offered its group's selection just before it
            // gains the keyboard, as the data device's protocol says.
            self.offer_selection(client_id);
            self.offer_primary_selection(client_id);
            let serial = self.next_serial();
            for keyboard in self.objects_of(client_id, true) {
                self.send_keyboard_enter(client_id, keyboard, new, serial);
            }
        }
        for changed in [old, surface].into_iter().flatten() {
            if self
                .xdg
                .get(&changed)
                .is_some_and(|xdg| xdg.mapped && matches!(xdg.role, crate::xdg::XdgRole::Toplevel { .. }))
            {
                self.configure(changed);
            }
        }
    }

    /// A surface that is going away gives up the keyboard and the pointer.
    /// Nothing else takes the keyboard: the next click decides.
    pub(crate) fn drop_focus_from(&mut self, surface: SurfaceId) {
        if self.seat.keyboard_focus == Some(surface) {
            self.set_keyboard_focus(None);
        }
        if self.seat.pointer_focus.is_some_and(|(focus, _)| focus == surface) {
            self.seat.pointer_focus = None;
        }
    }

    /// Every key from every backend enters here: the evdev code, whether it
    /// went down, and the time in milliseconds.
    ///
    /// Every chord with Super belongs to the compositor and is never sent to
    /// a client (ADR 0020): a key pressed while Super is held is taken here,
    /// with its release. Super is the physical key, whatever the keymap
    /// says.
    pub fn handle_key(&mut self, key: u32, pressed: bool, time: u32) {
        self.input_seen();
        // Super tapped alone, with no other key or button between its press
        // and its release, asks the shell for its launcher.
        if SUPER.contains(&key) {
            if pressed {
                self.seat.super_alone = self.seat.keys.is_empty() && self.seat.buttons.is_empty();
            } else if std::mem::take(&mut self.seat.super_alone) && !self.locked() {
                self.control_event("launcher");
            }
        } else if pressed {
            self.seat.super_alone = false;
        }
        if !pressed && self.seat.withheld.contains(&key) {
            self.seat.withheld.retain(|held| *held != key);
            return;
        }
        let super_held = self.seat.keys.iter().any(|held| SUPER.contains(held));
        if pressed && super_held && !MODIFIERS.contains(&key) {
            if !self.seat.withheld.contains(&key) {
                self.seat.withheld.push(key);
            }
            // While locked, the chord is taken and does nothing.
            if !self.locked() {
                let shift = self.seat.keys.iter().any(|held| SHIFT.contains(held));
                self.compositor_key(key, shift);
            }
            return;
        }
        if pressed {
            if !self.seat.keys.contains(&key) {
                self.seat.keys.push(key);
            }
        } else {
            self.seat.keys.retain(|held| *held != key);
        }
        let changed = self
            .seat
            .keyboard
            .as_mut()
            .is_some_and(|keyboard| keyboard.update(key + 8, pressed));
        let Some(focus) = self.seat.keyboard_focus else {
            return;
        };
        let Some(client_id) = self.surfaces.get(&focus).map(|surface| surface.client) else {
            return;
        };
        let serial = self.next_serial();
        let modifiers = self.seat.modifiers();
        let state = if pressed {
            wl_keyboard::key_state::PRESSED
        } else {
            wl_keyboard::key_state::RELEASED
        };
        for keyboard in self.objects_of(client_id, true) {
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.send(
                    keyboard,
                    wl_keyboard::Event::Key {
                        serial,
                        time,
                        key,
                        state,
                    },
                );
                if changed {
                    client.send(
                        keyboard,
                        wl_keyboard::Event::Modifiers {
                            serial,
                            mods_depressed: modifiers.depressed,
                            mods_latched: modifiers.latched,
                            mods_locked: modifiers.locked,
                            group: modifiers.group,
                        },
                    );
                }
            }
        }
    }

    /// A chord with Super: one of the compositor's keys.
    ///
    /// | Chord | What it does |
    /// | --- | --- |
    /// | Super+1 to Super+9 | Shows that workspace |
    /// | Super+Shift+1 to Super+Shift+9 | Moves the window with the keyboard there |
    /// | Super+Up | Maximizes the window with the keyboard, or restores it |
    /// | Super+F | Makes it fullscreen, or restores it |
    /// | Super+Left, Super+Right | Snaps it to that half, or restores it |
    /// | Super+Down | Restores it |
    ///
    /// Super tapped alone, pressed and let go with nothing else between,
    /// asks the shell for its launcher (see `handle_key`). Every other chord
    /// with Super is taken and does nothing yet.
    fn compositor_key(&mut self, key: u32, shift: bool) {
        use crate::modes::Mode;
        match key {
            KEY_L => return self.lock("key"),
            KEY_UP => return self.toggle_mode(Mode::Maximized),
            KEY_F => return self.toggle_mode(Mode::Fullscreen),
            KEY_LEFT => return self.toggle_mode(Mode::Left),
            KEY_RIGHT => return self.toggle_mode(Mode::Right),
            KEY_DOWN => return self.toggle_mode(Mode::Floating),
            _ => {}
        }
        if !(KEY_1..=KEY_9).contains(&key) {
            return;
        }
        let workspace = key - KEY_1 + 1;
        if !shift {
            self.show_workspace(workspace);
            return;
        }
        if let Some(focus) = self.seat.keyboard_focus {
            let root = self.root_of(focus);
            let window = self
                .toplevel_of(root)
                .filter(|toplevel| self.windows.iter().any(|window| window.surface == *toplevel));
            if let Some(window) = window {
                self.move_to_workspace(window, workspace);
            }
        }
    }

    /// The button held while the pointer is on a window's tree, as when its
    /// client asks to move or resize it.
    pub(crate) fn button_held_on(&self, window: SurfaceId) -> Option<u32> {
        let button = *self.seat.buttons.first()?;
        let (focus, _) = self.seat.pointer_focus?;
        let root = self.root_of(focus);
        (root == window || self.toplevel_of(root) == Some(window)).then_some(button)
    }

    /// The pointer leaves the surface it is on, as a grab starts.
    pub(crate) fn end_pointer_focus(&mut self) {
        if let Some((surface, _)) = self.seat.pointer_focus.take() {
            self.send_pointer(surface, |serial, surface| wl_pointer::Event::Leave { serial, surface });
        }
        self.seat.buttons.clear();
    }

    /// Looks again at what is under the pointer, as after what is shown
    /// changed under it.
    pub(crate) fn refresh_pointer_focus(&mut self) {
        let time = self.start_time.elapsed().as_millis() as u32;
        self.update_pointer_focus(time);
    }

    /// The pointer is now at this place on the desktop: it is kept on the
    /// outputs, its focus follows what is under it, and every output is
    /// looked at again for a backend that draws the pointer.
    pub fn pointer_moved(&mut self, x: f64, y: f64) {
        self.pointer_moved_at(x, y, self.start_time.elapsed().as_millis() as u32);
    }

    pub fn pointer_moved_at(&mut self, x: f64, y: f64, time: u32) {
        self.input_seen();
        let (x, y) = self.clamp_to_outputs(x, y);
        self.pointer = (x, y);
        if self.grab.is_some() {
            // The pointer drives the window, and no client hears it.
            self.grab_motion();
        } else {
            self.update_pointer_focus(time);
        }
        self.queue_redraw_all();
    }

    /// Moves the pointer by a distance, as a mouse does.
    pub fn pointer_moved_by(&mut self, dx: f64, dy: f64, time: u32) {
        self.pointer_moved_at(self.pointer.0 + dx, self.pointer.1 + dy, time);
    }

    fn clamp_to_outputs(&self, x: f64, y: f64) -> (f64, f64) {
        let Some(bounds) = self
            .outputs
            .iter()
            .map(|output| output.rect())
            .reduce(|a, b| a.union(&b))
        else {
            return (x, y);
        };
        (
            x.clamp(f64::from(bounds.x), f64::from(bounds.x + bounds.width - 1)),
            y.clamp(f64::from(bounds.y), f64::from(bounds.y + bounds.height - 1)),
        )
    }

    fn update_pointer_focus(&mut self, time: u32) {
        let (x, y) = self.pointer;
        let under = if self.seat.buttons.is_empty() {
            self.surface_under(x, y)
        } else {
            // A button is held: the pointer stays with its surface.
            self.seat.pointer_focus
        };
        let old = self.seat.pointer_focus;
        if old.map(|(surface, _)| surface) != under.map(|(surface, _)| surface) {
            // A client's image is its own: the next client sets its own.
            self.reset_cursor();
            if let Some((old_surface, _)) = old {
                self.send_pointer(old_surface, |serial, surface| wl_pointer::Event::Leave {
                    serial,
                    surface,
                });
            }
            self.seat.pointer_focus = under;
            if let Some((surface, origin)) = under {
                let (sx, sy) = (Fixed::from_f64(x - origin.0), Fixed::from_f64(y - origin.1));
                self.send_pointer(surface, |serial, surface| wl_pointer::Event::Enter {
                    serial,
                    surface,
                    surface_x: sx,
                    surface_y: sy,
                });
            }
        } else if let Some((surface, origin)) = under {
            self.seat.pointer_focus = under;
            let (sx, sy) = (Fixed::from_f64(x - origin.0), Fixed::from_f64(y - origin.1));
            self.send_pointer(surface, |_, _| wl_pointer::Event::Motion {
                time,
                surface_x: sx,
                surface_y: sy,
            });
        }
    }

    /// Sends one event, then a frame, to each pointer of a surface's client.
    fn send_pointer(&mut self, surface: SurfaceId, event: impl Fn(u32, ObjectId) -> wl_pointer::Event) {
        let Some((client_id, object)) = self.surfaces.get(&surface).map(|s| (s.client, s.object)) else {
            return;
        };
        let serial = self.next_serial();
        for pointer in self.objects_of(client_id, false) {
            if let Some(client) = self.clients.get_mut(&client_id) {
                let version = client.objects.get(&pointer).map_or(1, |object| object.version);
                client.send(pointer, event(serial, ObjectId(object)));
                if version >= 5 {
                    client.send(pointer, wl_pointer::Event::Frame);
                }
            }
        }
    }

    /// A pointer button went down or up: `button` is the evdev code. A press
    /// with no other button held gives the keyboard to what is under the
    /// pointer.
    pub fn pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        self.input_seen();
        if pressed {
            self.seat.super_alone = false;
        }
        if self.grab.is_some() {
            self.seat.buttons.retain(|held| *held != button);
            if !pressed {
                self.grab_button_released(button);
            }
            return;
        }
        // Super with the left button moves the window under the pointer,
        // and with the right button resizes it. Neither press reaches it.
        if pressed && self.seat.buttons.is_empty() && self.seat.keys.iter().any(|held| SUPER.contains(held)) {
            let window = self
                .surface_under(self.pointer.0, self.pointer.1)
                .map(|(surface, _)| self.root_of(surface))
                .and_then(|root| self.toplevel_of(root))
                .filter(|toplevel| self.windows.iter().any(|window| window.surface == *toplevel));
            if let Some(window) = window {
                self.raise(window);
                match button {
                    BTN_LEFT => return self.start_move(window, button),
                    BTN_RIGHT => {
                        let edges = self.nearest_edges(window);
                        return self.start_resize(window, button, edges);
                    }
                    _ => {}
                }
            }
        }
        if pressed {
            if self.seat.buttons.is_empty() {
                self.focus_under_pointer();
            }
            if !self.seat.buttons.contains(&button) {
                self.seat.buttons.push(button);
            }
        } else {
            self.seat.buttons.retain(|held| *held != button);
        }
        if let Some((surface, _)) = self.seat.pointer_focus {
            let state = if pressed {
                wl_pointer::button_state::PRESSED
            } else {
                wl_pointer::button_state::RELEASED
            };
            self.send_pointer(surface, |serial, _| wl_pointer::Event::Button {
                serial,
                time,
                button,
                state,
            });
        }
        if !pressed && self.seat.buttons.is_empty() {
            self.update_pointer_focus(time);
        }
    }

    /// A scroll: distances along each axis, in the units of
    /// `wl_pointer.axis`.
    pub fn pointer_axis(&mut self, horizontal: f64, vertical: f64, time: u32) {
        self.input_seen();
        let Some((surface, _)) = self.seat.pointer_focus else {
            return;
        };
        for (axis, value) in [
            (wl_pointer::axis::HORIZONTAL_SCROLL, horizontal),
            (wl_pointer::axis::VERTICAL_SCROLL, vertical),
        ] {
            if value != 0.0 {
                self.send_pointer(surface, |_, _| wl_pointer::Event::Axis {
                    time,
                    axis,
                    value: Fixed::from_f64(value),
                });
            }
        }
    }

    /// A click gives the keyboard to what is under the pointer: a window,
    /// which is raised, or a layer surface that takes keys. A click on
    /// anything else leaves the keyboard where it is.
    fn focus_under_pointer(&mut self) {
        let (x, y) = self.pointer;
        let Some((surface, _)) = self.surface_under(x, y) else {
            return;
        };
        let root = self.root_of(surface);
        if self.windows.iter().any(|window| window.surface == root) {
            self.raise(root);
            self.set_keyboard_focus(Some(root));
        } else if self.layer_of(root).is_some_and(|layer| layer.takes_keys()) {
            self.set_keyboard_focus(Some(root));
        }
    }

    /// Puts a window on top of the others.
    pub(crate) fn raise(&mut self, surface: SurfaceId) {
        let Some(index) = self.windows.iter().position(|window| window.surface == surface) else {
            return;
        };
        if index + 1 == self.windows.len() {
            return;
        }
        let window = self.windows.remove(index);
        self.windows.push(window);
        for output in self.outputs_showing(surface) {
            self.queue_redraw(output);
        }
    }

    /// The surface under a point of the desktop, and where its corner is:
    /// the shell's overlay and top layers, then popups and windows from the
    /// top down, then its bottom and background layers. A surface's input
    /// region decides which of its points count.
    pub(crate) fn surface_under(&self, x: f64, y: f64) -> Option<(SurfaceId, (f64, f64))> {
        use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_shell_v1::layer;
        let point = (x.floor() as i32, y.floor() as i32);
        // While the session is locked, only the lock screen is under the
        // pointer.
        if self.locked() {
            let hit = self.layers.iter().rev().find_map(|surface| {
                (surface.mapped && self.is_lock_screen(surface.surface))
                    .then(|| self.tree_hit(surface.surface, (surface.rect.x, surface.rect.y), point, 0))
                    .flatten()
            })?;
            return Some((hit.0, (f64::from(hit.1.0), f64::from(hit.1.1))));
        }
        let layers = |kinds: [u32; 2]| {
            kinds.into_iter().find_map(|kind| {
                self.layers
                    .iter()
                    .rev()
                    .filter(|surface| surface.mapped && surface.layer() == kind)
                    .find_map(|surface| self.tree_hit(surface.surface, (surface.rect.x, surface.rect.y), point, 0))
            })
        };
        // As drawn: a fullscreen window, and every window above it, are
        // above the top layers, and each window's popups are with it.
        let showing: Vec<&crate::windows::Window> = self
            .windows
            .iter()
            .filter(|window| window.workspace == self.workspace)
            .collect();
        let split = showing
            .iter()
            .rposition(|window| window.mode == crate::modes::Mode::Fullscreen)
            .unwrap_or(showing.len());
        let (below, above) = showing.split_at(split);
        let popup_hit = |of: &dyn Fn(Option<SurfaceId>) -> bool| {
            self.popups.iter().rev().find_map(|popup| {
                if !self.xdg.get(popup).is_some_and(|xdg| xdg.mapped) || !of(self.toplevel_of(*popup)) {
                    return None;
                }
                let corner = self.xdg_location(*popup, 0)?;
                let geometry = self.window_geometry(*popup);
                self.tree_hit(*popup, (corner.0 - geometry.x, corner.1 - geometry.y), point, 0)
            })
        };
        let windows_hit = |windows: &[&crate::windows::Window]| {
            windows.iter().rev().find_map(|window| {
                let geometry = self.window_geometry(window.surface);
                let origin = (window.location.0 - geometry.x, window.location.1 - geometry.y);
                self.tree_hit(window.surface, origin, point, 0)
            })
        };
        let among = |windows: &[&crate::windows::Window], toplevel: Option<SurfaceId>| {
            windows.iter().any(|window| Some(window.surface) == toplevel)
        };
        let hit = popup_hit(&|toplevel| toplevel.is_none())
            .or_else(|| layers([layer::OVERLAY, layer::OVERLAY]))
            .or_else(|| popup_hit(&|toplevel| among(above, toplevel)))
            .or_else(|| windows_hit(above))
            .or_else(|| layers([layer::TOP, layer::TOP]))
            .or_else(|| popup_hit(&|toplevel| among(below, toplevel)))
            .or_else(|| windows_hit(below))
            .or_else(|| layers([layer::BOTTOM, layer::BACKGROUND]))?;
        Some((hit.0, (f64::from(hit.1.0), f64::from(hit.1.1))))
    }

    /// The surface of a tree at a point, topmost first, with its corner.
    fn tree_hit(
        &self,
        id: SurfaceId,
        origin: (i32, i32),
        point: (i32, i32),
        depth: usize,
    ) -> Option<(SurfaceId, (i32, i32))> {
        let surface = self.surfaces.get(&id)?;
        surface.current.content.as_ref()?;
        for member in surface.stack.iter().rev() {
            if *member == id {
                let (width, height) = surface.current.size();
                let local = (point.0 - origin.0, point.1 - origin.1);
                let inside = local.0 >= 0 && local.1 >= 0 && local.0 < width && local.1 < height;
                let accepts = surface
                    .current
                    .input
                    .as_ref()
                    .is_none_or(|region| region.contains(local.0, local.1));
                if inside && accepts {
                    return Some((id, origin));
                }
            } else if depth < crate::surface::MAX_SUBSURFACE_DEPTH
                && let Some(child) = self.surfaces.get(member)
            {
                let at = (origin.0 + child.position.0, origin.1 + child.position.1);
                if let Some(hit) = self.tree_hit(*member, at, point, depth + 1) {
                    return Some(hit);
                }
            }
        }
        None
    }
}
