"""The fixture suite: every party, the stand-in for the trusted ask, and the
control service, each on its own loopback port, sharing one lab clock and one
record.

    suite = Suite(work_dir)
    suite.start()
    suite.reset("a01")      # fresh accounts, Data, clock, faults, and record
    ...                     # point the broker at suite.manifest()["services"]
    suite.stop()

Every request is handled under one lock, so the record is one total order. A
held ask waits outside the lock, so the control service can answer it.
"""
from http.server import ThreadingHTTPServer
from pathlib import Path
import re
import threading
import traceback

from fixtures import (DOMAIN, EPOCH, RECORD_FORMAT, Canaries, Clock, Faults, Handler,
                      Record, Refuse, Response, Service, canonical, iso, json_response,
                      load_seed, sha256)
import home
from services import PARTIES

RUN_NAME = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}\Z")
ANSWERS = ("yes", "no", "hold")


class Asks(Service):
    """The stand-in for the trusted ask. The system under test posts each ask
    with the fixed words it shows and the exact content, and gets the scripted
    answer. The real shell and confirm gesture are Milestone 4.

    POST /asks {"task", "kind", "words", "content", "destination"?}
      -> {"ask": "ask-0001", "answer": "yes" | "no"}

    The script's rules are tried in order; the first that matches answers, and
    a rule with "times" stops matching after that many asks. With no match the
    default answers, and the default is "no". The answer "hold" makes the ask
    wait until the control service answers it, so a test can look for effects
    while the ask is open. A held ask is answered "no" after the script's
    hold_timeout_seconds of real time, or when the suite resets.
    """

    name = "asks"
    kind = "stand-in for the trusted ask"
    title = "Trusted ask stand-in"
    FIELDS = ("task", "kind", "words", "content")

    def reset(self):
        self.pending = {}
        self.counter = 0
        self.set_script(load_seed("asks", self.canaries), record=False)

    def set_script(self, spec, record=True):
        allowed = {"default", "rules", "hold_timeout_seconds", "note"}
        if set(spec) - allowed:
            raise Refuse(400, f"unknown script fields: {sorted(set(spec) - allowed)}")
        default = spec.get("default", "no")
        timeout = spec.get("hold_timeout_seconds", 30)
        rules = spec.get("rules", [])
        if default not in ANSWERS or not isinstance(rules, list):
            raise Refuse(400, "default must be yes, no, or hold, and rules a list")
        if not isinstance(timeout, (int, float)) or isinstance(timeout, bool) or timeout <= 0:
            raise Refuse(400, "hold_timeout_seconds must be above 0")
        for rule in rules:
            if not isinstance(rule, dict) or set(rule) - {"match", "answer", "times"}:
                raise Refuse(400, "a rule has match, answer, and times only")
            match = rule.get("match", {})
            if not isinstance(match, dict) or set(match) - {"task", "kind", "contains"} or not all(
                    isinstance(v, str) for v in match.values()):
                raise Refuse(400, "match takes task, kind, and contains, each a string")
            if rule.get("answer") not in ANSWERS:
                raise Refuse(400, "a rule's answer must be yes, no, or hold")
            times = rule.get("times")
            if times is not None and (not isinstance(times, int) or times < 1):
                raise Refuse(400, "times must be 1 or more")
        self.script = {"default": default, "rules": rules, "hold_timeout_seconds": timeout}
        self.used = [0] * len(rules)
        if record:
            self.suite.record.add("control", "control", action="asks-script", script=self.script)

    def match(self, ask):
        text = canonical(ask)
        for index, rule in enumerate(self.script["rules"]):
            match = rule.get("match", {})
            if "task" in match and match["task"] != ask["task"]:
                continue
            if "kind" in match and match["kind"] != ask["kind"]:
                continue
            if "contains" in match and match["contains"] not in text:
                continue
            if rule.get("times") is not None and self.used[index] >= rule["times"]:
                continue
            self.used[index] += 1
            return index, rule["answer"]
        return "default", self.script["default"]

    def is_ask(self, request):
        return request.method == "POST" and request.path == "/asks"

    def handle(self, request):
        raise Refuse(404 if request.path != "/asks" else 405, "post asks to /asks")

    def serve_ask(self, request):
        """Called without the suite's lock: a held ask waits outside it."""
        suite = self.suite
        with suite.lock:
            try:
                ask = request.json()
                unknown = set(ask) - set(self.FIELDS) - {"destination"}
                missing = [name for name in self.FIELDS if name not in ask]
                if unknown or missing:
                    raise Refuse(400, f"an ask has {list(self.FIELDS)} and destination; "
                                      f"unknown {sorted(unknown)}, missing {missing}")
                for name in ("task", "kind", "words"):
                    if not isinstance(ask[name], str) or not ask[name]:
                        raise Refuse(400, f"{name} must be a non-empty string")
                if not isinstance(ask.get("destination", ""), str):
                    raise Refuse(400, "destination must be a string")
            except Refuse as refused:
                return suite.record_request(self, request, self.error(
                    refused.status, refused.message), None)
            self.counter += 1
            ask_id = f"ask-{self.counter:04d}"
            rule, answer = self.match(ask)
            suite.record.add("asks", "ask", ask=ask_id, rule=rule, task=ask["task"],
                             ask_kind=ask["kind"], words=ask["words"], content=ask["content"],
                             destination=ask.get("destination"))
            if answer != "hold":
                suite.record.add("asks", "answer", ask=ask_id, answer=answer, by="script")
                return json_response({"ask": ask_id, "answer": answer})
            held = {"ask": ask_id, "task": ask["task"], "kind": ask["kind"],
                    "event": threading.Event(), "answer": None}
            self.pending[ask_id] = held
            timeout = self.script["hold_timeout_seconds"]
        if not held["event"].wait(timeout):
            with suite.lock:
                if self.pending.get(ask_id) is held:
                    self.answer(ask_id, "no", "timeout")
        return json_response({"ask": ask_id, "answer": held["answer"]})

    def answer(self, ask_id, answer, by):
        """Answer a held ask. The caller holds the suite's lock."""
        if answer not in ("yes", "no"):
            raise Refuse(400, "answer must be yes or no")
        held = self.pending.pop(ask_id, None)
        if held is None:
            raise Refuse(404, f"no held ask {ask_id}")
        held["answer"] = answer
        self.suite.record.add("asks", "answer", ask=ask_id, answer=answer, by=by)
        held["event"].set()

    def release_all(self, by):
        for ask_id in sorted(self.pending):
            self.answer(ask_id, "no", by)


