//! Outputs: each display the compositor shows the desktop on, and its
//! `wl_output` global.
//!
//! Until Milestone 2 brings displays, an output is added by its caller, as a
//! test does. Outputs sit side by side, left to right, at scale 1.

use fulcrum_wire::protocol::wayland::wl_output;

use crate::{State, client::ClientId, globals::GlobalKind};

/// The `wl_output` version Fulcrum offers: name and description included.
pub const OUTPUT_VERSION: u32 = 4;

#[derive(Clone, Debug)]
pub struct Output {
    pub number: u64,
    pub name: String,
    /// In pixels.
    pub size: (i32, i32),
    /// In millihertz.
    pub refresh: i32,
    /// Its top-left corner on the desktop.
    pub position: (i32, i32),
    pub scale: i32,
    pub global: u32,
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
            .map(|output| output.position.0 + output.size.0)
            .max()
            .unwrap_or(0);
        let global = self.add_global(&wl_output::INFO, OUTPUT_VERSION, GlobalKind::Output(number));
        self.outputs.push(Output {
            number,
            name: name.to_string(),
            size,
            refresh,
            position: (x, 0),
            scale: 1,
            global,
        });
        number
    }

    /// Removes an output and its global.
    pub fn remove_output(&mut self, number: u64) {
        let Some(index) = self.outputs.iter().position(|output| output.number == number) else {
            return;
        };
        let output = self.outputs.remove(index);
        self.remove_global(output.global);
    }

    /// A client bound an output: tells it what the output is.
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
