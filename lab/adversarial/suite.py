"""The fixture suite: every party, the stand-in for the trusted ask, and the
control service, each on its own loopback port, sharing one lab clock and one
record.

    suite = Suite(work_dir)
    suite.start()
    suite.reset("a01")      # fresh accounts, Data, clock, faults, and record
    ...                     # point the broker at suite.manifest()["services"]
    suite.stop()

Every request is handled under one lock, so the record is one total order. A
waiting ask waits outside the lock, so the control service can answer it.
"""
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
# What a script can say to an ask: yes, no, or leave it waiting.
ANSWERS = ("yes", "no", "wait")
# The answer an ask takes, and the answer a yes was given with (ADR 0026).
KINDS = ("plain", "confirm-gesture", "signature")


def digest(shown):
    """The digest of what an ask shows, or of what an answer covers."""
    return sha256(canonical(shown).encode())


class Asks(Service):
    """The stand-in for the trusted ask. The system under test posts each ask
    with the fixed words it shows, the exact content, and the answer it takes,
    and gets the scripted answer. The real shell and confirm gesture are
    Milestone 4.

    POST /asks {"task", "kind", "words", "takes", "content" or "items",
                "destination"?, "line"?}
      -> {"ask": "ask-0001", "answer": "yes" | "no", "with", "struck", "sha256"}

    - "takes" is the answer the ask takes: plain, confirm-gesture, or
      signature. "with" is the answer a yes was given with. The stand-in gives
      the one the ask takes unless the script names another, so a run can give
      the wrong one. It is the system under test that must not accept it.
    - An ask holds one act, in "content", or a list of acts of one kind, in
      "items". An answer may strike items of a list. "struck" lists their
      places, from 0, and "sha256" is the digest of what the answer covers:
      the content, or the items that are left.
    - "line" marks an ask the gate held, with the line it waits in: one line
      for each principal, and one for the Agent and all its tasks. A held ask
      waits whatever the script says, until the control service answers it or
      dismisses its line.

    The script's rules (see fixtures.ScriptRules) match on task, kind, takes,
    and text the ask contains. With no match the default answers, and the
    default is "no". The answer "wait" leaves the ask waiting until the
    control service answers it, so a test can look for effects while the ask
    is open. No ask ends on a timer. One that nobody answers ends with its
    task, when the control service ends the task, or when the suite resets
    or stops.
    """

    name = "asks"
    kind = "stand-in for the trusted ask"
    title = "Trusted ask stand-in"
    FIELDS = ("task", "kind", "words", "takes")

    def reset(self):
        self.pending = {}
        self.counter = 0
        self.set_script(load_seed("asks", self.canaries))

    def set_script(self, spec):
        check_fields(spec, (), ("default", "rules", "note"), "a script")
        default = spec.get("default", "no")
        require(default in ANSWERS, 400, "default must be yes, no, or wait")
        rules = ScriptRules(spec.get("rules", []), ("task", "kind", "takes"), "answer",
                            lambda answer: require(answer in ANSWERS, 400,
                                                   "a rule's answer must be yes, no, or wait"),
                            also=("with", "strike"))
        for rule in rules.rules:
            require(rule.get("with") is None or rule["with"] in KINDS, 400,
                    f"with must be one of {list(KINDS)}")
            self.places(rule.get("strike", []))
        self.rules = rules
        self.script = {"default": default, "rules": rules.rules}
        return self.script

    @staticmethod
    def places(strike):
        """The places a strike names, sorted, each once."""
        require(isinstance(strike, list), 400, "strike must be a list of places, from 0")
        return sorted({whole(place, 0, "a place in strike") for place in strike})

    def waiting(self):
        return [{key: held[key] for key in ("ask", "task", "kind", "takes", "line")}
                for _, held in sorted(self.pending.items())]

    def parse(self, request):
        ask = check_fields(request.json(), self.FIELDS,
                           ("content", "items", "destination", "line"), "an ask")
        for name in ("task", "kind", "words"):
            require(isinstance(ask[name], str) and ask[name], 400,
                    f"{name} must be a non-empty string")
        require(ask["takes"] in KINDS, 400, f"takes must be one of {list(KINDS)}")
        require(("content" in ask) != ("items" in ask), 400,
                "an ask holds one act in content, or a list of acts in items")
        require("items" not in ask or (isinstance(ask["items"], list) and ask["items"]), 400,
                "items must be a list of at least one act")
        require(isinstance(ask.get("destination", ""), str), 400, "destination must be a string")
        require("line" not in ask or (isinstance(ask["line"], str) and ask["line"]), 400,
                "line must be a non-empty string")
        return ask

    def serve(self, request):
        """Takes the suite's lock itself: a waiting ask waits outside it."""
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
            held = self.open(ask, request, fault)
        held["event"].wait()
        return held["sent"]

    def open(self, ask, request, fault):
        """Record an ask and its scripted answer. The caller holds the lock."""
        self.counter += 1
        ask_id = f"ask-{self.counter:04d}"
        shown = ask["items"] if "items" in ask else ask["content"]
        line = ask.get("line")
        index, answer = self.rules.first(canonical(ask), ask) or ("default", self.script["default"])
        rule = self.rules.rules[index] if index != "default" else {}
        self.suite.record.add(self.name, "ask", ask=ask_id, rule=index, task=ask["task"],
                              ask_kind=ask["kind"], words=ask["words"], takes=ask["takes"],
                              content=ask.get("content"), items=ask.get("items"),
                              destination=ask.get("destination"), line=line,
                              sha256=digest(shown))
        held = {"ask": ask_id, "task": ask["task"], "kind": ask["kind"], "takes": ask["takes"],
                "line": line, "shown": shown, "is_list": "items" in ask,
                "event": threading.Event(), "sent": None, "request": request, "fault": fault}
        self.pending[ask_id] = held
        # A held ask is not put to the person, so no rule answers it.
        if line is None and answer != "wait":
            try:
                self.answer(ask_id, answer, "script", rule.get("with"), rule.get("strike", []))
            except Refuse:
                pass  # The rule's strike does not fit this ask: it waits.
        return held

    def answer(self, ask_id, answer, by, given_with=None, strike=()):
        """Answer a waiting ask. The caller holds the suite's lock."""
        require(answer in ("yes", "no"), 400, "answer must be yes or no")
        require(given_with is None or given_with in KINDS, 400,
                f"with must be one of {list(KINDS)}")
        held = self.pending.get(ask_id)
        require(held is not None, 404, f"no waiting ask {ask_id}")
        struck = self.places(list(strike)) if answer == "yes" else []
        covers = held["shown"]
        if struck:
            require(held["is_list"], 400, "only a list ask can have an item struck")
            require(struck[-1] < len(covers), 400, "strike names a place the list does not hold")
            require(len(struck) < len(covers), 400,
                    "striking every item is a decline: answer no instead")
            covers = [item for place, item in enumerate(covers) if place not in struck]
        del self.pending[ask_id]
        given = (given_with or held["takes"]) if answer == "yes" else None
        result = {"ask": ask_id, "answer": answer, "with": given, "struck": struck,
                  "sha256": digest(covers)}
        self.suite.record.add(self.name, "answer", by=by, **result)
        held["sent"] = json_response(result)
        if held["fault"]:
            # Recorded now, under the lock, so a reset that releases the ask
            # cannot move its request into the next run's record.
            held["sent"] = self.suite.record_request(self, held["request"], held["sent"],
                                                     held["fault"])
        held["event"].set()

    def decline(self, by, chosen):
        """Answer "no" to every waiting ask that `chosen` picks, in order. The
        caller holds the suite's lock."""
        for ask_id in [ask for ask, held in sorted(self.pending.items()) if chosen(held)]:
            self.answer(ask_id, "no", by)

    def release_all(self, by):
        self.decline(by, lambda held: True)


