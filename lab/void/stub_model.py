#!/usr/bin/env python3
"""OpenAI-compatible scripted stub Model for CrowbarOS lab checks.

Speaks `/v1/chat/completions` (streaming SSE and non-streaming JSON),
`/v1/models`, and `/health` using only the Python standard library. Installed
only in lab images (`--lab-account`), never in a person's image.

Usage:
    stub_model.py --script <path> [--host 127.0.0.1] [--port 0]
                  [--port-file <path>] [--log <path>]
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import signal
import sys
import threading
import time


def _extract_text(content):
    """Extracts plain text from an OpenAI message `content` field."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if isinstance(item, dict) and isinstance(item.get("text"), str):
                parts.append(item["text"])
        return "\n".join(parts)
    return ""


def _latest_turn(messages):
    """Returns `(system_text, prompt_text, tool_output)` for the latest turn.

    Only tool outputs that appear *after* the latest user message belong to
    this turn, so a tool call on an earlier turn does not mask a new turn.
    """
    system_parts = []
    last_user_idx = -1
    prompt_text = ""
    for idx, msg in enumerate(messages):
        if not isinstance(msg, dict):
            continue
        role = msg.get("role")
        if role == "system":
            system_parts.append(_extract_text(msg.get("content", "")))
        elif role == "user":
            text = _extract_text(msg.get("content", ""))
            if text:
                last_user_idx = idx
                prompt_text = text
    tool_output = None
    if last_user_idx >= 0:
        for msg in messages[last_user_idx + 1:]:
            if isinstance(msg, dict) and msg.get("role") == "tool":
                tool_output = _extract_text(msg.get("content", ""))
    return "\n".join(system_parts), prompt_text, tool_output


def _tool_outputs(messages):
    """Every tool output in the conversation so far, in order: what the
    harness told the Model of each act, a refused one included."""
    return [
        _extract_text(msg.get("content", ""))
        for msg in messages
        if isinstance(msg, dict) and msg.get("role") == "tool"
    ]


def _is_title_request(system_text, has_tools):
    """Detects OpenCode's background title-generation chat requests."""
    if has_tools:
        return False
    lowered = system_text.lower()
    return "title" in lowered or "summarize" in lowered


def _render(template, *, model, prompt, tool_output):
    return (
        template
        .replace("{model}", model)
        .replace("{prompt}", prompt)
        .replace("{tool_output}", (tool_output or "").strip())
    )


def decide_response(script, body):
    """Decides either a text reply or a tool call for a chat completion body.

    Returns a dict with either:
      {"kind": "text", "text": "..."}
    or:
      {"kind": "tool_call", "name": "...", "arguments": "{...}", "id": "call_stub_1"}
    """
    model = str(body.get("model") or "stub-1")
    messages = body.get("messages") or []
    tools = body.get("tools") or []
    has_tools = bool(tools)
    system_text, prompt, tool_output = _latest_turn(messages)

    if _is_title_request(system_text, has_tools):
        return {"kind": "text", "text": script.get("title_reply", "Stub thread")}

    matched = None
    for rule in script.get("rules", []):
        needle = rule.get("match")
        if needle is None or needle in prompt:
            matched = rule
            break

    if matched is not None:
        if matched.get("error"):
            err = matched["error"]
            return {
                "kind": "error",
                "status": int(err.get("status", 400)),
                "message": str(err.get("message", "Stub model error")),
                "type": str(err.get("type", "invalid_request_error")),
            }
        if matched.get("tool_call") and tool_output is None and has_tools:
            call = matched["tool_call"]
            args = call.get("arguments", {})
            args_json = args if isinstance(args, str) else json.dumps(args)
            return {
                "kind": "tool_call",
                "id": str(call.get("id") or "call_stub_1"),
                "name": str(call["name"]),
                "arguments": args_json,
                "reasoning": matched.get("reasoning"),
            }
        if tool_output is not None and "after_tool" in matched:
            return {
                "kind": "text",
                "text": _render(matched["after_tool"], model=model, prompt=prompt, tool_output=tool_output),
            }
        if "reply" in matched:
            return {
                "kind": "text",
                "text": _render(matched["reply"], model=model, prompt=prompt, tool_output=tool_output),
                "reasoning": matched.get("reasoning"),
            }

    if tool_output is not None:
        template = script.get("default_after_tool", "Tool output: {tool_output}")
    else:
        template = script.get("default_reply", "Stub ({model}): {prompt}")
    return {
        "kind": "text",
        "text": _render(template, model=model, prompt=prompt, tool_output=tool_output),
    }


