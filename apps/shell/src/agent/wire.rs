// SPDX-License-Identifier: MIT
//! Lever surface-socket wire framing and JSON message decoding.

use anyhow::{Context, Result, bail};
use std::{
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    time::Instant,
};

use super::types::{
    Act, ActState, CommandInfo, DesktopContext, DirEntry, DirListing, EffortInfo, LeverMessage,
    ModeInfo, ModelInfo, Note, Offer, Queued, Request, RequestAnswer, RequestState, ResumedThread,
    StatusInfo, Steer, Subagent, SubagentMessage, SubagentState, ThreadSummary, Turn, TurnState,
    calm_turn_error,
};

/// Protocol version spoken on Lever's surface socket.
pub(super) const SURFACE_VERSION: u32 = 1;
/// Longest frame accepted from Lever (64 MiB, matching `crowbar-agent`, so
/// `resume` replies carrying long threads are not truncated).
pub(super) const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Encodes one framed JSON message (4-byte big-endian length + UTF-8 JSON).
pub(super) fn encode_frame(message: &serde_json::Value) -> Vec<u8> {
    let bytes = message.to_string().into_bytes();
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&bytes);
    out
}

/// Reads one framed JSON message from `stream`.
pub(super) fn read_frame(stream: &mut UnixStream) -> Result<serde_json::Value> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        bail!("Lever frame too long ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("Invalid JSON frame from Lever")
}

pub(super) enum DeadlineIoError {
    TimedOut,
    Other(anyhow::Error),
}

pub(super) enum FrameReadError {
    TooLong(usize),
    TimedOut,
    Other(anyhow::Error),
}

impl From<DeadlineIoError> for anyhow::Error {
    fn from(err: DeadlineIoError) -> Self {
        match err {
            DeadlineIoError::TimedOut => anyhow::anyhow!("Timed out waiting for Lever"),
            DeadlineIoError::Other(e) => e,
        }
    }
}

fn read_exact_before(
    stream: &mut UnixStream,
    buf: &mut [u8],
    deadline: Instant,
) -> std::result::Result<(), DeadlineIoError> {
    let mut read = 0;
    while read < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DeadlineIoError::TimedOut);
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|e| DeadlineIoError::Other(e.into()))?;
        match stream.read(&mut buf[read..]) {
            Ok(0) => {
                return Err(DeadlineIoError::Other(anyhow::anyhow!(
                    "Lever closed the surface socket before finishing the frame"
                )));
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                return Err(DeadlineIoError::TimedOut);
            }
            Err(e) => return Err(DeadlineIoError::Other(e.into())),
        }
    }
    Ok(())
}

fn write_all_before(
    stream: &mut UnixStream,
    buf: &[u8],
    deadline: Instant,
) -> std::result::Result<(), DeadlineIoError> {
    let mut written = 0;
    while written < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DeadlineIoError::TimedOut);
        }
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|e| DeadlineIoError::Other(e.into()))?;
        match stream.write(&buf[written..]) {
            Ok(0) => {
                return Err(DeadlineIoError::Other(anyhow::anyhow!(
                    "Lever closed the surface socket while writing"
                )));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                return Err(DeadlineIoError::TimedOut);
            }
            Err(e) => return Err(DeadlineIoError::Other(e.into())),
        }
    }
    Ok(())
}

pub(super) fn read_frame_before(
    stream: &mut UnixStream,
    deadline: Instant,
) -> std::result::Result<serde_json::Value, FrameReadError> {
    let mut header = [0u8; 4];
    read_exact_before(stream, &mut header, deadline).map_err(|e| match e {
        DeadlineIoError::TimedOut => FrameReadError::TimedOut,
        DeadlineIoError::Other(err) => FrameReadError::Other(err),
    })?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(FrameReadError::TooLong(len));
    }
    let mut body = vec![0u8; len];
    read_exact_before(stream, &mut body, deadline).map_err(|e| match e {
        DeadlineIoError::TimedOut => FrameReadError::TimedOut,
        DeadlineIoError::Other(err) => FrameReadError::Other(err),
    })?;
    serde_json::from_slice(&body)
        .context("Invalid JSON frame from Lever")
        .map_err(FrameReadError::Other)
}

