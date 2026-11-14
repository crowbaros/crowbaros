# The broker makes every outbound connection and holds every secret

Everything leaving the fleet goes through the capability broker, which makes the outbound connection itself. Tools and harnesses never open their own. Secret values live in a vault the broker holds; existing password managers such as 1Password or Bitwarden can plug in as its backend. API secrets appear inside sandboxes only as placeholders. The broker inserts the real value only for the hosts the secret is tied to, and refuses anything else. Interactive logins use an approved fill: the person approves in trusted system UI, the OS fills and submits the form, and the agent learns only that it succeeded.

The broker makes the connection itself rather than only rewriting headers on the way out. That closes the holes prior art found in header-rewriting proxies: request signing such as AWS SigV4 (the broker signs), servers that echo the secret back (the broker scrubs it), trusting DNS (the broker checks certificates), and WebSocket traffic after the handshake. There is deliberately no mode that puts real secret values in environment variables, because code an agent wrote can read them. Keys that matter (a machine's membership in the fleet, the approver's and the owner's signing keys, each Agent's identity) are bound to the TPM or secure enclave where the hardware has one. See [research/agent-credentials-prior-art.md](../agent-first/research/agent-credentials-prior-art.md).

## Consequences

- Every protocol that needs a secret needs a broker adapter, which costs more up front than a generic proxy.
- The ledger can record every egress, because there is only one place it can happen.