def make_handler(script, log_path=None, forbidden=None):
    forbidden_terms = [term for term in (forbidden or []) if term]
    log_lock = threading.Lock()
    counter_lock = threading.Lock()
    counter = [0]

    def next_id():
        with counter_lock:
            counter[0] += 1
            return f"chatcmpl-stub-{counter[0]}"

    def append_log(entry):
        if not log_path:
            return
        with log_lock:
            with open(log_path, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry) + "\n")
                handle.flush()

    class StubHandler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):
            pass

        def _send_json(self, status, payload):
            raw = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            if path == "/health":
                self._send_json(200, {"ok": True})
                return
            if path in ("/v1/models", "/models"):
                models = script.get("models") or ["stub-1"]
                self._send_json(200, {
                    "object": "list",
                    "data": [
                        {"id": m, "object": "model", "created": 1, "owned_by": "crowbar-stub"}
                        for m in models
                    ],
                })
                return
            self._send_json(404, {"error": {"message": f"unknown path {path}"}})

        def do_POST(self):
            path = self.path.split("?", 1)[0]
            if path not in ("/v1/chat/completions", "/chat/completions"):
                self._send_json(404, {"error": {"message": f"unknown path {path}"}})
                return
            length = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(length)
            raw_text = raw.decode("utf-8", errors="replace")
            try:
                body = json.loads(raw_text)
            except ValueError as err:
                self._send_json(400, {"error": {"message": f"invalid json: {err}"}})
                return

            model = str(body.get("model") or "stub-1")
            stream = bool(body.get("stream", False))
            messages = body.get("messages") or []
            _, prompt, tool_output = _latest_turn(messages)
            decision = decide_response(script, body)
            forbidden_hit_count = sum(1 for term in forbidden_terms if term in raw_text)
            append_log({
                "path": path,
                "model": model,
                "stream": stream,
                "has_tools": bool(body.get("tools")),
                "prompt": prompt,
                "tool_output": tool_output,
                "history_tool_outputs": _tool_outputs(messages),
                "decision": decision["kind"],
                "forbidden_found": forbidden_hit_count > 0,
                "forbidden_hit_count": forbidden_hit_count,
            })

            cmpl_id = next_id()
            created = int(time.time())

            if decision["kind"] == "error":
                self._send_json(decision["status"], {
                    "error": {
                        "message": decision["message"],
                        "type": decision["type"],
                    }
                })
                return

            if stream:
                chunks = []
                reasoning = decision.get("reasoning")
                if reasoning:
                    chunks.append({
                        "id": cmpl_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {"role": "assistant", "reasoning_content": reasoning},
                            "finish_reason": None,
                        }],
                    })
                if decision["kind"] == "tool_call":
                    chunks.append({
                        "id": cmpl_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "role": "assistant",
                                "tool_calls": [{
                                    "index": 0,
                                    "id": decision["id"],
                                    "type": "function",
                                    "function": {
                                        "name": decision["name"],
                                        "arguments": decision["arguments"],
                                    },
                                }],
                            },
                            "finish_reason": None,
                        }],
                    })
                    finish = "tool_calls"
                else:
                    chunks.append({
                        "id": cmpl_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {"role": "assistant", "content": decision["text"]},
                            "finish_reason": None,
                        }],
                    })
                    finish = "stop"
                chunks.append({
                    "id": cmpl_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                })
                payload = "".join(f"data: {json.dumps(ch)}\n\n" for ch in chunks) + "data: [DONE]\n\n"
                encoded = payload.encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)
                self.wfile.flush()
                return

            if decision["kind"] == "tool_call":
                message = {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [{
                        "id": decision["id"],
                        "type": "function",
                        "function": {
                            "name": decision["name"],
                            "arguments": decision["arguments"],
                        },
                    }],
                }
                finish = "tool_calls"
            else:
                message = {"role": "assistant", "content": decision["text"]}
                finish = "stop"
            self._send_json(200, {
                "id": cmpl_id,
                "object": "chat.completion",
                "created": created,
                "model": model,
                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })

    return StubHandler


def start_server(script, host="127.0.0.1", port=0, log_path=None, forbidden=None):
    handler = make_handler(script, log_path=log_path, forbidden=forbidden)
    return http.server.ThreadingHTTPServer((host, port), handler)


def main(argv=None):
    parser = argparse.ArgumentParser(description="OpenAI-compatible scripted stub Model")
    parser.add_argument("--script", required=True, type=Path, help="JSON script path")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--port-file", type=Path, default=None)
    parser.add_argument("--log", type=Path, default=None)
    parser.add_argument("--forbid", action="append", default=[], help="Sentinel checked against full raw request body")
    args = parser.parse_args(argv)

    script = json.loads(args.script.read_text(encoding="utf-8"))
    server = start_server(script, host=args.host, port=args.port, log_path=args.log, forbidden=args.forbid)
    bound_port = server.server_address[1]

    if args.port_file is not None:
        args.port_file.parent.mkdir(parents=True, exist_ok=True)
        part = args.port_file.with_name(args.port_file.name + f".{os.getpid()}.tmp")
        part.write_text(f"{bound_port}\n", encoding="utf-8")
        part.replace(args.port_file)

    def stop_handler(_signum, _frame):
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop_handler)
    signal.signal(signal.SIGINT, stop_handler)

    try:
        server.serve_forever()
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
