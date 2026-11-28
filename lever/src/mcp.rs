//! Harness-agnostic Model Context Protocol (MCP) server specification, tool
//! definitions, and stdio JSON-RPC 2.0 bridge.
//!
//! Every ACP harness today (OpenCode and `crowbar-stub-harness`, and future
//! harness adapters) receives the same [`McpServerSpec`]:
//! `crowbar-lever mcp-bridge`, communicating over standard input and output.
//!
//! The bridge opens **no network port**. It forwards `tools/call` requests to
//! Lever's private `SO_PEERCRED`-checked Unix domain socket using credentials
//! passed exclusively via environment variables (never command-line arguments).

use crate::frames::{self, FrameReader, MAX_FRAME};
use crate::version::{LEVER_VERSION, PROTOCOL_VERSION};
use serde_json::{Value, json};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The MCP server name Lever registers with every harness.
pub const MCP_SERVER_NAME: &str = "crowbar";

/// The MCP tool that makes the calling thread a project.
pub const TOOL_MAKE_PROJECT: &str = "make_project";

/// Environment variable holding Lever's private tool Unix socket path.
pub const ENV_MCP_SOCKET: &str = "CROWBAR_MCP_SOCKET";
/// Environment variable holding the harness-lifetime authentication secret.
pub const ENV_MCP_AUTH: &str = "CROWBAR_MCP_AUTH";
/// Environment variable holding the per-session token identifying the calling
/// thread for harnesses that spawn one MCP server per session.
pub const ENV_MCP_SESSION: &str = "CROWBAR_MCP_SESSION";
/// Optional environment variable naming a harness-specific `_meta` key on MCP
/// `tools/call` params that carries the caller's native session ID (for
/// harnesses that share one MCP server across sessions).
pub const ENV_MCP_META_SESSION_KEY: &str = "CROWBAR_MCP_META_SESSION_KEY";

/// Supported MCP protocol versions, newest first. During `initialize`, if the
/// client proposes one of these, Lever echoes it; otherwise Lever responds with
/// its newest supported version (or `DEFAULT_MCP_PROTOCOL_VERSION` if omitted).
const SUPPORTED_MCP_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Read/write timeout for a single bridge call to Lever's local Unix socket.
const BRIDGE_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

/// Harness-agnostic specification for a stdio MCP server that any harness
/// adapter translates into its own configuration format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl McpServerSpec {
    /// Builds the `crowbar` stdio MCP server specification pointing back at
    /// Lever's private tool Unix socket. Credentials stay in `env`, never in
    /// `args`.
    pub fn crowbar(exe: PathBuf, socket_path: &Path, auth_token: &str, session_token: &str) -> Self {
        Self {
            name: MCP_SERVER_NAME.to_string(),
            command: exe,
            args: vec!["mcp-bridge".to_string()],
            env: vec![
                (ENV_MCP_SOCKET.to_string(), socket_path.display().to_string()),
                (ENV_MCP_AUTH.to_string(), auth_token.to_string()),
                (ENV_MCP_SESSION.to_string(), session_token.to_string()),
            ],
        }
    }
}

/// Returns the MCP `tools/list` tool definitions served by Lever.
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": TOOL_MAKE_PROJECT,
            "description": "Make the current thread a project by tying it to a folder. Creates the folder if it does not exist, or uses an existing folder as-is without changing anything in it. Defaults to ~/Projects/<name> unless 'place' is given (for example '~/Code' puts the project at '~/Code/<name>'). After this call, subsequent turns run in the project's folder automatically with the thread's prior conversation preserved; if you run a command in the same turn, use the returned path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Project folder name (a single folder name with no path separators)."
                    },
                    "place": {
                        "type": "string",
                        "description": "Optional parent folder where the project folder is placed (e.g. '~/Code' puts the project at '~/Code/<name>'). Defaults to '~/Projects'."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }
    ])
}

/// Extracts a caller harness-session hint from MCP `tools/call` params `_meta`,
/// checking an adapter-declared `extra_key` first and then generic session keys.
pub fn extract_meta_session(params: &Value, extra_key: Option<&str>) -> Option<String> {
    let meta = params.get("_meta")?.as_object()?;
    if let Some(key) = extra_key
        && let Some(val) = meta.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
    {
        return Some(val.to_string());
    }
    for key in ["sessionId", "session_id"] {
        if let Some(val) = meta.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return Some(val.to_string());
        }
    }
    None
}

/// Formats a human-readable detail line for a `make_project` tool call request
/// (shown in Ask first request cards without exposing raw JSON or internal IDs).
/// Uses the same path resolver as `make_project` execution so the destination
/// shown to the person matches the folder Lever will actually create or use.
pub fn format_make_project_detail(input: &Value, home: &Path) -> Option<String> {
    let obj = input.as_object()?;
    let name = obj.get("name").and_then(Value::as_str)?;
    let place = match obj.get("place") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return None,
    };
    let resolved = crate::project::resolve_project_path(name, place, home).ok()?;
    let home_norm = crate::project::expand_absolute_path("~", home, "Home").ok();
    if let Some(ref h) = home_norm {
        if resolved == *h {
            return Some("~".to_string());
        }
        if let Ok(rel) = resolved.strip_prefix(h) {
            return Some(format!("~/{}", rel.display()));
        }
    }
    Some(resolved.display().to_string())
}