class Hand(Service):
    """The stand-in for the person's hand: what the compositor and the shell
    see the person do (ADR 0025). The compositor's four paste acts are its
    fourth slice, so until then a run's script makes each act here.

    Only the control service makes an act: a paste, a drop, a click on a
    link, or a turn the person types. The system under test reads them, and
    can make none. Anything else it sends is a request in the record, so a
    compartment that claims an act the person did not make is seen.

    GET /acts         every act so far, in order
    GET /acts/{id}    one act

    An act names where it happened by the names the run gives its windows
    and conversations. A paste and a drop move "content" from one to another:
    text, or a path in the Data folder. A click opens the "address" of a link
    shown "in" a window. A typed turn is the "words", exactly as typed, "in" a
    conversation.
    """

    name = "hand"
    kind = "stand-in for the person's hand"
    title = "The person's hand stand-in"
    ACTS = {"paste": ("from", "to", "content"), "drop": ("from", "to", "content"),
            "click": ("in", "address"), "type": ("in", "words")}

    def reset(self):
        self.acts = []

    def make(self, spec):
        """Record an act of the person's. The caller holds the suite's lock."""
        require(isinstance(spec, dict) and spec.get("act") in self.ACTS, 400,
                f"act must be one of {list(self.ACTS)}")
        parts = self.ACTS[spec["act"]]
        spec = self.canaries.expand(check_fields(spec, ("act", *parts), (), "an act"))
        for name in parts:
            require(isinstance(spec[name], str) and spec[name], 400,
                    f"{name} must be a non-empty string")
        act = {"hand": f"hand-{len(self.acts) + 1:04d}", **spec}
        self.acts.append(act)
        self.suite.record.add(self.name, "hand", hand=act["hand"], act=act["act"],
                              detail={name: act[name] for name in parts})
        return act

    def serve(self, request):
        suite = self.suite
        with suite.lock:
            one = re.fullmatch(r"/acts/(hand-\d{4})", request.path)
            if request.method == "GET" and request.path == "/acts":
                return json_response(self.acts)
            if request.method == "GET" and one:
                found = [act for act in self.acts if act["hand"] == one.group(1)]
                return json_response(found[0]) if found else self.error(404, "no such act")
            status = 405 if one or request.path == "/acts" else 404
            return suite.record_request(self, request, self.error(
                status, "the person's acts are read here; nothing the system under test "
                        "sends makes one"))


