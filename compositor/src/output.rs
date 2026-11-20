//! Outputs: each display the compositor shows the desktop on, and its
//! `wl_output` global.
//!
//! A backend adds an output for each display it lights, and a test adds its
//! own. Outputs sit side by side, left to right.
//!
//! Each has a scale from 1 to 3 in steps of a quarter (ADR 0020), kept in
//! 120ths as fractional scale counts. The desktop is in logical units: an
//! output covers its pixels over its scale, and the scene is drawn at its
//! pixels (see `scene.rs`). A client hears the scale rounded up as
//! `wl_output.scale`, and the scale itself as its surface's preferred
//! fractional scale.

use fulcrum_wire::protocol::wayland::wl_output;

use crate::{State, client::ClientId, globals::GlobalKind};

/// The `wl_output` version Fulcrum offers: name and description included.
pub const OUTPUT_VERSION: u32 = 4;

#[derive(Clone, Debug)]
pub struct Output {
    pub number: u64,
    pub name: String,
    /// What the layout is kept by: the monitor's identity, or its name
    /// where the backend knows no other (see `layout.rs`).
    pub identity: String,
    /// The modes it can show: width, height, and refresh in millihertz.
    pub modes: Vec<(i32, i32, i32)>,
    /// In pixels: its mode.
    pub size: (i32, i32),
    /// In millihertz.
    pub refresh: i32,
    /// Its top-left corner on the desktop.
    pub position: (i32, i32),
    /// Its scale in 120ths: from 120 to 360, in steps of 30.
    pub scale_120: u32,
    /// The scale rounded up, as `wl_output.scale` says it.
    pub scale: i32,
    pub global: u32,
    pub(crate) redraw: crate::redraw::Redraw,
    /// How many times it was drawn, or looked at for drawing.
    pub(crate) draws: u64,
    /// Presentation feedback for the frame on its way to the display.
    pub(crate) presenting: Vec<(ClientId, u32)>,
}

/// The scale every output starts at, in 120ths.
pub const SCALE_ONE: u32 = 120;

impl Output {
    /// Its size on the desktop: its pixels over its scale.
    pub fn logical_size(&self) -> (i32, i32) {
        let scale = i64::from(self.scale_120.max(1));
        let over = |pixels: i32| ((i64::from(pixels) * 120 + scale / 2) / scale) as i32;
        (over(self.size.0), over(self.size.1))
    }

    /// The rectangle it covers on the desktop.
    pub fn rect(&self) -> crate::geometry::Rect {
        let (width, height) = self.logical_size();
        crate::geometry::Rect::new(self.position.0, self.position.1, width, height)
    }
}

impl State {
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// Adds an output to the right of the others, and its global. `refresh`
    /// is in millihertz.
    pub fn add_output(&mut self, name: &str, size: (i32, i32), refresh: i32) -> u64 {
        self.next_output += 1;
        let number = self.next_output;
        let x = self
            .outputs
            .iter()
            .map(|output| output.rect())
            .map(|rect| rect.x + rect.width)
            .max()
            .unwrap_or(0);
        let global = self.add_global(&wl_output::INFO, OUTPUT_VERSION, GlobalKind::Output(number));
        self.outputs.push(Output {
            number,
            name: name.to_string(),
            identity: name.to_string(),
            modes: vec![(size.0, size.1, refresh)],
            size,
            refresh,
            position: (x, 0),
            scale_120: SCALE_ONE,
            scale: 1,
            global,
            redraw: crate::redraw::Redraw::Idle,
            draws: 0,
            presenting: Vec::new(),
        });
        self.queue_redraw(number);
        number
    }