/// Entry point for `crowbar-lever mcp-bridge`: speaks MCP JSON-RPC 2.0 over
/// standard input and output, forwarding `tools/call` to Lever's private tool
/// Unix socket.
pub fn run_stdio_bridge() -> Result<(), String> {
    let socket = std::env::var(ENV_MCP_SOCKET)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("mcp-bridge requires {ENV_MCP_SOCKET}"))?;
    let auth = std::env::var(ENV_MCP_AUTH)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("mcp-bridge requires {ENV_MCP_AUTH}"))?;
    let session = std::env::var(ENV_MCP_SESSION).unwrap_or_default();
    let meta_session_key = std::env::var(ENV_MCP_META_SESSION_KEY).ok().filter(|s| !s.is_empty());

    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_stdio_with_meta_key(
        stdin.lock(),
        stdout.lock(),
        meta_session_key.as_deref(),
        |tool, arguments, meta_session| {
            call_lever_tool(Path::new(&socket), &auth, &session, tool, arguments, meta_session)
        },
    )
}

/// Outcome returned by Lever for one `tools/call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallOutcome {
    pub ok: bool,
    pub text: String,
}

/// Runs the MCP JSON-RPC 2.0 line loop over arbitrary `reader`/`writer` streams
/// (allowing pure unit testing without sockets).
pub fn serve_stdio<R, W, F>(reader: R, writer: W, on_tool_call: F) -> Result<(), String>
where
    R: BufRead,
    W: Write,
    F: FnMut(&str, &Value, Option<&str>) -> ToolCallOutcome,
{
    serve_stdio_with_meta_key(reader, writer, None, on_tool_call)
}

/// Like [`serve_stdio`], also checking `extra_meta_session_key` inside `_meta`
/// on `tools/call` requests.
pub fn serve_stdio_with_meta_key<R, W, F>(
    reader: R,
    mut writer: W,
    extra_meta_session_key: Option<&str>,
    mut on_tool_call: F,
) -> Result<(), String>
where
    R: BufRead,
    W: Write,
    F: FnMut(&str, &Value, Option<&str>) -> ToolCallOutcome,
{
    for line in reader.lines() {
        let line = line.map_err(|e| format!("failed to read MCP stdin: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                write_json_line(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": "Parse error" }
                    }),
                )?;
                continue;
            }
        };
        let Some(obj) = msg.as_object() else {
            continue;
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            continue;
        };
        // Notifications carry no `id` and expect no reply.
        let Some(id) = id else {
            continue;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let response = match method {
            "initialize" => {
                let version = match params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    Some(req) if SUPPORTED_MCP_PROTOCOL_VERSIONS.contains(&req) => req,
                    Some(_) => SUPPORTED_MCP_PROTOCOL_VERSIONS[0],
                    None => DEFAULT_MCP_PROTOCOL_VERSION,
                };
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": MCP_SERVER_NAME,
                            "version": LEVER_VERSION
                        }
                    }
                })
            }
            "ping" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": tool_definitions()
                }
            }),
            "tools/call" => {
                let Some(tool_name) = params.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                    write_json_line(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "Missing tool name." }
                        }),
                    )?;
                    continue;
                };
                let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let meta_session = extract_meta_session(&params, extra_meta_session_key);
                let outcome = on_tool_call(tool_name, &arguments, meta_session.as_deref());
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": outcome.text }],
                        "isError": !outcome.ok
                    }
                })
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        };
        write_json_line(&mut writer, &response)?;
    }
    Ok(())
}

fn write_json_line<W: Write>(writer: &mut W, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| format!("failed to encode MCP reply: {e}"))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|e| format!("failed to write MCP stdout: {e}"))
}

fn call_lever_tool(
    socket_path: &Path,
    auth: &str,
    session: &str,
    tool: &str,
    arguments: &Value,
    meta_session: Option<&str>,
) -> ToolCallOutcome {
    match call_lever_tool_inner(socket_path, auth, session, tool, arguments, meta_session) {
        Ok(outcome) => outcome,
        Err(err) => ToolCallOutcome { ok: false, text: err },
    }
}

