"""The record of one run, and the check of its format.

Every request that reached a fixture, every effect it had, every ask and
answer, and every control action: one canonical JSON object per line,
numbered from 1, timed by the lab clock.
"""
import json
import re

from fixtures import EPOCH, canonical, iso, parse_iso

RECORD_FORMAT = 2
CONTROL_ACTIONS = {
    "reset": ("run", "format", "home"),
    "clock": ("now",),
    "fault": ("fault",),
    "faults-clear": (),
    "asks-script": ("script",),
    "model-script": ("script",),
    "line-dismissed": ("line", "asks"),
    "task-ended": ("task", "asks"),
}
RECORD_KINDS = {
    "control": ("action",),
    "request": ("method", "path", "query", "host", "headers", "body", "status", "response"),
    "effect": ("effect", "cause", "detail"),
    "ask": ("ask", "rule", "task", "ask_kind", "words", "takes", "content", "items",
            "destination", "line", "sha256"),
    "answer": ("ask", "answer", "by", "with", "struck", "sha256"),
}
TIME = re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ\Z")


class Record:
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

    def close(self):
        if self.stream:
            self.stream.close()
            self.stream = None


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
