//! The control channel between the compositor and the shell.
//!
//! This is the interface that replaces the apps' use of the Hyprland socket.
//! The compositor makes a socket pair and gives one end to the shell process
//! it starts. Nothing is bound in the filesystem, so no other process can
//! open it.
//!
//! The wire format is one JSON object per line. The shell sends a request and
//! gets one reply. The compositor may also send an event at any time. The set
//! of requests is fixed: there is no way to run a command or load code.
//!
//! ```text
//! -> {"op":"snapshot"}
//! <- {"reply":"snapshot","workspace":1,"active":"0x2","windows":[...]}
//! -> {"op":"focus_workspace","id":2}
//! <- {"reply":"ok"}
//! -> {"op":"focus_window","id":"0x1"}
//! <- {"reply":"ok"}
//! <- {"event":"confirm"}
//! ```

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};

use serde::{Deserialize, Serialize};
use smithay::{
    reexports::calloop::{Interest, Mode, PostAction, generic::Generic},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{State, windows::WORKSPACES};

/// The longest request line the compositor reads.
const MAX_LINE: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    // A struct variant, so that serde refuses unknown fields here too.
    Snapshot {},
    FocusWorkspace { id: u32 },
    FocusWindow { id: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Snapshot {
        workspace: u32,
        active: Option<String>,
        windows: Vec<WindowInfo>,
    },
    Ok,
    Error {
        message: String,
    },
}

/// Sent without being asked for.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The compositor took the confirm chord. No client received it.
    Confirm,
}

#[derive(Debug, Serialize)]
struct WindowInfo {
    id: String,
    app_id: String,
    title: String,
    workspace: u32,
}

fn window_name(id: u32) -> String {
    format!("{id:#x}")
}

impl State {
    /// Starts serving the compositor's end of the control channel.
    pub fn serve_control(&mut self, stream: UnixStream) -> std::io::Result<()> {
        stream.set_nonblocking(true)?;
        self.control = Some(stream.try_clone()?);
        let mut pending = Vec::new();
        self.loop_handle
            .insert_source(
                Generic::new(stream, Interest::READ, Mode::Level),
                move |_, stream, state| {
                    let mut chunk = [0u8; 1024];
                    // Safety: the stream is not dropped or replaced.
                    let read = unsafe { stream.get_mut() }.read(&mut chunk);
                    match read {
                        Ok(0) => {
                            state.control = None;
                            return Ok(PostAction::Remove);
                        }
                        Ok(n) => pending.extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            state.control = None;
                            return Ok(PostAction::Remove);
                        }
                    }
                    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=end).collect();
                        let reply = state.answer_control(&line[..end]);
                        state.send_control(&reply);
                    }
                    if pending.len() > MAX_LINE {
                        state.control = None;
                        return Ok(PostAction::Remove);
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }

    pub fn send_event(&mut self, event: Event) {
        self.send_control(&event);
    }

    fn send_control(&mut self, message: &impl Serialize) {
        let Some(stream) = self.control.as_mut() else {
            return;
        };
        let mut line = serde_json::to_vec(message).expect("control messages serialize");
        line.push(b'\n');
        if stream.write_all(&line).is_err() {
            self.control = None;
        }
    }

    fn answer_control(&mut self, line: &[u8]) -> Reply {
        let request = match serde_json::from_slice::<Request>(line) {
            Ok(request) => request,
            Err(error) => {
                return Reply::Error {
                    message: format!("not a request: {error}"),
                };
            }
        };
        match request {
            Request::Snapshot {} => self.snapshot(),
            Request::FocusWorkspace { id } => {
                if (1..=WORKSPACES).contains(&id) {
                    self.show_workspace(id);
                    Reply::Ok
                } else {
                    Reply::Error {
                        message: "no such workspace".into(),
                    }
                }
            }
            Request::FocusWindow { id } => {
                let found = self
                    .windows
                    .iter()
                    .find(|managed| window_name(managed.id) == id)
                    .map(|managed| (managed.window.clone(), managed.workspace));
                match found {
                    Some((window, workspace)) => {
                        self.show_workspace(workspace);
                        self.focus_window(Some(&window));
                        Reply::Ok
                    }
                    None => Reply::Error {
                        message: "no such window".into(),
                    },
                }
            }
        }
    }

    fn snapshot(&self) -> Reply {
        let windows = self
            .windows
            .iter()
            .map(|managed| {
                let (app_id, title) = managed
                    .window
                    .toplevel()
                    .map(|toplevel| {
                        with_states(toplevel.wl_surface(), |states| {
                            let data = states
                                .data_map
                                .get::<XdgToplevelSurfaceData>()
                                .unwrap()
                                .lock()
                                .unwrap();
                            (
                                data.app_id.clone().unwrap_or_default(),
                                data.title.clone().unwrap_or_default(),
                            )
                        })
                    })
                    .unwrap_or_default();
                WindowInfo {
                    id: window_name(managed.id),
                    app_id,
                    title,
                    workspace: managed.workspace,
                }
            })
            .collect();
        Reply::Snapshot {
            workspace: self.active_workspace,
            active: self.active_window.map(window_name),
            windows,
        }
    }
}
