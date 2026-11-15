"""The core of the adversarial lab: the lab clock, the record, injected faults,
and the HTTP plumbing every fixture service shares.

Stdlib only. Each service listens on 127.0.0.1 on a port the system picks, and
is named by a fixture host name such as `mail.fixture.test`. Ports never reach
the record or a response body, so the same run gives the same record.
"""
from datetime import datetime, timedelta, timezone
from email.utils import format_datetime
import hashlib
import http.cookies
import http.server
import json
from pathlib import Path
import re
import urllib.parse

LAB = Path(__file__).resolve().parent
ROOT = LAB.parents[1]
SEED = LAB / "seed"
WORK = ROOT / ".lab/adversarial"
DOMAIN = "fixture.test"
EPOCH = datetime(2026, 11, 2, 9, 0, 0, tzinfo=timezone.utc)
RECORD_FORMAT = 1
# Request headers the record keeps. Everything else (User-Agent, Accept, ...)
# varies by client and says nothing about what reached the party.
RECORDED_HEADERS = ("authorization", "content-type", "cookie", "idempotency-key")
BODY_LIMIT = 65536
CANARY_CLASSES = ("planted", "private", "secret")
PLACEHOLDER = re.compile(r"\{\{(planted|private|secret):([A-Z0-9-]+)\}\}")


def canonical(value):
    """The one JSON encoding the record uses: sorted keys, no spaces."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def iso(moment):
    return moment.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_iso(text):
    return datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


class Refuse(Exception):
    """A request the fixture answers with an error status."""

    def __init__(self, status, message):
        super().__init__(message)
        self.status = status
        self.message = message


class Canaries:
    """The strings a run tracks, from seed/canaries.json. Each carries a marker,
    CB-<CLASS>-<ID>, that a check can search for."""

    def __init__(self, path=SEED / "canaries.json"):
        raw = json.loads(path.read_text())
        self.classes = {cls: dict(raw[cls]) for cls in CANARY_CLASSES}
        for cls, entries in self.classes.items():
            for cid, text in entries.items():
                if self.marker(cls, cid) not in text:
                    raise ValueError(f"canary {cls}:{cid} does not carry its marker")

    @staticmethod
    def marker(cls, cid):
        return f"CB-{cls.upper()}-{cid}"

    def text(self, cls, cid):
        try:
            return self.classes[cls][cid]
        except KeyError:
            raise ValueError(f"no canary {cls}:{cid}") from None

    def expand(self, value):
        """Replace every {{class:ID}} in a seed value with the canary's text."""
        if isinstance(value, str):
            return PLACEHOLDER.sub(lambda m: self.text(m.group(1), m.group(2)), value)
        if isinstance(value, list):
            return [self.expand(item) for item in value]
        if isinstance(value, dict):
            return {key: self.expand(item) for key, item in value.items()}
        return value

    def markers(self):
        return {cls: {cid: self.marker(cls, cid) for cid in entries}
                for cls, entries in self.classes.items()}


def load_seed(name, canaries):
    return canaries.expand(json.loads((SEED / f"{name}.json").read_text()))


class Clock:
    """The lab clock. It starts at EPOCH on every reset and moves only when the
    control service moves it, and never backwards."""

    def __init__(self):
        self.reset()

    def reset(self):
        self.now = EPOCH

    def advance(self, seconds):
        if not isinstance(seconds, int) or isinstance(seconds, bool) or seconds < 0:
            raise Refuse(400, "advance_seconds must be a whole number of seconds, 0 or more")
        self.now += timedelta(seconds=seconds)

    def set(self, text):
        try:
            moment = parse_iso(text)
        except (TypeError, ValueError):
            raise Refuse(400, "set must be a UTC time like 2026-11-02T09:00:00Z") from None
        if moment < self.now:
            raise Refuse(409, "the lab clock never goes back; reset the suite instead")
        self.now = moment


class Record:
    """The append-only record of one run: every request that reached a fixture,
    every effect it had, every ask and answer, and every control action. One
    canonical JSON object per line, numbered from 1, timed by the lab clock."""

    def __init__(self, clock):
        self.clock = clock
        self.lines = []
        self.path = None
        self.stream = None

    def start(self, path):
        self.close()
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path
        self.stream = path.open("w", encoding="utf-8")
        self.lines = []

    def add(self, service, kind, **fields):
        entry = {"seq": len(self.lines) + 1, "t": iso(self.clock.now),
                 "service": service, "kind": kind, **fields}
        line = canonical(entry)
        self.lines.append(line)
        if self.stream:
            self.stream.write(line + "\n")
            self.stream.flush()
        return entry["seq"]

    def text(self):
        return "".join(line + "\n" for line in self.lines)

    def digest(self):
        return sha256(self.text().encode())

    def close(self):
        if self.stream:
            self.stream.close()
            self.stream = None


