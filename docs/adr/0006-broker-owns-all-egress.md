# The broker makes every outbound connection and holds every secret

Everything leaving the fleet goes through the capability broker, which makes the outbound connection itself. Tools and harnesses never open their own. Secret values live in a vault the broker holds; existing password managers such as 1Password or Bitwarden can plug in as its backend. API secrets appear inside sandboxes only as placeholders. The broker inserts the real value only for the hosts the secret is tied to, and refuses anything else. Interactive logins use an approved fill: the person approves in trusted system UI, the OS fills and submits the form, and the agent learns only that it succeeded.

The broker makes the connection itself rather than only rewriting headers on the way out. That closes the holes prior art found in header-rewriting proxies: request signing such as AWS SigV4 (the broker signs), servers that echo the secret back (the broker scrubs it), trusting DNS (the broker checks certificates), and WebSocket traffic after the handshake. There is deliberately no mode that puts real secret values in environment variables, because code an agent wrote can read them. Keys that matter (a machine's membership in the fleet, the gate's and the owner's signing keys, each Agent's identity) are bound to the TPM or secure enclave where the hardware has one. See [research/agent-credentials-prior-art.md](../research/agent-credentials-prior-art.md).

**A secret obtained with a secret is a secret.** When an adapter obtains a credential, with a vault secret (an OAuth exchange, a session cookie from a login) or without one (a device flow, a sign-up, a reset link), it stores the credential in the vault and returns a handle. A protocol with no such adapter is listed as unsupported; a Tool does not get to hold the credential as an ordinary value. Added after council rounds 3 and 4.

**Installed software may hold its own login, declared.** Where no adapter exists, or an app works only with its own credential, the person may let that app hold it. The approval screen says so in fixed words ("this app holds its own login to X"), and that compartment reaches only its granted destinations. Code inside it, including code an agent wrote, can read the credential. Tools never get this exception. Added by [ADR 0017](0017-a-pinned-catalog-and-workbenches.md), which also puts adapters for Anthropic, OpenAI, and GitHub first.

## Consequences

- Every protocol that needs a secret needs a broker adapter, which costs more up front than a generic proxy.
- The ledger can record every egress, because there is only one place it can happen.
- A compartment that holds its own login is listed as one, so the person knows where credentials are.
