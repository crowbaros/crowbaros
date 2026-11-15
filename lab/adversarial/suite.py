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
from datetime import timedelta
from http.server import ThreadingHTTPServer
from pathlib import Path
import re
import threading
import traceback

from faults import Faults
from fixtures import (DOMAIN, EPOCH, HOST, Canaries, Clock, Handler, Party, Refuse, Response,
                      ScriptRules, Service, canonical, check_fields, iso, json_response,
                      load_seed, require, sha256, whole)
import home
from record import RECORD_FORMAT, Record
from services import PARTIES

RUN_NAME = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}\Z")
ANSWERS = ("yes", "no", "hold")


class Asks(Service):
    """The stand-in for the trusted ask. The system under test posts each ask
    with the fixed words it shows and the exact content, and gets the scripted
    answer. The real shell and confirm gesture are Milestone 4.

    POST /asks {"task", "kind", "words", "content", "destination"?}
      -> {"ask": "ask-0001", "answer": "yes" | "no"}

    The script's rules (see fixtures.ScriptRules) match on task, kind, and text
    the ask contains. With no match the default answers, and the default is
    "no". The answer "hold" makes the ask wait until the control service
    answers it, so a test can look for effects while the ask is open. A held
    ask is answered "no" once the lab clock passes hold_timeout_seconds after
    it was asked, or when the suite resets or stops. Real time plays no part.
    """

    name = "asks"
    kind = "stand-in for the trusted ask"
    title = "Trusted ask stand-in"
    FIELDS = ("task", "kind", "words", "content")

    def reset(self):
        self.pending = {}
        self.counter = 0
        self.set_script(load_seed("asks", self.canaries))

    def set_script(self, spec):
        check_fields(spec, (), ("default", "rules", "hold_timeout_seconds", "note"), "a script")
        default = spec.get("default", "no")
        require(default in ANSWERS, 400, "default must be yes, no, or hold")
        timeout = whole(spec.get("hold_timeout_seconds", 30), 1, "hold_timeout_seconds")
        rules = ScriptRules(spec.get("rules", []), ("task", "kind"), "answer",
                            lambda answer: require(answer in ANSWERS, 400,
                                                   "a rule's answer must be yes, no, or hold"))
        self.rules = rules
        self.script = {"default": default, "rules": rules.rules, "hold_timeout_seconds": timeout}
        return self.script

    def held(self):
        return [{key: held[key] for key in ("ask", "task", "kind")}
                for _, held in sorted(self.pending.items())]

    def parse(self, request):
        ask = check_fields(request.json(), self.FIELDS, ("destination",), "an ask")
        for name in ("task", "kind", "words"):
            require(isinstance(ask[name], str) and ask[name], 400,
                    f"{name} must be a non-empty string")
        require(isinstance(ask.get("destination", ""), str), 400, "destination must be a string")
        return ask

    def serve(self, request):
        """Takes the suite's lock itself: a held ask waits outside it."""
        suite = self.suite
        with suite.lock:
            if (request.method, request.path) != ("POST", "/asks"):
                status = 404 if request.path != "/asks" else 405
                return suite.record_request(self, request, self.error(status, "post asks to /asks"))
            fault = suite.faults.take(self.name, request.method, request.path)
            if fault and not fault.after_effect:
                return suite.record_request(self, request, None, fault)
            try:
                ask = self.parse(request)
            except Refuse as refused:
                return suite.record_request(self, request,
                                            self.error(refused.status, refused.message), fault)
            held = self.open(ask)
        held["event"].wait()
        response = json_response({"ask": held["ask"], "answer": held["answer"]})
        if fault:
            with suite.lock:
                return suite.record_request(self, request, response, fault)
        return response

    def open(self, ask):
        """Record an ask and its scripted answer. The caller holds the lock."""
        self.counter += 1
        ask_id = f"ask-{self.counter:04d}"
        rule, answer = self.rules.first(canonical(ask), ask) or ("default", self.script["default"])
        self.suite.record.add(self.name, "ask", ask=ask_id, rule=rule, task=ask["task"],
                              ask_kind=ask["kind"], words=ask["words"], content=ask["content"],
                              destination=ask.get("destination"))
        held = {"ask": ask_id, "task": ask["task"], "kind": ask["kind"],
                "event": threading.Event(), "answer": None,
                "deadline": self.clock.now + timedelta(seconds=self.script["hold_timeout_seconds"])}
        self.pending[ask_id] = held
        if answer != "hold":
            self.answer(ask_id, answer, "script")
        return held

    def answer(self, ask_id, answer, by):
        """Answer an open ask. The caller holds the suite's lock."""
        require(answer in ("yes", "no"), 400, "answer must be yes or no")
        held = self.pending.pop(ask_id, None)
        require(held is not None, 404, f"no held ask {ask_id}")
        held["answer"] = answer
        self.suite.record.add(self.name, "answer", ask=ask_id, answer=answer, by=by)
        held["event"].set()

    def expire(self):
        """Answer "no" to every held ask whose deadline the lab clock has passed."""
        for ask_id, held in sorted(self.pending.items()):
            if held["deadline"] <= self.clock.now:
                self.answer(ask_id, "no", "timeout")

    def release_all(self, by):
        for ask_id in sorted(self.pending):
            self.answer(ask_id, "no", by)


