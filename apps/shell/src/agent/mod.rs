// SPDX-License-Identifier: MIT
//! The Agent domain module: Lever surface-socket types, wire codec, transport
//! adapter, core `AgentState`, and the in-process MVU `AgentController`.

pub mod controller;
pub mod state;
pub mod transport;
pub mod types;
pub mod wire;

#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod tests;

pub use controller::{
    AgentController, AgentEffect, AgentIntent, FocusTarget, ThreadAction, display_folder_path,
    join_project_place, place_suggestions, validate_project_form,
};
pub use state::AgentState;
pub use transport::{CommandSender, load_picks, save_picks, start_from_env};
pub use types::{
    Act, ActTone, AgentEvent, AgentViewSnapshot, AnswerPart, ControlBounds, DEFAULT_MODES,
    DetailLine, Mark, Provider, Request, RequestView, Subagent, SubagentState, Turn, TurnState,
    act_display_status, format_act_detail, format_context_summary, format_elapsed,
    fullscreen_mark_overlay_output, mode_name, providers, request_detail_lines, request_view,
    unix_now,
};
