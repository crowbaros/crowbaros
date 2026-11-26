"""Unit tests for the Python OpenAI-compatible stub Model."""
import json
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.request

import stub_model


class StubModelTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.log = Path(self.tmp.name) / "requests.jsonl"
        self.script = {
            "models": ["stub-1", "stub-2"],
            "rules": [
                {
                    "match": "run-whoami",
                    "reasoning": "Let me check whoami.",
                    "tool_call": {
                        "name": "shell",
                        "arguments": {"command": "id -un && id -u", "description": "Check user"},
                    },
                    "after_tool": "Ran as ({model}): {tool_output}",
                },
                {
                    "match": "hello",
                    "reply": "Hi from {model}!",
                },
                {
                    "match": "make-fail",
                    "error": {"status": 400, "message": "Stub model declined this turn"},
                },
            ],
            "default_reply": "Default ({model}): {prompt}",
        }
        self.server = stub_model.start_server(self.script, log_path=self.log)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def _get(self, path):
        with urllib.request.urlopen(f"http://127.0.0.1:{self.port}{path}") as resp:
            return resp.status, json.loads(resp.read().decode("utf-8"))

    def _post(self, path, payload):
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            data=json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req) as resp:
            return resp.status, resp.headers.get("Content-Type", ""), resp.read().decode("utf-8")

    def test_health_and_models_endpoints(self):
        status, body = self._get("/health")
        self.assertEqual((status, body), (200, {"ok": True}))
        status, body = self._get("/v1/models")
        self.assertEqual(status, 200)
        self.assertEqual([m["id"] for m in body["data"]], ["stub-1", "stub-2"])

    def test_streaming_text_reply_and_request_log(self):
        status, ctype, raw = self._post("/v1/chat/completions", {
            "model": "stub-2",
            "stream": True,
            "messages": [{"role": "user", "content": "say hello"}],
            "tools": [{"type": "function", "function": {"name": "shell"}}],
        })
        self.assertEqual(status, 200)
        self.assertIn("text/event-stream", ctype)
        events = [line.removeprefix("data: ") for line in raw.splitlines() if line.startswith("data: ")]
        self.assertEqual(events[-1], "[DONE]")
        parsed = [json.loads(e) for e in events[:-1]]
        text = "".join(ch["choices"][0]["delta"].get("content", "") for ch in parsed)
        self.assertEqual(text, "Hi from stub-2!")
        self.assertEqual(parsed[-1]["choices"][0]["finish_reason"], "stop")

        logged = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(logged), 1)
        self.assertEqual(logged[0]["model"], "stub-2")
        self.assertEqual(logged[0]["prompt"], "say hello")

    def test_tool_call_and_follow_up_reply(self):
        status, _, raw = self._post("/v1/chat/completions", {
            "model": "stub-1",
            "stream": True,
            "messages": [{"role": "user", "content": "please run-whoami"}],
            "tools": [{"type": "function", "function": {"name": "shell"}}],
        })
        self.assertEqual(status, 200)
        events = [json.loads(line.removeprefix("data: "))
                  for line in raw.splitlines()
                  if line.startswith("data: ") and line != "data: [DONE]"]
        tool_deltas = [ch["choices"][0]["delta"]["tool_calls"][0]
                       for ch in events if "tool_calls" in ch["choices"][0]["delta"]]
        self.assertEqual(len(tool_deltas), 1)
        self.assertEqual(tool_deltas[0]["function"]["name"], "shell")
        self.assertEqual(json.loads(tool_deltas[0]["function"]["arguments"])["command"], "id -un && id -u")
        self.assertEqual(events[-1]["choices"][0]["finish_reason"], "tool_calls")

        # Follow-up request with tool output returns after_tool answer.
        status, _, raw2 = self._post("/v1/chat/completions", {
            "model": "stub-1",
            "stream": False,
            "messages": [
                {"role": "user", "content": "please run-whoami"},
                {"role": "assistant", "tool_calls": [{"id": "call_stub_1", "type": "function", "function": {"name": "shell", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "call_stub_1", "content": "crowbar\n1003\n"},
            ],
            "tools": [{"type": "function", "function": {"name": "shell"}}],
        })
        self.assertEqual(status, 200)
        body2 = json.loads(raw2)
        self.assertEqual(body2["choices"][0]["message"]["content"], "Ran as (stub-1): crowbar\n1003")

    def test_title_generation_request_with_no_tools_never_emits_tool_call(self):
        status, _, raw = self._post("/v1/chat/completions", {
            "model": "stub-1",
            "stream": False,
            "messages": [
                {"role": "system", "content": "Generate a short title for this conversation."},
                {"role": "user", "content": "please run-whoami"},
            ],
        })
        self.assertEqual(status, 200)
        body = json.loads(raw)
        self.assertEqual(body["choices"][0]["finish_reason"], "stop")
        self.assertEqual(body["choices"][0]["message"]["content"], "Stub thread")

    def test_error_rule_returns_http_error_json(self):
        import urllib.error

        req = urllib.request.Request(
            f"http://127.0.0.1:{self.port}/v1/chat/completions",
            data=json.dumps({
                "model": "stub-1",
                "stream": True,
                "messages": [{"role": "user", "content": "please make-fail"}],
                "tools": [{"type": "function", "function": {"name": "shell"}}],
            }).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            urllib.request.urlopen(req)
        with ctx.exception as err:
            self.assertEqual(err.code, 400)
            body = json.loads(err.read().decode("utf-8"))
        self.assertEqual(body["error"]["message"], "Stub model declined this turn")

    def test_forbid_scans_entire_request_including_system_and_earlier_messages(self):
        log = Path(self.tmp.name) / "forbid.jsonl"
        server = stub_model.start_server(
            self.script,
            log_path=log,
            forbidden=["WINSECRET_ABC", "CLIPSECRET_XYZ"],
        )
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        def post(payload):
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}/v1/chat/completions",
                data=json.dumps(payload).encode("utf-8"),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req) as resp:
                return resp.status

        # Clean request.
        self.assertEqual(post({
            "model": "stub-1",
            "messages": [{"role": "user", "content": "say hello"}],
        }), 200)
        # Sentinel hidden in system prompt (not in latest user prompt).
        self.assertEqual(post({
            "model": "stub-1",
            "messages": [
                {"role": "system", "content": "leaked WINSECRET_ABC here"},
                {"role": "user", "content": "say hello"},
            ],
        }), 200)
        # Sentinel hidden in an earlier user message.
        self.assertEqual(post({
            "model": "stub-1",
            "messages": [
                {"role": "user", "content": "earlier CLIPSECRET_XYZ"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "say hello"},
            ],
        }), 200)

        entries = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual([e["forbidden_found"] for e in entries], [False, True, True])
        self.assertEqual([e["forbidden_hit_count"] for e in entries], [0, 1, 1])
        # The log itself never echoes the forbidden secrets.
        raw_log = log.read_text()
        self.assertNotIn("WINSECRET_ABC", raw_log)
        self.assertNotIn("CLIPSECRET_XYZ", raw_log)

        # Every tool output so far is logged, so a check sees what the
        # harness told the Model of an act the person refused.
        self.assertEqual(post({
            "model": "stub-1",
            "messages": [
                {"role": "user", "content": "please run-cmd"},
                {"role": "assistant", "content": None},
                {"role": "tool", "content": "The user declined this tool call"},
                {"role": "user", "content": "and now?"},
            ],
        }), 200)
        last = json.loads(log.read_text().splitlines()[-1])
        self.assertEqual(last["history_tool_outputs"], ["The user declined this tool call"])
        self.assertIsNone(last["tool_output"])


if __name__ == "__main__":
    unittest.main()
