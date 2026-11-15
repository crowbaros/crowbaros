"""Errors armed through the control service.

A fault fires on the nth request after arming that matches its service,
method, and path, and on the count - 1 matching requests after that.

    status               answer with the status and message; no effect
    drop                 close the connection with no answer; no effect
    status-after-effect  the effect happens, then the answer is replaced
    drop-after-effect    the effect happens, then the connection is closed

A status fault answers in the service's own error shape, with the message
inside it, so a client parses it as it would a real error.
"""
from fixtures import check_fields, require, whole

# kind -> (answers with a status, happens after the effect)
KINDS = {
    "status": (True, False),
    "drop": (False, False),
    "status-after-effect": (True, True),
    "drop-after-effect": (False, True),
}


class Fault:
    def __init__(self, fault_id, spec, services, canaries):
        check_fields(spec, ("service", "method", "path", "kind"),
                     ("nth", "count", "status", "body"), "a fault")
        require(spec["service"] in services, 400, f"service must be one of {sorted(services)}")
        require(spec["kind"] in KINDS, 400, f"kind must be one of {list(KINDS)}")
        require(isinstance(spec["method"], str) and isinstance(spec["path"], str), 400,
                "method and path must be strings")
        self.id = fault_id
        self.service = spec["service"]
        self.method = spec["method"].upper()
        self.path = spec["path"]
        self.kind = spec["kind"]
        self.answers, self.after_effect = KINDS[self.kind]
        self.nth = whole(spec.get("nth", 1), 1, "nth")
        self.count = whole(spec.get("count", 1), 1, "count")
        self.status = self.body = None
        if self.answers:
            self.status = spec.get("status")
            require(isinstance(self.status, int) and 400 <= self.status <= 599, 400,
                    "a status fault needs a status from 400 to 599")
            self.body = canaries.expand(spec.get("body", ""))
            require(isinstance(self.body, str), 400, "body must be a string")
        self.seen = 0
        self.fired = 0

    def public(self):
        view = {"id": self.id, "service": self.service, "method": self.method,
                "path": self.path, "kind": self.kind, "nth": self.nth, "count": self.count}
        if self.answers:
            view |= {"status": self.status, "body": self.body}
        return view

    def matches(self, service, method, path):
        return (self.service, self.method, self.path) == (service, method, path)

    def answer(self, service):
        """What the client gets: the service's error with this fault's status
        and message, or None to drop the connection."""
        return service.error(self.status, self.body) if self.answers else None


class Faults:
    def __init__(self, services, canaries):
        self.services = set(services)
        self.canaries = canaries
        self.clear()

    def clear(self):
        self.armed = []
        self.counter = 0

    def arm(self, spec):
        self.counter += 1
        fault = Fault(f"fault-{self.counter:04d}", spec, self.services, self.canaries)
        self.armed.append(fault)
        return fault

    def listing(self):
        return [fault.public() for fault in self.armed]

    def take(self, service, method, path):
        """The fault that fires on this request, or None."""
        for fault in self.armed:
            if not fault.matches(service, method, path):
                continue
            fault.seen += 1
            if fault.seen < fault.nth:
                continue
            fault.fired += 1
            if fault.fired >= fault.count:
                self.armed.remove(fault)
            return fault
        return None