class Request:
    def __init__(self, method, target, headers, body):
        split = urllib.parse.urlsplit(target)
        self.method = method
        self.path = split.path
        self.query = split.query
        self.params = urllib.parse.parse_qs(split.query, keep_blank_values=True)
        self.headers = headers
        self.body = body

    def header(self, name):
        return self.headers.get(name)

    def param(self, name):
        values = self.params.get(name)
        return values[0] if values else None

    def json(self):
        try:
            value = json.loads(self.body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise Refuse(400, "the body must be JSON") from None
        if not isinstance(value, dict):
            raise Refuse(400, "the body must be a JSON object")
        return value

    def form(self):
        try:
            text = self.body.decode("utf-8")
        except UnicodeDecodeError:
            raise Refuse(400, "the form must be UTF-8") from None
        return {key: values[0] for key, values in
                urllib.parse.parse_qs(text, keep_blank_values=True).items()}

    def bearer(self):
        value = self.header("Authorization") or ""
        return value[7:] if value.startswith("Bearer ") else None

    def cookie(self, name):
        jar = http.cookies.SimpleCookie()
        try:
            jar.load(self.header("Cookie") or "")
        except http.cookies.CookieError:
            return None
        return jar[name].value if name in jar else None

    def host(self):
        """The Host header without its port, so the record does not depend on
        which port the system picked."""
        return re.sub(r":\d+\Z", "", self.header("Host") or "")

    def entry(self):
        headers = {name: self.header(name) for name in RECORDED_HEADERS
                   if self.header(name) is not None}
        return {"method": self.method, "path": self.path, "query": self.query,
                "host": self.host(), "headers": headers, "body": body_field(self.body)}


def body_field(data):
    """A body as the record keeps it: the text when it is short UTF-8,
    otherwise its size and digest."""
    if len(data) <= BODY_LIMIT:
        try:
            return data.decode("utf-8")
        except UnicodeDecodeError:
            pass
    return {"bytes": len(data), "sha256": sha256(data)}


class Response:
    def __init__(self, status=200, body=b"", content_type="text/plain; charset=utf-8",
                 headers=()):
        self.status = status
        self.body = body.encode("utf-8") if isinstance(body, str) else body
        self.content_type = content_type
        self.headers = list(headers)
        self.effects = []

    def effect(self, name, **detail):
        """Record a change of state at the party: the thing an attack test
        checks did not happen."""
        self.effects.append((name, detail))
        return self


def json_response(value, status=200, headers=()):
    return Response(status, json.dumps(value, sort_keys=True, indent=2) + "\n",
                    "application/json", headers)


def html_response(text, status=200, headers=()):
    return Response(status, text, "text/html; charset=utf-8", headers)


class Service:
    """One fixture party. A subclass sets name, kind, and title, loads its seed
    in reset(), and answers requests in handle(). Neither may read the wall
    clock or anything random: time comes from self.clock."""

    name = None
    kind = None
    title = None

    def __init__(self, suite):
        self.suite = suite
        self.canaries = suite.canaries
        self.reset()

    @property
    def host(self):
        return f"{self.name}.{DOMAIN}"

    @property
    def clock(self):
        return self.suite.clock

    def reset(self):
        pass

    def handle(self, request):
        raise NotImplementedError

    def error(self, status, message):
        return json_response({"error": message}, status)


class Faults:
    """Errors armed through the control service. A fault fires on the nth
    request after arming that matches its service, method, and path, and on
    the count - 1 matching requests after that.

    status               answer with the given status and body; no effect
    drop                 close the connection without an answer; no effect
    status-after-effect  the effect happens, then the answer is replaced
    drop-after-effect    the effect happens, then the connection is closed
    """

    KINDS = ("status", "drop", "status-after-effect", "drop-after-effect")

    def __init__(self, suite):
        self.suite = suite
        self.clear()

    def clear(self):
        self.armed = []
        self.counter = 0

    def arm(self, spec):
        allowed = {"service", "method", "path", "nth", "count", "kind", "status", "body"}
        if set(spec) - allowed:
            raise Refuse(400, f"unknown fault fields: {sorted(set(spec) - allowed)}")
        if spec.get("service") not in self.suite.services:
            raise Refuse(400, f"service must be one of {sorted(self.suite.services)}")
        if spec.get("kind") not in self.KINDS:
            raise Refuse(400, f"kind must be one of {list(self.KINDS)}")
        if not isinstance(spec.get("method"), str) or not isinstance(spec.get("path"), str):
            raise Refuse(400, "method and path are required strings")
        fault = {"service": spec["service"], "method": spec["method"].upper(),
                 "path": spec["path"], "kind": spec["kind"],
                 "nth": spec.get("nth", 1), "count": spec.get("count", 1)}
        for field in ("nth", "count"):
            value = fault[field]
            if not isinstance(value, int) or isinstance(value, bool) or value < 1:
                raise Refuse(400, f"{field} must be a whole number, 1 or more")
        if fault["kind"].startswith("status"):
            status = spec.get("status")
            if not isinstance(status, int) or not 400 <= status <= 599:
                raise Refuse(400, "a status fault needs a status from 400 to 599")
            fault["status"] = status
            fault["body"] = self.suite.canaries.expand(spec.get("body", ""))
            if not isinstance(fault["body"], str):
                raise Refuse(400, "body must be a string")
        self.counter += 1
        fault["id"] = f"fault-{self.counter:04d}"
        fault["seen"] = 0
        fault["fired"] = 0
        self.armed.append(fault)
        return self.public(fault)

    @staticmethod
    def public(fault):
        return {key: value for key, value in fault.items() if key not in ("seen", "fired")}

    def take(self, service, method, path):
        for fault in self.armed:
            if (fault["service"], fault["method"], fault["path"]) != (service, method, path):
                continue
            fault["seen"] += 1
            if fault["seen"] < fault["nth"]:
                continue
            fault["fired"] += 1
            if fault["fired"] >= fault["count"]:
                self.armed.remove(fault)
            return fault
        return None


class Handler(http.server.BaseHTTPRequestHandler):
    """Hands every request to the suite. The server carries .suite and .service."""

    protocol_version = "HTTP/1.1"

    def version_string(self):
        return "crowbar-fixture/1"

    def date_time_string(self, timestamp=None):
        return format_datetime(self.server.suite.clock.now, usegmt=True)

    def log_message(self, format, *args):
        pass

    def dispatch(self):
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = 0
        body = self.rfile.read(length) if length > 0 else b""
        request = Request(self.command, self.path, self.headers, body)
        response = self.server.suite.serve(self.server.service, request)
        if response is None:  # An injected drop: close without an answer.
            self.close_connection = True
            return
        self.send_response(response.status)
        self.send_header("Content-Type", response.content_type)
        self.send_header("Content-Length", str(len(response.body)))
        for name, value in response.headers:
            self.send_header(name, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(response.body)

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = do_HEAD = dispatch


class Idempotency:
    """Idempotency keys for an act. The same key with the same request replays
    the first answer and has no second effect; the same key with a different
    request is refused."""

    def __init__(self):
        self.seen = {}

    def replay(self, key, fingerprint):
        if key not in self.seen:
            return None
        first, status, value = self.seen[key]
        if first != fingerprint:
            raise Refuse(409, "this Idempotency-Key was used for a different request")
        return json_response(value, status, [("Idempotent-Replayed", "true")])

    def store(self, key, fingerprint, status, value):
        if key:
            self.seen[key] = (fingerprint, status, value)


CONTROL_ACTIONS = {
    "reset": ("run", "format", "home"),
    "clock": ("now",),
    "fault": ("fault",),
    "faults-clear": (),
    "asks-script": ("script",),
}
RECORD_KINDS = {
    "control": ("action",),
    "request": ("method", "path", "query", "host", "headers", "body", "status", "response"),
    "effect": ("effect", "cause", "detail"),
    "ask": ("ask", "rule", "task", "ask_kind", "words", "content", "destination"),
    "answer": ("ask", "answer", "by"),
}
TIME = re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ\Z")


def record_problems(lines, services):
    """Every way a record breaks the format, as a list of messages. An empty
    list means the record is well formed."""
    problems = []
    last_time = None
    requests, asks = {}, set()
    for index, line in enumerate(lines, 1):
        where = f"line {index}"
        try:
            entry = json.loads(line)
        except json.JSONDecodeError as error:
            problems.append(f"{where}: not JSON ({error})")
            continue
        if not isinstance(entry, dict) or canonical(entry) != line:
            problems.append(f"{where}: not canonical JSON")
            continue
        kind = entry.get("kind")
        if entry.get("seq") != index:
            problems.append(f"{where}: seq is {entry.get('seq')}, expected {index}")
        if not isinstance(entry.get("t"), str) or not TIME.match(entry["t"]):
            problems.append(f"{where}: t is not a lab clock time")
        else:
            if parse_iso(entry["t"]) < EPOCH or (last_time and entry["t"] < last_time):
                problems.append(f"{where}: t goes back or is before the epoch")
            last_time = entry["t"]
        if entry.get("service") not in services:
            problems.append(f"{where}: unknown service {entry.get('service')!r}")
        if kind not in RECORD_KINDS:
            problems.append(f"{where}: unknown kind {kind!r}")
            continue
        required = set(RECORD_KINDS[kind])
        if kind == "control":
            if entry.get("action") not in CONTROL_ACTIONS:
                problems.append(f"{where}: unknown control action {entry.get('action')!r}")
                continue
            required |= set(CONTROL_ACTIONS[entry["action"]])
        expected = {"seq", "t", "service", "kind", *required}
        if kind == "request":
            expected.add("fault")
        missing = required - set(entry)
        unknown = set(entry) - expected
        if missing or unknown:
            problems.append(f"{where}: {kind} missing {sorted(missing)}, unknown {sorted(unknown)}")
        if index == 1 and (kind, entry.get("action"), entry.get("format")) != (
                "control", "reset", RECORD_FORMAT):
            problems.append(f"{where}: a record starts with a reset in format {RECORD_FORMAT}")
        if kind == "request":
            requests[entry["seq"]] = entry["service"]
        elif kind == "effect" and requests.get(entry.get("cause")) != entry["service"]:
            problems.append(f"{where}: effect's cause is not an earlier request to its service")
        elif kind == "ask":
            asks.add(entry.get("ask"))
        elif kind == "answer" and entry.get("ask") not in asks:
            problems.append(f"{where}: answer to an ask the record does not hold")
    return problems
