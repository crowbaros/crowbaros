# Agents never drive the person's browser profile

The person's browser profile holds their sessions, cookies, and history. It reads every page and can connect anywhere, which is exactly the combination the design forbids a principal an injection can steer. So only the person drives it. No agent, task, or Tool can drive it, read it, or receive its cookies.

A task that needs the web gets a **task browser context**: a fresh browser context for one task, with no sessions. A login reaches it by approved fill (ADR 0006): the person approves in trusted UI, and the OS fills the form. The session cookie stays in the broker, which attaches it to that context's requests. The task cannot read it, and it ends with the task. A page fetched under a login is private to that site. Each site is an outside party with a clearance (ADR 0008): it receives public data, what it supplied itself, and what the person released to it. A context that is not logged in holds only public data, so it may reach any host (ADR 0011). A logged-in context reaches its one site, and the shared hosts below. The task drives its own context through the browser's automation interface inside its sandbox. That is not input to a surface the shell draws for the person (ADR 0014).

Reading a page is open untrusted input, so the task is marked (ADR 0011).

**On a logged-in site, the person answers once for each task and each site.** One ask at the start names the site, shows the task's objective and limits in fixed words, and shows the exact values the task may send there, such as a name and a phone number. It includes the approved fill, and the confirm gesture answers it ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)). The answer covers the task's requests to that one site until the task ends. A second site takes a second ask. This rule is deterministic: nothing has to judge what a request means.

What that answer does not change:

- The task reaches that one site, and the shared hosts below. A connection to any other host is dropped.
- The session cookie stays in the broker.
- The task holds only what the person released to it in that ask.
- The Allowance is the only payment credential the task can supply, and the card issuer or the wallet caps it. A payment method the site already stores is not the task's to supply, and is not capped: see what this gives up, below.
- The context runs in a framed window the person can watch.
- Every request is in the ledger.

**Two lists come with the catalog** ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)). On the person's machine, a change to either takes a signature, whether the person makes it or a new catalog snapshot brings it. A snapshot's changes are shown together and take one signature.

- **Shared hosts:** the payment processors, content delivery networks, and font hosts a page may load from, each named by its exact host. A logged-in context reaches these as well as its site. For the length of that task, a shared host is cleared for what the task holds for its site: the site's own pages, and what the person released to the site in the ask. The person's answer for the site is the authority for that, and the ask says so. A shared host is cleared for nothing else the fleet holds. A request to any other host, telemetry included, is dropped quietly, and does not count as a refusal ([ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md)).
- **Adapter-only sites:** banks, mail providers, and identity providers, and any site the person adds. A task gets no browser context for one of these. It acts there only through an adapter, and where there is no adapter, the Agent hands the errand back to the person.

**A site that breaks without a host that is not on the shared list is handed back to the person.**

**An adapter is still the only way an act on a site happens with no ask at all:** a broker adapter that knows the operation, under a standing rule for that operation ("pay this bill, up to this amount").

Revised after council round 5. Until then, every request in a logged-in context that could change state (anything but a GET or a HEAD) waited for the person's answer. Research for that round found that an ordinary logged-in flow of a few minutes makes roughly 20 to 60 such requests, of which 1 to 3 are acts the person meant. The rest are telemetry, fraud signals, and reads sent by POST. The number is an inference from one shop's load test and vendors' telemetry documents, and nobody publishes a measurement. The direction is not in doubt: "check my balance" would have asked many times.

## Considered Options

- **Let the Agent drive the person's own browser.** Rejected: one injected page would reach every site the person is logged in to.
- **Give tasks the person's cookies for one site.** Rejected: a cookie is a secret, and ADR 0006 keeps secrets out of every agent's reach. Approved fill gives the same result without the task holding it.
- **Every request that can change state asks (the earlier rule).** Rejected: see above. No product or paper the research found asks per request.
- **A model judges which requests are consequential, as shipping browser agents do.** Rejected: it puts a model on the trust path.
- **Adapters only: every site without one is handed back.** Rejected as the rule for every site: most sites have no adapter, so the Agent would run few web errands for years. It is kept for the sites where one account matters most.

## Consequences

- The person's browser is installed software in a sandbox with broker-owned egress. Inside it, keeping one site's content from another is the browser's job, and we rely on it.
- Agent work on a site starts logged out and costs one ask, which includes the approved fill, or a standing grant for that site's login.
- **While the task runs, an injected page on that site can make other changes on that one account:** a second booking, a changed address. The risk is confined to one account on one site for the length of one task, and the site already controls that account.
- **That includes spending with a payment method the site already stores:** a saved card, or billing to the account. Nothing deterministic can tell that a request is a charge, so the Allowance does not cap it. The ask for the site says so in fixed words. A site where that matters belongs on the adapter-only list, which the person can add to.
- **What a task holds for one site can reach a host on the shared list.** Some shared hosts serve anyone's content, as many content delivery networks do, so that can be an attacker's content on a shared host.
- A site that needs a host the shared list leaves out does not work for a task until the list gains it.
- Counting requests on real sites, with real accounts, would tune the shared list. It is not done yet and blocks nothing before Milestone 5.
- Every kind of unasked act on a site needs an adapter.
- Which browser engine, and how a task browser context is built from it, is still open.
