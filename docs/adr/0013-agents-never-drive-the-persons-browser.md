# Agents never drive the person's browser profile

The person's browser profile holds their sessions, cookies, and history. It reads every page and can connect anywhere, which is exactly the combination the design forbids a principal an injection can steer. So only the person drives it. No agent, task, or Tool can drive it, read it, or receive its cookies.

A task that needs the web gets a **task browser context**: a fresh browser context for one task and the sites named in that task, with no sessions. A login reaches it by approved fill (ADR 0006): the person approves in trusted UI, and the OS fills the form. The session cookie stays in the broker, which attaches it to that context's requests. The task cannot read it, and it ends with the task. A page fetched under a login is private to that site. The context can connect only to its named sites, and each site is an outside party with a clearance (ADR 0008): it receives public data, what it supplied itself, and what the person released to it. The task drives its own context through the browser's automation interface inside its sandbox. That is not input to a surface the shell draws for the person (ADR 0014).

Reading a page is open untrusted input, so the task is marked (ADR 0011).

**Allowing a site does not approve acts on the person's account there.** In a logged-in context, every request that can change state (anything but a GET or a HEAD) waits for the person's answer, shown with its exact content. An act happens without asking only through a broker adapter that knows the operation, under a standing rule for that operation ("pay this bill, up to this amount"). This rule is deterministic: nothing has to judge what a request means.

## Considered Options

- **Let the Agent drive the person's own browser.** Rejected: one injected page would reach every site the person is logged in to.
- **Give tasks the person's cookies for one site.** Rejected: a cookie is a secret, and ADR 0006 keeps secrets out of every agent's reach. Approved fill gives the same result without the task holding it.

## Consequences

- The person's browser is installed software in a sandbox with broker-owned egress. Inside it, keeping one site's content from another is the browser's job, and we rely on it.
- Agent work on a site starts logged out and costs an approved fill, or a standing grant for that site's login.
- A badly built site can change state on a GET. That remains.
- Real pages load from many other domains and send requests in the background. Whether a logged-in task context is livable under this rule is a question for a build, not for more writing.
- Every kind of unasked act on a site needs an adapter.
- Which browser engine, and how a task browser context is built from it, is still open.