fn call_lever_tool_inner(
    socket_path: &Path,
    auth: &str,
    session: &str,
    tool: &str,
    arguments: &Value,
    meta_session: Option<&str>,
) -> Result<ToolCallOutcome, String> {
    let mut stream =
        UnixStream::connect(socket_path).map_err(|e| format!("Could not reach Lever's tool socket: {e}"))?;
    let _ = stream.set_read_timeout(Some(BRIDGE_SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(BRIDGE_SOCKET_TIMEOUT));

    let mut reader = FrameReader::new(MAX_FRAME);

    // Protocol v1 handshake.
    stream
        .write_all(&frames::encode(&json!({ "version": PROTOCOL_VERSION })))
        .map_err(|e| format!("Failed to send handshake to Lever: {e}"))?;
    let hello = read_one_frame(&mut stream, &mut reader)?;
    if hello.get("version").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Err("Lever rejected the tool bridge protocol version.".to_string());
    }

    let mut req = json!({
        "op": "mcp-call",
        "auth": auth,
        "session": session,
        "tool": tool,
        "arguments": arguments,
    });
    if let Some(meta) = meta_session {
        req["meta_session"] = Value::String(meta.to_string());
    }
    stream
        .write_all(&frames::encode(&req))
        .map_err(|e| format!("Failed to send tool call to Lever: {e}"))?;

    let reply = read_one_frame(&mut stream, &mut reader)?;
    match reply.get("reply").and_then(Value::as_str) {
        Some("mcp-result") => Ok(ToolCallOutcome {
            ok: reply.get("ok").and_then(Value::as_bool).unwrap_or(false),
            text: reply
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("Tool completed.")
                .to_string(),
        }),
        Some("error") => Ok(ToolCallOutcome {
            ok: false,
            text: reply
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Lever refused the tool call.")
                .to_string(),
        }),
        _ => Err("Lever returned an unexpected tool response.".to_string()),
    }
}

fn read_one_frame(stream: &mut UnixStream, reader: &mut FrameReader) -> Result<Value, String> {
    let mut buf = [0u8; 4096];
    loop {
        if let Some(frame) = reader
            .next_frame()
            .map_err(|_| "Lever sent an oversized tool frame.".to_string())?
        {
            return serde_json::from_slice(&frame).map_err(|e| format!("Lever sent invalid JSON on tool socket: {e}"));
        }
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("Failed reading from Lever tool socket: {e}"))?;
        if n == 0 {
            return Err("Lever closed the tool socket before answering.".to_string());
        }
        reader.push(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_protocol_handles_discover_initialize_list_and_call() {
        let input = [
            r#"{"jsonrpc":"2.0","id":"probe-1","method":"server/discover","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"make_project","arguments":{"name":"demo","place":"~/Code"},"_meta":{"vendor/sessionID":"ses_abc"}}}"#,
        ]
        .join("\n")
            + "\n";

        let mut out = Vec::new();
        let mut seen = Vec::new();
        serve_stdio_with_meta_key(
            input.as_bytes(),
            &mut out,
            Some("vendor/sessionID"),
            |tool, args, meta| {
                seen.push((tool.to_string(), args.clone(), meta.map(str::to_string)));
                ToolCallOutcome {
                    ok: true,
                    text: "Project ready at /home/person/Code/demo".to_string(),
                }
            },
        )
        .unwrap();

        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["id"], "probe-1");
        assert_eq!(lines[0]["error"]["code"], -32601);

        assert_eq!(lines[1]["id"], 1);
        assert_eq!(lines[1]["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(lines[1]["result"]["serverInfo"]["name"], "crowbar");

        assert_eq!(lines[2]["id"], 2);
        assert_eq!(lines[2]["result"]["tools"][0]["name"], "make_project");

        assert_eq!(lines[3]["id"], 3);
        assert_eq!(lines[3]["result"]["isError"], false);
        assert_eq!(
            lines[3]["result"]["content"][0]["text"],
            "Project ready at /home/person/Code/demo"
        );

        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "make_project");
        assert_eq!(seen[0].1["name"], "demo");
        assert_eq!(seen[0].1["place"], "~/Code");
        assert_eq!(seen[0].2.as_deref(), Some("ses_abc"));
    }

    #[test]
    fn negotiates_supported_protocol_version() {
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{}}"#,
        ]
        .join("\n")
            + "\n";
        let mut out = Vec::new();
        serve_stdio(input.as_bytes(), &mut out, |_, _, _| ToolCallOutcome {
            ok: true,
            text: String::new(),
        })
        .unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            lines[0]["result"]["protocolVersion"],
            SUPPORTED_MCP_PROTOCOL_VERSIONS[0]
        );
        assert_eq!(lines[1]["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(lines[2]["result"]["protocolVersion"], DEFAULT_MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn formats_make_project_request_detail() {
        let home = Path::new("/home/person");
        assert_eq!(
            format_make_project_detail(&json!({"name": "demo"}), home).as_deref(),
            Some("~/Projects/demo")
        );
        assert_eq!(
            format_make_project_detail(&json!({"name": "demo", "place": "~/Code/"}), home).as_deref(),
            Some("~/Code/demo")
        );
        assert_eq!(
            format_make_project_detail(&json!({"name": "demo", "place": "~//Code"}), home).as_deref(),
            Some("~/Code/demo")
        );
        assert_eq!(
            format_make_project_detail(&json!({"name": "demo", "place": "~/Code/../../../tmp"}), home).as_deref(),
            Some("/tmp/demo")
        );
        assert_eq!(format_make_project_detail(&json!({"name": "   "}), home), None);
    }
}