class Control(Service):
    """The lab's own service, for the smoke check and the red-team harness,
    never for the system under test. Reads are not recorded, so polling cannot
    change the record; every change is.

    GET  /manifest       services, host names, URLs, canaries, the home folder
    GET  /record         the current run's record, as JSON lines
    GET  /clock          {"now"}
    POST /clock          {"advance_seconds": n} or {"set": "2026-11-02T10:00:00Z"}
    POST /reset          {"run"?: name}: start a fresh run
    GET  /faults         armed faults
    POST /faults         arm one (see fixtures.Faults)
    POST /faults/clear   disarm all
    POST /asks/script    replace the ask script
    GET  /asks/pending   held asks
    POST /asks/answer    {"ask", "answer": "yes" | "no"}: answer a held ask
    """

    name = "control"
    kind = "lab control"
    title = "Lab control"

    def handle(self, request):
        suite = self.suite
        method, path = request.method, request.path
        if method == "GET":
            if path == "/manifest":
                return json_response(suite.manifest())
            if path == "/record":
                return Response(200, suite.record.text(), "application/x-ndjson")
            if path == "/clock":
                return json_response({"now": iso(suite.clock.now)})
            if path == "/faults":
                return json_response([suite.faults.public(f) for f in suite.faults.armed])
            if path == "/asks/pending":
                return json_response([{key: held[key] for key in ("ask", "task", "kind")}
                                      for _, held in sorted(suite.asks.pending.items())])
            raise Refuse(404, f"no route GET {path}")
        if method != "POST":
            raise Refuse(405, "control takes GET and POST")
        body = request.json() if request.body else {}
        if path == "/reset":
            run = suite.reset(body.get("run"))
            return json_response({"run": run, "record": str(suite.record.path)})
        if path == "/clock":
            if set(body) == {"advance_seconds"}:
                suite.clock.advance(body["advance_seconds"])
            elif set(body) == {"set"}:
                suite.clock.set(body["set"])
            else:
                raise Refuse(400, "send advance_seconds or set")
            suite.record.add("control", "control", action="clock", now=iso(suite.clock.now))
            return json_response({"now": iso(suite.clock.now)})
        if path == "/faults":
            fault = suite.faults.arm(body)
            suite.record.add("control", "control", action="fault", fault=fault)
            return json_response(fault, 201)
        if path == "/faults/clear":
            suite.faults.clear()
            suite.record.add("control", "control", action="faults-clear")
            return json_response({"armed": 0})
        if path == "/asks/script":
            suite.asks.set_script(body)
            return json_response(suite.asks.script)
        if path == "/asks/answer":
            if set(body) != {"ask", "answer"}:
                raise Refuse(400, "send ask and answer")
            suite.asks.answer(body["ask"], body["answer"], "control")
            return json_response({"ask": body["ask"], "answer": body["answer"]})
        raise Refuse(404, f"no route POST {path}")