pub(super) fn write_frame_before(
    stream: &mut UnixStream,
    message: &serde_json::Value,
    deadline: Instant,
) -> std::result::Result<(), DeadlineIoError> {
    write_all_before(stream, &encode_frame(message), deadline)
}

/// Translates one message from Lever's surface socket into a [`LeverMessage`].
pub fn parse_lever_message(msg: &serde_json::Value) -> Option<LeverMessage> {
    if let Some(reply) = msg.get("reply").and_then(|v| v.as_str()) {
        return match reply {
            "status" => {
                let lever_version = msg.get("version")?.as_str()?.to_string();
                let harness_started = msg
                    .get("harness_started")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let harnesses = msg
                    .get("harnesses")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                item.as_str()
                                    .or_else(|| item.get("name").and_then(|n| n.as_str()))
                                    .map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let free_hosted_models = msg
                    .get("free_hosted_models")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let models_chosen = msg
                    .get("models_chosen")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(LeverMessage::Status(StatusInfo {
                    lever_version,
                    harness_started,
                    harnesses,
                    free_hosted_models,
                    models_chosen,
                    default_mode: opt_str(msg, "default_mode"),
                }))
            }
            "models" => Some(LeverMessage::Models(parse_offer(msg))),
            "free-models" => Some(LeverMessage::FreeModels {
                enabled: msg.get("enabled")?.as_bool()?,
                models_chosen: msg.get("models_chosen").and_then(|v| v.as_bool()),
            }),
            "efforts" => Some(LeverMessage::Efforts {
                model: msg.get("model")?.as_str()?.to_string(),
                efforts: msg
                    .get("efforts")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                let id = item.get("id")?.as_str()?.to_string();
                                let name =
                                    opt_str(item, "name").filter(|n| !n.trim().is_empty());
                                Some(EffortInfo {
                                    name: name.unwrap_or_else(|| id.clone()),
                                    id,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "commands" => Some(LeverMessage::Commands {
                thread: opt_str(msg, "thread"),
                commands: parse_commands(msg),
                known: msg.get("known").and_then(|v| v.as_bool()).unwrap_or(true),
            }),
            "model-set" => Some(LeverMessage::ModelSet {
                thread: msg.get("thread")?.as_str()?.to_string(),
                model: msg.get("model")?.as_str()?.to_string(),
            }),
            "effort-set" => Some(LeverMessage::EffortSet {
                thread: msg.get("thread")?.as_str()?.to_string(),
                effort: opt_str(msg, "effort"),
            }),
            "mode-set" => Some(LeverMessage::ModeSet {
                thread: msg.get("thread")?.as_str()?.to_string(),
                mode: msg.get("mode")?.as_str()?.to_string(),
            }),
            "default-mode" => Some(LeverMessage::DefaultMode {
                mode: msg.get("mode")?.as_str()?.to_string(),
            }),
            "activity" => parse_activity(msg),
            "list" => {
                let threads = msg
                    .get("threads")?
                    .as_array()?
                    .iter()
                    .filter_map(parse_thread_summary)
                    .collect();
                Some(LeverMessage::List { threads })
            }
            "resumed" => {
                let thread = msg.get("thread")?.as_str()?.to_string();
                let title = msg
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let cwd = msg
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let model = msg
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let effort = opt_str(msg, "effort");
                let mode = opt_str(msg, "mode");
                let working = msg
                    .get("working")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let turns = msg
                    .get("turns")?
                    .as_array()?
                    .iter()
                    .filter_map(parse_turn)
                    .collect();
                Some(LeverMessage::Resumed(ResumedThread {
                    thread,
                    title,
                    cwd,
                    model,
                    effort,
                    mode,
                    working,
                    turns,
                }))
            }
            "sent" => Some(LeverMessage::Sent {
                thread: msg.get("thread")?.as_str()?.to_string(),
                turn: msg.get("turn")?.as_u64()?,
            }),
            "steered" => Some(LeverMessage::SteerTaken {
                thread: msg.get("thread")?.as_str()?.to_string(),
                turn: msg.get("turn")?.as_u64()?,
            }),
            "queued" => Some(LeverMessage::QueueTaken {
                thread: msg.get("thread")?.as_str()?.to_string(),
                turn: msg.get("turn")?.as_u64()?,
            }),
            "archived" | "deleted" => Some(LeverMessage::Lifecycle {
                thread: msg.get("thread")?.as_str()?.into(),
                deleted: reply == "deleted",
                warning: msg
                    .get("warning")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            }),
            "project-made" => Some(LeverMessage::ProjectMade {
                thread: msg.get("thread")?.as_str()?.to_string(),
                cwd: msg.get("cwd")?.as_str()?.to_string(),
                created: msg
                    .get("created")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }),
            "dirs" => {
                let path = msg.get("path")?.as_str()?.to_string();
                let home = msg.get("home")?.as_str()?.to_string();
                let parent = msg
                    .get("parent")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let entries = msg
                    .get("entries")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                Some(DirEntry {
                                    name: e.get("name")?.as_str()?.to_string(),
                                    path: e.get("path")?.as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(LeverMessage::Dirs(DirListing {
                    path,
                    home,
                    parent,
                    entries,
                }))
            }
            "ok" => Some(LeverMessage::Ok),
            "error" => Some(LeverMessage::Error {
                code: msg
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string(),
                message: msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("The Agent could not carry out that request.")
                    .to_string(),
            }),
            _ => None,
        };
    }

    let event = msg.get("event")?.as_str()?;
    if event == "activity" {
        return parse_activity(msg);
    }
    if event == "models" {
        return Some(LeverMessage::Models(parse_offer(msg)));
    }
    if event == "free-models" {
        return Some(LeverMessage::FreeModels {
            enabled: msg.get("enabled")?.as_bool()?,
            models_chosen: msg.get("models_chosen").and_then(|v| v.as_bool()),
        });
    }
    if event == "threads" {
        return Some(LeverMessage::Threads {
            threads: serde_json::from_value(msg.get("threads")?.clone()).ok()?,
            finished: msg
                .get("finished")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }
    if event == "deleted" || (event == "archived" && msg["archived"] == true) {
        return Some(LeverMessage::Lifecycle {
            thread: msg.get("thread")?.as_str()?.into(),
            deleted: event == "deleted",
            warning: None,
        });
    }
    if event == "commands" {
        return Some(LeverMessage::Commands {
            thread: opt_str(msg, "thread"),
            commands: parse_commands(msg),
            known: true,
        });
    }
    let thread = msg.get("thread")?.as_str()?.to_string();
    if event == "thread-updated" {
        return Some(LeverMessage::ThreadUpdated {
            thread,
            title: opt_str(msg, "title"),
            cwd: opt_str(msg, "cwd"),
            mode: opt_str(msg, "mode"),
            model: opt_str(msg, "model"),
            effort: msg.get("effort").map(|v| v.as_str().map(str::to_string)),
        });
    }
    let turn = msg.get("turn")?.as_u64()?;
    match event {
        "message" => {
            let text = msg.get("text")?.as_str()?.to_string();
            let context = msg
                .get("context")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value::<DesktopContext>(v.clone()).ok());
            Some(LeverMessage::Message {
                thread,
                turn,
                text,
                context,
                from_harness: msg.get("origin").and_then(|v| v.as_str()) == Some("harness"),
            })
        }
        "text" => Some(LeverMessage::Text {
            thread,
            turn,
            text: msg.get("text")?.as_str()?.to_string(),
        }),
        "steered" => Some(LeverMessage::Steered {
            thread,
            turn,
            steer: parse_steer(msg, 0)?,
        }),
        "queued" => Some(LeverMessage::Queued {
            thread,
            turn,
            queued: parse_queued(msg)?,
        }),
        "act" => Some(LeverMessage::Act {
            thread,
            turn,
            act: parse_act(msg)?,
        }),
        "note" => Some(LeverMessage::Note {
            thread,
            turn,
            note: parse_note(msg)?,
        }),
        "request" => Some(LeverMessage::Request {
            thread,
            turn,
            request: parse_request(msg)?,
        }),
        "subagent" => Some(LeverMessage::Subagent {
            thread,
            turn,
            subagent: parse_subagent(msg)?,
        }),
        "subagent-text" => Some(LeverMessage::SubagentText {
            thread,
            turn,
            subagent: msg.get("subagent")?.as_str()?.to_string(),
            role: msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("answer")
                .to_string(),
            text: msg.get("text")?.as_str()?.to_string(),
        }),
        "subagent-act" => Some(LeverMessage::SubagentAct {
            thread,
            turn,
            subagent: msg.get("subagent")?.as_str()?.to_string(),
            act: parse_act(msg)?,
        }),
        "done" => Some(LeverMessage::Done { thread, turn }),
        "stopped" => Some(LeverMessage::Stopped { thread, turn }),
        "failed" => Some(LeverMessage::Failed {
            thread,
            turn,
            message: msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("The Agent failed to answer.")
                .to_string(),
        }),
        "interrupted" => Some(LeverMessage::Interrupted { thread, turn }),
        _ => None,
    }
}

fn parse_queued(value: &serde_json::Value) -> Option<Queued> {
    Some(Queued {
        id: value.get("queued")?.as_str()?.to_string(),
        text: value.get("text")?.as_str()?.to_string(),
        state: value.get("state")?.as_str()?.to_string(),
    })
}

fn parse_steer(value: &serde_json::Value, at: usize) -> Option<Steer> {
    Some(Steer {
        text: value.get("text")?.as_str()?.to_string(),
        at: value
            .get("at")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(at),
        context: value
            .get("context")
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value::<DesktopContext>(v.clone()).ok()),
    })
}

fn parse_offer(msg: &serde_json::Value) -> Offer {
    Offer {
        supported: msg
            .get("supported")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        models: msg
            .get("models")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(ModelInfo {
                            id: item.get("id")?.as_str()?.to_string(),
                            name: opt_str(item, "name").unwrap_or_default(),
                            provider: opt_str(item, "provider"),
                            provider_name: opt_str(item, "provider_name"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        default_model: opt_str(msg, "default"),
        free_hosted_models: msg
            .get("free_hosted_models")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        steering: msg
            .get("steering")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        modes: msg
            .get("modes")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item.get("id")?.as_str()?.to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(ModeInfo { id, name })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn parse_thread_summary(value: &serde_json::Value) -> Option<ThreadSummary> {
    Some(ThreadSummary {
        archived: value
            .get("archived")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        thread: value.get("thread")?.as_str()?.to_string(),
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        cwd: value
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        model: value
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        started: value.get("started").and_then(|v| v.as_i64()).unwrap_or(0),
        updated: value.get("updated").and_then(|v| v.as_i64()).unwrap_or(0),
        turns: value.get("turns").and_then(|v| v.as_u64()).unwrap_or(0),
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("idle")
            .to_string(),
        working: value
            .get("working")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        waiting: value
            .get("waiting")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        subagents: value.get("subagents").and_then(|v| v.as_u64()).unwrap_or(0),
        mode: opt_str(value, "mode"),
    })
}

fn parse_commands(msg: &serde_json::Value) -> Vec<CommandInfo> {
    msg.get("commands")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(CommandInfo {
                        name: item.get("name")?.as_str()?.to_string(),
                        description: opt_str(item, "description").unwrap_or_default(),
                        hint: opt_str(item, "hint"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn opt_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn str_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_activity(msg: &serde_json::Value) -> Option<LeverMessage> {
    Some(LeverMessage::Activity {
        working: str_list(msg, "working"),
        waiting: str_list(msg, "waiting"),
    })
}

fn parse_request(value: &serde_json::Value) -> Option<Request> {
    let id = value.get("request")?.as_str()?.to_string();
    let answers = value
        .get("answers")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(RequestAnswer {
                        answer: item.get("answer")?.as_str()?.to_string(),
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("answered") => RequestState::Answered,
        Some("withdrawn") => RequestState::Withdrawn,
        _ => RequestState::Waiting,
    };
    Some(Request {
        id,
        act: opt_str(value, "act"),
        subagent: opt_str(value, "subagent"),
        kind: value
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("other")
            .to_string(),
        title: opt_str(value, "title").unwrap_or_default(),
        detail: opt_str(value, "detail").unwrap_or_default(),
        path: opt_str(value, "path"),
        answers,
        state,
        answer: opt_str(value, "answer"),
        sending: None,
    })
}

fn parse_act(value: &serde_json::Value) -> Option<Act> {
    let id = value.get("act")?.as_str()?.to_string();
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("other")
        .to_string();
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Act")
        .to_string();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("working") => ActState::Working,
        Some("done") => ActState::Done,
        Some("failed") => ActState::Failed,
        _ => ActState::Pending,
    };
    let detail = value
        .get("detail")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(Act {
        id,
        kind,
        title,
        state,
        detail,
    })
}

fn parse_note(value: &serde_json::Value) -> Option<Note> {
    Some(Note {
        id: value.get("note")?.as_str()?.to_string(),
        text: value.get("text")?.as_str()?.to_string(),
    })
}

fn parse_subagent(value: &serde_json::Value) -> Option<Subagent> {
    let id = value.get("subagent")?.as_str()?.to_string();
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Subagent")
        .to_string();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("working") => SubagentState::Working,
        Some("stopped") => SubagentState::Stopped,
        Some("interrupted") => SubagentState::Interrupted,
        Some("failed") => SubagentState::Failed(calm_turn_error(
            value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("The subagent failed."),
        )),
        _ => SubagentState::Done,
    };
    let messages = value
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(SubagentMessage {
                        role: item.get("role")?.as_str()?.to_string(),
                        text: item.get("text")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let acts = value
        .get("acts")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_act).collect())
        .unwrap_or_default();
    Some(Subagent {
        id,
        title,
        state,
        started: value.get("started").and_then(|v| v.as_i64()),
        ended: value.get("ended").and_then(|v| v.as_i64()),
        messages,
        acts,
    })
}

pub(super) fn parse_turn(value: &serde_json::Value) -> Option<Turn> {
    let turn = value.get("turn")?.as_u64()?;
    let message = value.get("message")?.as_str()?.to_string();
    let answer = value
        .get("answer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let context = value
        .get("context")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<DesktopContext>(v.clone()).ok());
    let acts = value
        .get("acts")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_act).collect())
        .unwrap_or_default();
    let notes = value
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_note).collect())
        .unwrap_or_default();
    let requests = value
        .get("requests")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_request).collect())
        .unwrap_or_default();
    let subagents = value
        .get("subagents")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_subagent).collect())
        .unwrap_or_default();
    let queued = value
        .get("queued")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_queued).collect())
        .unwrap_or_default();
    let steered = value
        .get("steered")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| parse_steer(item, answer.len()))
                .collect()
        })
        .unwrap_or_default();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("working") => TurnState::Working,
        Some("stopped") => TurnState::Stopped,
        Some("interrupted") => TurnState::Interrupted,
        Some("failed") => TurnState::Failed(calm_turn_error(
            value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("The turn failed."),
        )),
        _ => TurnState::Done,
    };
    Some(Turn {
        turn,
        message,
        context,
        acts,
        notes,
        requests,
        subagents,
        mode: opt_str(value, "mode"),
        answer,
        state,
        started: value.get("started").and_then(|v| v.as_i64()),
        ended: value.get("ended").and_then(|v| v.as_i64()),
        from_harness: value.get("origin").and_then(|v| v.as_str()) == Some("harness"),
        steered,
        queued,
    })
}