class Control(Service):
    """The lab's own service, for the smoke check and the red-team harness,
    never for the system under test. Reads are not recorded, so polling cannot
    change the record; every change is.

    GET  /manifest       services, host names, URLs, canaries, the Data folder,
                         its label records
    GET  /record         the current run's record, as JSON lines
    GET  /clock          {"now"}
    POST /clock          {"advance_seconds": n} or {"set": "2026-11-02T10:00:00Z"}
    POST /reset          {"run"?: name}: start a fresh run
    GET  /faults         armed faults
    POST /faults         arm one (see faults.py)
    POST /faults/clear   disarm all
    POST /asks/script    replace the ask script
    GET  /asks/pending   waiting asks, each with its line if the gate held it
    POST /asks/answer    {"ask", "answer": "yes" | "no", "with"?, "strike"?}:
                         answer a waiting ask
    POST /asks/dismiss   {"line"}: decline every ask held in a line
    POST /tasks/end      {"task"}: end a task, which declines the asks it left
                         waiting
    POST /hand           {"act": "paste" | "drop" | "click" | "type", ...}: make
                         an act of the person's (see Hand)
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
                "/asks/pending": lambda: json_response(suite.asks.waiting()),
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
            check_fields(body, ("ask", "answer"), ("with", "strike"))
            suite.asks.answer(body["ask"], body["answer"], "control", body.get("with"),
                              body.get("strike", []))
            return json_response(body)
        if path == "/hand":
            return json_response(suite.hand.make(body), 201)
        if path == "/asks/dismiss":
            line = check_fields(body, ("line",))["line"]
            in_line = lambda held: held["line"] == line
            asks = sorted(ask for ask, held in suite.asks.pending.items() if in_line(held))
            require(asks, 404, f"no ask is held in the line {line!r}")
            suite.record.add(self.name, "control", action="line-dismissed", line=line, asks=asks)
            suite.asks.decline("dismiss", in_line)
            return json_response({"line": line, "asks": asks})
        if path == "/tasks/end":
            task = check_fields(body, ("task",))["task"]
            require(isinstance(task, str) and task, 400, "task must be a non-empty string")
            of_task = lambda held: held["task"] == task
            asks = sorted(ask for ask, held in suite.asks.pending.items() if of_task(held))
            suite.record.add(self.name, "control", action="task-ended", task=task, asks=asks)
            suite.asks.decline("task-ended", of_task)
            return json_response({"task": task, "asks": asks})
        raise Refuse(404, f"no route POST {path}")


class Suite:
    def __init__(self, work):
        self.work = Path(work)
        self.home = self.work / "home"
        self.labels = self.work / "home.labels.json"
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
        self.hand = Hand(self)
        self.control = Control(self)
        self.faults = Faults([*self.services, self.asks.name], self.canaries)
        self.servers = {}

    def all_services(self):
        return [*self.services.values(), self.asks, self.hand, self.control]

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
        folder and its label records, the lab clock at the epoch, no faults, the
        default scripts, and a new record whose first entry is this reset."""
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
            home.write_labels(self.labels, self.home)
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
        """Record a request, the party's receipt of it, and the effects of its
        response. Returns what to send: the response, the fault's answer, or
        None to drop the connection. The caller holds the lock."""
        sent = fault.answer(service) if fault else response
        effects = list(response.effects if response else [])
        receipt = service.receipt(request) if isinstance(service, Party) else None
        if receipt and receipt[0] not in [name for name, _ in effects]:
            effects.insert(0, receipt)
        entry = request.entry()
        entry["status"] = sent.status if sent else None
        entry["response"] = {"bytes": len(sent.body), "sha256": sha256(sent.body)} if sent else None
        if fault:
            entry["fault"] = fault.id
        seq = self.record.add(service.name, "request", **entry)
        for name, detail in effects:
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
            "labels": str(self.labels),
            "services": {s.name: {"host": s.host, "kind": s.kind, "title": s.title,
                                  "url": self.url(s.name) if s.name in self.servers else None}
                         for s in self.all_services()},
            "canaries": self.canaries.classes,
            "markers": self.canaries.markers(),
        }