class Suite:
    def __init__(self, work, host="127.0.0.1"):
        self.work = Path(work)
        self.home = self.work / "home"
        self.records = self.work / "records"
        self.host = host
        self.canaries = Canaries()
        self.clock = Clock()
        self.record = Record(self.clock)
        self.lock = threading.RLock()
        self.run = None
        self.runs = 0
        self.home_digest = None
        self.services = {}
        for cls in PARTIES:
            self.services[cls.name] = cls(self)
        self.faults = Faults(self)
        self.asks = Asks(self)
        self.control = Control(self)
        self.servers = {}

    def all_services(self):
        return [*self.services.values(), self.asks, self.control]

    def start(self):
        for service in self.all_services():
            server = ThreadingHTTPServer((self.host, 0), Handler)
            server.daemon_threads = True
            server.suite = self
            server.service = service
            thread = threading.Thread(target=server.serve_forever, name=f"fixture-{service.name}",
                                      daemon=True)
            thread.start()
            self.servers[service.name] = server

    def url(self, name):
        host, port = self.servers[name].server_address[:2]
        return f"http://{host}:{port}"

    def stop(self):
        with self.lock:
            self.asks.release_all("stop")
        for server in self.servers.values():
            server.shutdown()
            server.server_close()
        self.servers = {}
        self.record.close()

    def reset(self, run=None):
        """Start a fresh run: every account and its data from the seed, the home
        folder, the lab clock at the epoch, no faults, the default ask script,
        and a new record whose first entry is this reset."""
        with self.lock:
            if run is None:
                run = f"run-{self.runs + 1:04d}"
            if not isinstance(run, str) or not RUN_NAME.match(run):
                raise Refuse(400, "a run name is 1-64 of a-z, 0-9, '.', '_', '-'")
            self.runs += 1
            self.asks.release_all("reset")
            self.clock.reset()
            self.faults.clear()
            for service in self.all_services():
                service.reset()
            self.home_digest = home.write(self.home, self.canaries)
            self.run = run
            self.record.start(self.records / f"{run}.jsonl")
            self.record.add("control", "control", action="reset", run=run,
                            format=RECORD_FORMAT, home=self.home_digest)
            return run

    def serve(self, service, request):
        if request.host() != service.host:
            return self.stray(service, request)
        if service is self.asks and self.asks.is_ask(request):
            return self.asks.serve_ask(request)
        with self.lock:
            if service is self.control:
                try:
                    return self.control.handle(request)
                except Refuse as refused:
                    return self.control.error(refused.status, refused.message)
            fault = self.faults.take(service.name, request.method, request.path)
            response = None
            if fault is None or fault["kind"].endswith("after-effect"):
                try:
                    response = service.handle(request)
                except Refuse as refused:
                    response = service.error(refused.status, refused.message)
                except Exception:  # A bug in a fixture: answer 500 and record it.
                    traceback.print_exc()
                    response = service.error(500, "fixture error")
            return self.record_request(service, request, response, fault)

    def stray(self, service, request):
        """A request not addressed to the fixture by its host name: another
        program on the machine that found the port. It is refused and kept
        out of the record, which holds only what the system under test sent."""
        with self.lock:
            self.work.mkdir(parents=True, exist_ok=True)
            with (self.work / "strays.jsonl").open("a", encoding="utf-8") as stream:
                stream.write(canonical({"service": service.name, "method": request.method,
                                        "path": request.path, "host": request.host()}) + "\n")
        return Response(421, f"Misdirected request: address this fixture as {service.host}.\n")

    def record_request(self, service, request, response, fault):
        """Record a request and its effects. Returns what to send: the
        response, the fault's answer, or None to drop the connection."""
        sent = response
        if fault and fault["kind"].startswith("status"):
            sent = Response(fault["status"], fault["body"])
        elif fault:
            sent = None
        entry = request.entry()
        entry["status"] = sent.status if sent else None
        entry["response"] = ({"bytes": len(sent.body), "sha256": sha256(sent.body)}
                             if sent else None)
        if fault:
            entry["fault"] = fault["id"]
        seq = self.record.add(service.name, "request", **entry)
        for name, detail in (response.effects if response else []):
            self.record.add(service.name, "effect", effect=name, cause=seq, detail=detail)
        return sent

    def manifest(self):
        return {
            "format": RECORD_FORMAT,
            "domain": DOMAIN,
            "epoch": iso(EPOCH),
            "run": self.run,
            "record": str(self.record.path) if self.record.path else None,
            "home": str(self.home),
            "services": {s.name: {"host": s.host, "kind": s.kind, "title": s.title,
                                  "url": self.url(s.name) if s.name in self.servers else None}
                         for s in self.all_services()},
            "canaries": self.canaries.classes,
            "markers": self.canaries.markers(),
        }
