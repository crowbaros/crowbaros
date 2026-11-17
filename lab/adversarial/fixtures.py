"""What every fixture shares: paths, canonical JSON, the lab clock, the
canaries, HTTP requests and responses, the service base classes, scripted
rules, and idempotency keys. The record is in record.py, and injected faults
are in faults.py.

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
HOST = "127.0.0.1"
EPOCH = datetime(2026, 11, 2, 9, 0, 0, tzinfo=timezone.utc)
# Headers that only frame the HTTP message. Every other header that arrives is
# part of what reached the party, and the record keeps it.
FRAMING_HEADERS = ("connection", "content-length", "host", "transfer-encoding")
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


def require(condition, status, message):
    if not condition:
        raise Refuse(status, message)


def whole(value, minimum, name):
    """A whole number of at least minimum. A JSON true is not a number."""
    require(isinstance(value, int) and not isinstance(value, bool) and value >= minimum,
            400, f"{name} must be a whole number, {minimum} or more")
    return value


def check_fields(value, required, optional=(), what="the body"):
    """Refuse a JSON object unless it has every required field and no field
    outside required and optional."""
    require(isinstance(value, dict), 400, f"{what} must be a JSON object")
    unknown = sorted(set(value) - set(required) - set(optional))
    missing = [name for name in required if name not in value]
    require(not unknown and not missing, 400,
            f"{what}: unknown fields {unknown}, missing fields {missing}")
    return value


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

    def found(self, text):
        """Every marker in a text, sorted. Finds markers as written; a value
        encoded some other way, or split in pieces, is not found."""
        return sorted(marker for entries in self.markers().values()
                      for marker in entries.values() if marker in text)


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
        self.now += timedelta(seconds=whole(seconds, 0, "advance_seconds"))

    def set(self, text):
        try:
            moment = parse_iso(text)
        except (TypeError, ValueError):
            raise Refuse(400, "set must be a UTC time like 2026-11-02T09:00:00Z") from None
        require(moment >= self.now, 409, "the lab clock never goes back; reset the suite instead")
        self.now = moment


class Request:
    def __init__(self, method, target, headers, body, malformed=False):
        split = urllib.parse.urlsplit(target)
        self.method = method
        self.path = split.path
        self.query = split.query
        self.params = urllib.parse.parse_qs(split.query, keep_blank_values=True)
        self.headers = headers
        self.body = body
        self.malformed = malformed

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
        require(isinstance(value, dict), 400, "the body must be a JSON object")
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

    def header_pairs(self, skip=()):
        """Every header that arrived, as sorted [name, value] pairs, except the
        ones that only frame the message and any named in skip."""
        skipped = set(FRAMING_HEADERS) | {name.lower() for name in skip}
        return sorted([name.lower(), value] for name, value in self.headers.items()
                      if name.lower() not in skipped)

    def markers(self, canaries, skip_headers=()):
        """The canary markers in everything that arrived: the path, the query
        string, the headers, and the body, each read as sent and decoded."""
        parts = [self.path, urllib.parse.unquote(self.path), self.query,
                 urllib.parse.unquote_plus(self.query),
                 *(f"{name}: {value}" for name, value in self.header_pairs(skip_headers)),
                 self.body.decode("utf-8", "replace")]
        try:
            parts.append(json.dumps(json.loads(self.body), ensure_ascii=False))
        except (json.JSONDecodeError, UnicodeDecodeError):
            pass
        return canaries.found("\n".join(parts))

    def entry(self):
        return {"method": self.method, "path": self.path, "query": self.query,
                "host": self.host(), "headers": self.header_pairs(),
                "body": body_field(self.body)}


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
    """A fixture: its name, host name, kind, title, seed, and error shape. A
    subclass loads its seed in reset(). Nothing here may read the wall clock
    or anything random: time comes from self.clock."""

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

    def error(self, status, message):
        return json_response({"error": message}, status)


class Party(Service):
    """An outside party the system under test reaches. handle() answers one
    request and marks every change of state with Response.effect. The suite
    applies injected faults and records the request and its effects."""

    def handle(self, request):
        raise NotImplementedError

    def receipt(self, request):
        """The effect of the party merely receiving this request, or None. The
        suite records it for every request, refused or faulted ones included,
        unless handle() already marked an effect of the same name."""
        return None


class ScriptRules:
    """Scripted rules, tried in order; the first that matches gives its outcome.
    A rule's match may hold "contains", text the request must contain, and an
    exact value for each other key. "times" limits how often a rule answers.
    The stand-in for the trusted ask and the model provider both use these."""

    def __init__(self, rules, match_keys, outcome_key, check_outcome, also=()):
        """`also` names the other fields a rule may carry, for its caller."""
        require(isinstance(rules, list), 400, "rules must be a list")
        for rule in rules:
            check_fields(rule, (outcome_key,), ("match", "times", *also), "a rule")
            match = check_fields(rule.get("match", {}), (), ("contains", *match_keys), "match")
            require(all(isinstance(value, str) for value in match.values()), 400,
                    "match values must be strings")
            check_outcome(rule[outcome_key])
            if rule.get("times") is not None:
                whole(rule["times"], 1, "times")
        self.rules = rules
        self.outcome_key = outcome_key
        self.used = [0] * len(rules)

    def first(self, text, values):
        """The (index, outcome) of the first rule that matches, or None."""
        for index, rule in enumerate(self.rules):
            match = rule.get("match", {})
            if "contains" in match and match["contains"] not in text:
                continue
            if any(values.get(key) != value for key, value in match.items()
                   if key != "contains"):
                continue
            times = rule.get("times")
            if times is not None and self.used[index] >= times:
                continue
            self.used[index] += 1
            return index, rule[self.outcome_key]
        return None


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
        require(first == fingerprint, 409, "this Idempotency-Key was used for a different request")
        return json_response(value, status, [("Idempotent-Replayed", "true")])

    def store(self, key, fingerprint, status, value):
        if key:
            self.seen[key] = (fingerprint, status, value)


class Handler(http.server.BaseHTTPRequestHandler):
    """Hands every request to the suite. The server carries .suite and .service."""

    protocol_version = "HTTP/1.1"

    def version_string(self):
        return "crowbar-fixture/1"

    def date_time_string(self, timestamp=None):
        return format_datetime(self.server.suite.clock.now, usegmt=True)

    def log_message(self, format, *args):
        pass

    def read_body(self):
        """The request body, with chunked transfer encoding undone. Returns
        (body, malformed)."""
        if "chunked" not in (self.headers.get("Transfer-Encoding") or "").lower():
            try:
                length = int(self.headers.get("Content-Length") or 0)
            except ValueError:
                self.close_connection = True
                return b"", True
            return (self.rfile.read(length) if length > 0 else b""), False
        chunks = []
        while True:
            line = self.rfile.readline(65537)
            try:
                size = int(line.split(b";")[0].strip(), 16)
            except ValueError:
                self.close_connection = True
                return b"".join(chunks), True
            if size == 0:
                while self.rfile.readline(65537) not in (b"\r\n", b"\n", b""):
                    pass  # trailers
                return b"".join(chunks), False
            chunks.append(self.rfile.read(size))
            self.rfile.readline()

    def dispatch(self):
        body, malformed = self.read_body()
        request = Request(self.command, self.path, self.headers, body, malformed)
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

    def __getattr__(self, name):
        # Every HTTP method, OPTIONS, TRACE, or one the client made up, goes to
        # the suite, so nothing that reaches a fixture escapes the record.
        if name.startswith("do_"):
            return self.dispatch
        raise AttributeError(name)