    /// Removes an output and its global.
    pub fn remove_output(&mut self, number: u64) {
        let Some(index) = self.outputs.iter().position(|output| output.number == number) else {
            return;
        };
        let output = self.outputs.remove(index);
        self.remove_global(output.global);
        // The frame it was showing never will be.
        let mut by_client: std::collections::HashMap<ClientId, Vec<u32>> = std::collections::HashMap::new();
        for (client, feedback) in &output.presenting {
            by_client.entry(*client).or_default().push(*feedback);
        }
        for (client, feedbacks) in by_client {
            self.discard_feedback(client, feedbacks);
        }
        self.usable.remove(&number);
        let closed: Vec<_> = self
            .layers
            .iter()
            .filter(|layer| layer.output == number)
            .map(|layer| layer.surface)
            .collect();
        for surface in closed {
            self.close_layer(surface);
        }
        // A window that is then on no output moves to the first one left.
        if let Some(first) = self.outputs.first().map(|output| output.number) {
            let area = self.usable_area(first);
            let bounds: Vec<_> = self.outputs.iter().map(Output::rect).collect();
            for window in &mut self.windows {
                if !bounds
                    .iter()
                    .any(|rect| rect.contains(window.location.0, window.location.1))
                {
                    window.location = (
                        area.x + crate::windows::NEW_WINDOW_OFFSET.0,
                        area.y + crate::windows::NEW_WINDOW_OFFSET.1,
                    );
                }
            }
        }
        let windows: Vec<_> = self.windows.iter().map(|window| window.surface).collect();
        for surface in windows {
            self.update_outputs_of(surface);
        }
        self.update_fractional_scales();
        self.queue_redraw_all();
    }

    /// Changes an output's size, as when its mode changes or the window the
    /// nested backend draws into is resized. The outputs to its right move
    /// along, each client is told, and the layer surfaces are placed again.
    pub fn resize_output(&mut self, number: u64, size: (i32, i32)) {
        self.reshape_output(number, |output| output.size = size);
    }

    /// An output now shows another mode: width, height, and refresh in
    /// millihertz.
    pub fn set_output_mode(&mut self, number: u64, mode: (i32, i32, i32)) {
        self.reshape_output(number, |output| {
            output.size = (mode.0, mode.1);
            output.refresh = mode.2;
        });
    }

    /// Names the monitor an output shows on, the modes it can show, and its
    /// physical size in millimetres, where (0, 0) is none reported. It gives
    /// the output the scale and place the layout keeps for the monitor, and
    /// where the layout keeps no scale, one picked from its density (see
    /// `default_scale.rs`). That one is not kept: only what the person sets
    /// is.
    pub fn identify_output(
        &mut self,
        number: u64,
        identity: &str,
        modes: Vec<(i32, i32, i32)>,
        physical_mm: (u32, u32),
    ) {
        let Some(output) = self.outputs.iter_mut().find(|output| output.number == number) else {
            return;
        };
        output.identity = identity.to_owned();
        if !modes.is_empty() {
            output.modes = modes;
        }
        let name = output.name.clone();
        let default = crate::default_scale::default_scale_120(output.size, physical_mm);
        let placement = self.layout.get(identity).cloned().unwrap_or_default();
        let mut kept = false;
        if let Some(scale) = placement.scale_120 {
            match self.set_output_scale(number, scale) {
                Ok(()) => kept = true,
                Err(error) => crate::log::warning!("the layout's scale was not taken: {error}"),
            }
        }
        if !kept {
            // Always a scale `set_output_scale` takes.
            let _ = self.set_output_scale(number, default);
            crate::log::info!(
                "output scale from its density name={name} scale={default} physical={}x{}mm",
                physical_mm.0,
                physical_mm.1
            );
        }
        if let Some(position) = placement.position {
            self.set_output_position(number, position);
        }
    }

    /// The mode the layout keeps for a monitor, for a backend to light it
    /// in.
    pub fn saved_mode(&self, identity: &str) -> Option<(i32, i32, i32)> {
        self.layout.get(identity)?.mode
    }

    /// Keeps the layout of displays in a file, and reads what it holds.
    pub fn keep_layout_in(&mut self, path: &std::path::Path) {
        self.layout = crate::layout::Layout::load(path);
    }

    /// Puts an output's top-left corner at a place on the desktop. The
    /// others stay where they are.
    pub fn set_output_position(&mut self, number: u64, position: (i32, i32)) {
        let Some(output) = self.outputs.iter_mut().find(|output| output.number == number) else {
            return;
        };
        if output.position == position {
            return;
        }
        output.position = position;
        let bound: Vec<(ClientId, u32)> = self
            .clients
            .iter()
            .flat_map(|(id, client)| client.outputs_bound(number).into_iter().map(|object| (*id, object)))
            .collect();
        for (client, object) in bound {
            self.output_bound(client, object, number);
        }
        self.rearrange(number);
        let windows: Vec<_> = self.windows.iter().map(|window| window.surface).collect();
        for surface in windows {
            self.update_outputs_of(surface);
        }
        self.update_fractional_scales();
        self.queue_redraw_all();
    }