class Control(Service):
    """The lab's own service, for the smoke check and the red-team harness,
    never for the system under test. Reads are not recorded, so polling cannot
    change the record; every change is.

    GET  /manifest       services, host names, URLs, canaries, the Data folder
    GET  /record         the current run's record, as JSON lines
    GET  /clock          {"now"}
    POST /clock          {"advance_seconds": n} or {"set": "2026-11-02T10:00:00Z"}
    POST /reset          {"run"?: name}: start a fresh run
    GET  /faults         armed faults
    POST /faults         arm one (see faults.py)
    POST /faults/clear   disarm all
    POST /asks/script    replace the ask script
    GET  /asks/pending   held asks
    POST /asks/answer    {"ask", "answer": "yes" | "no"}: answer a held ask
    POST /model/script   replace the model provider's script
    """

    name = "control"
    kind = "lab control"
    title = "Lab control"

    def serve(self, request):
        with self.suite.lock:
            try:
                return self.handle(request)
            except Refuse as refused:
                return self.error(refused.status, refused.message)

    def handle(self, request):
        suite = self.suite
        method, path = request.method, request.path
        if method == "GET":
            reads = {
                "/manifest": lambda: json_response(suite.manifest()),
                "/record": lambda: Response(200, suite.record.text(), "application/x-ndjson"),
                "/clock": lambda: json_response({"now": iso(suite.clock.now)}),
                "/faults": lambda: json_response(suite.faults.listing()),
                "/asks/pending": lambda: json_response(suite.asks.held()),
            }
            require(path in reads, 404, f"no route GET {path}")
            return reads[path]()
        require(method == "POST", 405, "control takes GET and POST")
        body = request.json() if request.body else {}
        if path == "/reset":
            run = suite.reset(check_fields(body, (), ("run",)).get("run"))
            return json_response({"run": run, "record": str(suite.record.path)})
        if path == "/clock":
            require(set(body) in ({"advance_seconds"}, {"set"}), 400,
                    "send advance_seconds or set")
            if "set" in body:
                suite.clock.set(body["set"])
            else:
                suite.clock.advance(body["advance_seconds"])
            suite.record.add(self.name, "control", action="clock", now=iso(suite.clock.now))
            suite.asks.expire()
            return json_response({"now": iso(suite.clock.now)})
        if path == "/faults":
            fault = suite.faults.arm(body).public()
            suite.record.add(self.name, "control", action="fault", fault=fault)
            return json_response(fault, 201)
        if path == "/faults/clear":
            suite.faults.clear()
            suite.record.add(self.name, "control", action="faults-clear")
            return json_response({"armed": 0})
        scripts = {"/asks/script": ("asks-script", suite.asks),
                   "/model/script": ("model-script", suite.model)}
        if path in scripts:
            action, service = scripts[path]
            script = service.set_script(body)
            suite.record.add(self.name, "control", action=action, script=script)
            return json_response(script)
        if path == "/asks/answer":
            check_fields(body, ("ask", "answer"))
            suite.asks.answer(body["ask"], body["answer"], "control")
            return json_response(body)
        raise Refuse(404, f"no route POST {path}")


class Suite:
    def __init__(self, work):
        self.work = Path(work)
        self.home = self.work / "home"
        self.records = self.work / "records"
        self.canaries = Canaries()
        self.clock = Clock()
        self.record = Record(self.clock)
        self.lock = threading.RLock()
        self.run = None
        self.runs = 0
        self.home_digest = None
        self.services = {cls.name: cls(self) for cls in PARTIES}
        self.model = self.services["model"]
        self.asks = Asks(self)
        self.control = Control(self)
        self.faults = Faults([*self.services, self.asks.name], self.canaries)
        self.servers = {}

    def all_services(self):
        return [*self.services.values(), self.asks, self.control]

    def start(self):
        for service in self.all_services():
            server = ThreadingHTTPServer((HOST, 0), Handler)
            server.daemon_threads = True
            server.suite = self
            server.service = service
            thread = threading.Thread(target=server.serve_forever, name=f"fixture-{service.name}",
                                      daemon=True)
            thread.start()
            self.servers[service.name] = server

    def url(self, name):
        return f"http://{HOST}:{self.servers[name].server_address[1]}"

    def stop(self):
        with self.lock:
            self.asks.release_all("stop")
        for server in self.servers.values():
            server.shutdown()
            server.server_close()
        self.servers = {}
        self.record.close()

    def reset(self, run=None):
        """Start a fresh run: every account and its data from the seed, the Data
        folder, the lab clock at the epoch, no faults, the default scripts, and
        a new record whose first entry is this reset."""
        with self.lock:
            if run is None:
                run = f"run-{self.runs + 1:04d}"
            require(isinstance(run, str) and RUN_NAME.match(run), 400,
                    "a run name is 1-64 of a-z, 0-9, '.', '_', '-'")
            self.runs += 1
            self.asks.release_all("reset")
            self.clock.reset()
            self.faults.clear()
            for service in self.all_services():
                service.reset()
            self.home_digest = home.write(self.home, self.canaries)
            self.run = run
            self.record.start(self.records / f"{run}.jsonl")
            self.record.add(self.control.name, "control", action="reset", run=run,
                            format=RECORD_FORMAT, home=self.home_digest)
            return run

    def serve(self, service, request):
        if request.host() != service.host:
            return self.stray(service, request)
        if not isinstance(service, Party):
            return service.serve(request)
        with self.lock:
            if request.malformed:
                return self.record_request(service, request,
                                           service.error(400, "malformed request body"))
            fault = self.faults.take(service.name, request.method, request.path)
            response = None
            if fault is None or fault.after_effect:
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

    def record_request(self, service, request, response, fault=None):
        """Record a request and the effects of its response. Returns what to
        send: the response, the fault's answer, or None to drop the connection.
        The caller holds the lock."""
        sent = fault.answer(service) if fault else response
        entry = request.entry()
        entry["status"] = sent.status if sent else None
        entry["response"] = {"bytes": len(sent.body), "sha256": sha256(sent.body)} if sent else None
        if fault:
            entry["fault"] = fault.id
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