    /// Sets an output's scale, in 120ths: from 120 to 360 in steps of 30,
    /// as ADR 0020 allows. The desktop it covers changes as a resize's does.
    pub fn set_output_scale(&mut self, number: u64, scale_120: u32) -> Result<(), String> {
        if !(SCALE_ONE..=3 * SCALE_ONE).contains(&scale_120) || !scale_120.is_multiple_of(30) {
            return Err(format!(
                "a scale is from 1 to 3 in steps of 0.25, not {}",
                f64::from(scale_120) / 120.0
            ));
        }
        self.reshape_output(number, |output| {
            output.scale_120 = scale_120;
            output.scale = scale_120.div_ceil(SCALE_ONE) as i32;
        });
        Ok(())
    }

    /// An output's size or scale changes: the outputs to its right move
    /// along, each client is told, and what is on it is placed again.
    fn reshape_output(&mut self, number: u64, change: impl FnOnce(&mut Output)) {
        let Some(index) = self.outputs.iter().position(|output| output.number == number) else {
            return;
        };
        let shape = |output: &Output| (output.rect(), output.size, output.scale_120, output.refresh);
        let before = shape(&self.outputs[index]);
        let old = before.0;
        change(&mut self.outputs[index]);
        let new = self.outputs[index].rect();
        if shape(&self.outputs[index]) == before {
            return;
        }
        let mut changed = vec![number];
        let shift = new.width - old.width;
        for output in &mut self.outputs {
            if output.number != number && output.position.0 >= old.x + old.width {
                output.position.0 += shift;
                changed.push(output.number);
            }
        }
        for number in &changed {
            let bound: Vec<(ClientId, u32)> = self
                .clients
                .iter()
                .flat_map(|(id, client)| client.outputs_bound(*number).into_iter().map(|object| (*id, object)))
                .collect();
            for (client, object) in bound {
                self.output_bound(client, object, *number);
            }
        }
        let numbers: Vec<u64> = self.outputs.iter().map(|output| output.number).collect();
        for number in numbers {
            self.rearrange(number);
        }
        // A window the smaller output left on no output moves back onto it.
        let area = self.usable_area(number);
        let bounds: Vec<_> = self.outputs.iter().map(Output::rect).collect();
        for window in &mut self.windows {
            let (x, y) = window.location;
            if old.contains(x, y) && !bounds.iter().any(|bound| bound.contains(x, y)) {
                window.location = (
                    area.x + crate::windows::NEW_WINDOW_OFFSET.0,
                    area.y + crate::windows::NEW_WINDOW_OFFSET.1,
                );
            }
        }
        let windows: Vec<_> = self.windows.iter().map(|window| window.surface).collect();
        for surface in windows {
            self.update_outputs_of(surface);
        }
        self.update_fractional_scales();
        self.queue_redraw_all();
    }

    /// Tells a client's `wl_output` what the output is: when it is bound,
    /// and again when the output changes.
    pub(crate) fn output_bound(&mut self, client_id: ClientId, object: u32, number: u64) {
        let Some(output) = self.outputs.iter().find(|output| output.number == number).cloned() else {
            return;
        };
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        let version = client.objects.get(&object).map_or(1, |object| object.version);
        client.send(
            object,
            wl_output::Event::Geometry {
                x: output.position.0,
                y: output.position.1,
                physical_width: 0,
                physical_height: 0,
                subpixel: wl_output::subpixel::UNKNOWN as i32,
                make: "CrowbarOS".into(),
                model: output.name.clone(),
                transform: wl_output::transform::NORMAL as i32,
            },
        );
        client.send(
            object,
            wl_output::Event::Mode {
                flags: wl_output::mode::CURRENT | wl_output::mode::PREFERRED,
                width: output.size.0,
                height: output.size.1,
                refresh: output.refresh,
            },
        );
        if version >= 2 {
            client.send(object, wl_output::Event::Scale { factor: output.scale });
        }
        if version >= 4 {
            client.send(
                object,
                wl_output::Event::Name {
                    name: output.name.clone(),
                },
            );
            client.send(
                object,
                wl_output::Event::Description {
                    description: format!("CrowbarOS {}", output.name),
                },
            );
        }
        if version >= 2 {
            client.send(object, wl_output::Event::Done);
        }
    }
}
