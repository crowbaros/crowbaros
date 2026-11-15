# Agent credentials: prior art

How AI agents use credentials without the agent or the model seeing the
values. How personal agent harnesses connect to an OS. Researched November
2026 from vendor docs, specs, and first-party blogs.

Markers:

- **[unverified]**: only secondary sources, or I could not open the primary page.
- **[docs conflict]**: the primary sources disagree with each other.

## Takeaways for CrowbarOS

1. **The industry uses our pattern.** A broker outside the sandbox holds
   the vault. The sandbox holds only an opaque placeholder. A proxy swaps
   in the real value at egress. Anthropic Managed Agents, OpenAI's Agents
   API, OpenClaw, Cloudflare Sandboxes, and Infisical Agent Vault all work
   this way. We should align with their vocabulary: placeholder or
   sentinel, egress substitution, allowed hosts.
2. **Bind each secret to destinations.** Anthropic (`allowed_hosts` plus
   `injection_location`) and OpenClaw (exact HTTPS hosts, no wildcards)
   substitute a secret only for listed hosts. OpenClaw fails closed on
   unknown placeholders. Our broker should store a destination binding with
   each credential, and a request with no binding should get no secret.
3. **Egress substitution has known holes.** Request signing (AWS SigV4)
   breaks. A service that echoes request headers returns the plaintext to
   the agent. Hostname rules trust DNS. WebSocket frames after the
   handshake are not rewritten. A token that the agent fetches with a
   brokered secret comes back unredacted. Our ADR should name these.
   Signing protocols need the broker to do the signing.
4. **Browser logins use a separate mechanism.** 1Password for Agents
   (shipped with Grok Bot, Meta Muse, and Town) has the password manager
   fill and submit the form after a human approves. The agent learns only
   that permission was granted. Values are wiped after submit. Approvals
   expire after 30 days. CrowbarOS needs both mechanisms: a network
   injector for APIs and a trusted fill path for web forms.
5. **"Agent-readable env" is a trap, and vendors say so.** OpenAI warns
   that putting a stored secret in the environment exposes it to
   agent-generated code. OpenClaw's `kind: "env"` entries are plaintext to
   the agent. Grok Bot Team Secrets are environment variables. Do not offer
   an env mode that holds the real value.
6. **Policy must live outside the agent.** The Microsoft MXC rule is "an
   agent cannot be its own security authority." OpenAI's OpenClaw
   deployment treats networking, identity, and sandboxing as trusted, and
   the agent and its code as untrusted. This matches our capability
   broker.
7. **Each agent should get its own identity.** Entra Agent ID, Google
   Agent Identity (SPIFFE, 24-hour certs, mTLS-bound tokens), HashiCorp
   Vault agentic IAM, and OpenAI's internal OpenClaw all give each agent
   its own principal, so audits can tell agent from human. For OAuth, MCP
   requires audience-bound tokens (RFC 8707) and forbids token
   passthrough.
8. **Hardware-bound keys are the strongest precedent, but nothing for
   agents is standard yet.** TPM and Secure Enclave keys can be used but
   not exported. Grok Bot lets its cloud browser use a security key on
   the user's desktop, with approval on every use. FIDO started an
   Agentic Authentication working group in April 2026 but has not
   published a spec.

## 1. 1Password

### 1Password for Agents (November 2026)

- 1Password's name for its agent sign-in integration. Launch partners are
  Meta Muse, SpaceXSI Grok Bot, and Town. All three signed shared security
  principles and passed a security review.
  https://1password.com/blog/1password-for-agents-security-privacy
- Flow: the agent reaches a sign-in page and requests a login. The
  1Password app shows the request and a suggested item. The user approves
  or denies. The extension fills and submits the password or one-time
  code. Values are wiped if the submission fails. Source as above.
- **What the agent sees:** that permission was granted. **What it does not
  see:** the password, the one-time code, and the rest of the vault.
  Agents "cannot autonomously interact with your vault." Source as above.
- Keys: the device generates the connection keys and encrypts them with a
  new integration key. 1Password stores the encrypted keys. Only the agent
  developer holds the integration key, so 1Password cannot decrypt them.
  Disconnecting deletes the keys and all approvals. Source as above.
- The Browserbase changelog describes the same model: the agent pauses
  until approval, 1Password signs in, and "Credentials never pass through
  your application, models, or logs." Filled values are removed after
  submission. https://www.browserbase.com/changelog/1password-for-agents
  (October 22, 2026)

### Grok Bot + 1Password specifically

- Requirements: personal or Family 1Password accounts only, 1Password
  8.12.42 or later, on the same device as Grok Bot. Only Login items in
  Personal or Private vaults. Fills usernames, passwords, and TOTP. No
  passkeys, no "Sign in with Google" items, no shared vaults, no cards.
  https://support.1password.com/1password-grok-bot/
- The approval prompt names Grok Bot and suggests an item. Approval for a
  login expires after 30 days. Source as above.
- The first version used a 1Password Service Account and a shared vault.
  That version is no longer supported, and users must reconnect.
  https://docs.x.ai/grok-bot/computer-and-apps
- Grok Bot's post announcing the new integration:
  https://x.com/bot/status/2113332983226831339 (October 2026). The earlier
  September 2026 shared-vault version is described only by secondary
  sources. **[unverified]**

### Secure Agentic Autofill with Browserbase (October 2025)

- 1Password's first agent-autofill product. Early access for Browserbase
  customers.
  https://1password.com/blog/closing-the-credential-risk-gap-for-browser-use-ai-agents
- A new protocol based on the Noise Framework creates an end-to-end
  encrypted channel. It runs between the approving 1Password device and a
  remote copy of the extension in the cloud browser. Source as above.
- "Raw credentials should never enter the LLM context." A human must
  approve each use. 1Password rejected MCP for this case because MCP
  "exposes credentials directly to the underlying model or system."
  Source as above.

### op CLI, service accounts, SDKs

- `op run` resolves `op://` references and starts the command in a
  subprocess with the secrets as plaintext environment variables. Output
  appears to be masked by default (`--no-masking` turns this off). The docs
  warn that other processes of the same user can read that environment.
  https://www.1password.dev/cli/secrets-environment-variables/
- **What the agent sees:** if the agent runs `op read` or `op run`, the
  agent's process gets the value. Masking stdout does not stop the agent
  from printing or sending the value in other ways. This is not
  brokering.
- The OpenClaw "1password" skill is a wrapper for the `op` CLI. It signs in
  inside tmux and reads secrets with `op read`, which puts values in the
  agent's context. https://clawhub.ai/ and secondary skill pages such as
  https://skills.lc/openclaw/skills/openclaw-skills-skills-steipete-1password-skill-md
  **[unverified: primary ClawHub page not opened]**

### 1Password's position on OpenClaw

- January 27, 2026: OpenClaw stores API keys, webhook tokens, transcripts,
  and memory as plaintext in predictable locations. "An infostealer can
  grab the whole thing in seconds." The proposal is a separate identity
  for each agent, plus minimal, time-bound, revocable, attributable
  authority at runtime. https://1password.com/blog/its-openclaw
- Follow-up: the most-downloaded ClawHub skill at the time ("Twitter")
  installed macOS infostealer malware through a fake dependency.
  https://1password.com/blog/from-magic-to-malware-how-openclaws-agent-skills-become-an-attack-surface

## 2. xAI / SpaceXSI Grok Bot

"Grok bot" in dictation almost certainly means **Grok Bot**. This is the
current product name.

### What it is

- Launched in beta on August 11, 2026. It is a set of persistent "AI
  teammates" that you give work through chat. They work across apps and
  sites, "including platforms with no clean API or MCP," and come back
  when they need approval. https://x.ai/news/introducing-grok-bot
- The publisher is now "SpaceX Super Intelligence" (SpaceXSI). docs.x.ai
  calls itself "SpaceXSI (xAI) developer documentation." Secondary sources
  and Wikipedia use "SpaceXAI." https://docs.x.ai/llms.txt
  **[docs conflict on naming]**
- Cursor (Anysphere) builds and runs it. Requests go to "Cursor's servers."
  The computers are "Cursor-operated infrastructure." Usage is billed on
  Cursor accounts. https://docs.x.ai/grok-bot/security
- The deal behind this: SpaceX agreed to buy Anysphere in June 2026, and
  the deal reportedly closed in mid-August 2026. **[unverified: secondary
  sources only, for example https://en.wikipedia.org/wiki/SpaceXAI]**
- Later releases: Grok Bot for Enterprise (September 3), Team Bots,
  Grok Bot 1.0 (November 2), which added Slack, Microsoft Teams, and
  meetings. https://x.ai/news/grok-bot-1-0 ,
  https://x.ai/news/grok-bot-for-enterprise
- Earlier, a "Grok Computer" desktop agent was mentioned in an April 2026
  X post. No docs.x.ai page mentions it.
  https://x.com/mark_k/status/2043650422552821963 **[unverified]**

### Architecture

- Each user gets one persistent, hardware-isolated cloud computer with a
  browser, shell, and filesystem (`/workspace`). All of the user's Bots
  share it. Each Bot has its own screen, but "the screens are separate
  work surfaces, not separate security boundaries."
  https://docs.x.ai/grok-bot/computer-and-apps
- Skills: you do a task once (or let the Bot watch you), then save it as a
  skill. Routines run skills on a schedule or on events (Slack message,
  email). Each Bot has its own memory.
  https://docs.x.ai/grok-bot/how-grok-bot-works
- Tools come from connected apps (Gmail, Drive, 1Password), plugins, and
  custom MCP servers (remote HTTPS or a local command). Source as above.
- "A Bot has no account or credentials of its own." Actions run as the
  signed-in member. Source as above.

### Control of the user's own computer

- Running commands on the user's Mac or Windows machine is a separate
  capability. "Ask every time" is the default. The other options are
  "Always allow" and "Never allow." The docs recommend "Never allow"
  unless needed. Team policy can set a stricter ceiling.
  https://docs.x.ai/grok-bot/approvals-security-and-privacy
- The cloud browser can use a hardware security key plugged into the
  user's desktop. Every use needs approval. This is on by default for macOS
  and Windows and not supported on Linux. Source as above.
- The docs do not describe how local execution works (daemon, protocol).
  **[unverified]**

### How it handles secrets

- Sign-in order: Vault login, then a connected password manager
  (1Password), then a Secure Form that asks the user. "Logins from Vault
  and values from a Secure Form are filled in without the Bot seeing
  them." https://docs.x.ai/grok-bot/vault
- Vault is write-only. Passwords are never shown again, and cards show
  only the last four digits. Source as above.
- API keys go in a "secure secret card." The value is masked, kept out of
  the transcript, and not shown to the model. Source as above.
- OAuth tokens for app connectors stay on Cursor's backend. Bots call
  tools without receiving them. https://docs.x.ai/grok-bot/security
- **Exception:** in app chats, a Bot can see a saved card's full number
  and security code so it can type them at checkout. The security page
  says the model never receives a card number, and the vault page says it
  can. https://docs.x.ai/grok-bot/vault **[docs conflict]**
- Team Bot secrets are environment-variable name/value pairs. "The
  description is visible to the Bot; the value is not." The docs do not
  say whether shell code can read the variable. Source as above.
  **[unverified mechanism]**
- Untrusted content is marked as data. A model-based "Auto Review" checks
  actions. The docs say these controls "reduce, but do not eliminate,
  risk." https://docs.x.ai/grok-bot/security

### API for external agents to drive a machine

- None is documented. docs.x.ai lists deep links and inference hooks, but
  no API or SDK for driving a Bot or its computer from outside.
  https://docs.x.ai/llms.txt

## 3. OpenClaw (formerly Clawdbot, then Moltbot)

### Name history

- Warelay, then CLAWDIS, then Clawdbot (until January 27, 2026), then
  Moltbot (renamed after an Anthropic trademark request), then OpenClaw
  (January 30, 2026). The author is Peter Steinberger, and the project is
  now run by the OpenClaw Foundation. https://en.wikipedia.org/wiki/OpenClaw
  **[secondary; the dates match several blogs]**
- The `clawdbot` CLI still works through shims. The config file is
  `~/.openclaw/openclaw.json`. https://github.com/openclaw/openclaw
- "Clawdbot" is a real former name of OpenClaw. It is not Grok Bot.

### Architecture (short)

- A self-hosted Gateway process owns the configuration, channels (chat
  apps), model providers, and skills (the SKILL.md format, published on
  ClawHub). Default port 18789. https://docs.openclaw.ai/gateway/secrets
  and https://github.com/openclaw/openclaw

### Secrets

- Plaintext works by default. SecretRefs are opt-in for each credential.
  Sources are env, file, exec (the hook for 1Password, Vault, Bitwarden,
  and sops), or a shared store. https://docs.openclaw.ai/gateway/secrets
- Model-provider keys become sentinels (`oc-sent-v2.<...>.end`). A guarded
  fetch swaps in the real value just before the request leaves. Unknown
  sentinel-shaped values block the request. The docs say this is "not
  process isolation."
  https://docs.openclaw.ai/gateway/secrets/runtime-model
- Egress proxy (off by default): subprocesses get one placeholder per
  secret. A loopback proxy run by the Gateway substitutes values in URLs,
  headers, and bodies. Each secret lists exact HTTPS hosts. "A secret with
  no allowed hosts is never substituted." A separate traffic allowlist can
  lock down all egress.
  https://docs.openclaw.ai/gateway/secrets/secret-store-and-egress
- Documented limits: allowed hosts that echo credentials; DNS spoofing;
  WebSocket frames after the handshake; raw sockets that ignore proxy
  variables. Store values are "not encrypted at rest" (0600 SQLite).
  `kind: "env"` entries are plaintext to the agent. Source as above.
- A masked `secrets` tool asks the user for a value, which never enters
  the transcript. Source as above.

### Incidents

- CVE-2026-25253 (CVSS 8.8): the Control UI trusted a `gatewayUrl` query
  parameter and sent the stored gateway token to it, which gave one-click
  RCE. It worked even when the Gateway listened only on loopback. Fixed
  in 2026.1.29. https://advisories.gitlab.com/npm/clawdbot/CVE-2026-25253/
- Tens of thousands of instances were exposed on the internet. Counts range
  from 21,000 to 135,000 depending on the source. **[unverified]**
- Malicious ClawHub skills spread macOS infostealers (see 1Password above).
- OpenAI's internal OpenClaw deployment: the Gateway runs in a separate
  cluster from the harness, a credential broker keeps secrets out of the
  agent's view, GitHub tokens are removed from command environments,
  `~/.ssh` is denied, egress uses an org allowlist through a proxy, and
  each agent has its own ChatGPT Service Account identity.
  https://developers.openai.com/blog/securing-persistent-agents-like-openclaw-at-openai
- Windows MXC lists OpenClaw as a supported agent.
  https://blogs.windows.com/windowsdeveloper/2026/10/07/microsoft-execution-containers-policy-driven-containment-for-ai-agents/

## 4. Other credential brokering patterns

### MCP authorization spec

- The MCP server is an OAuth 2.1 resource server. It MUST publish
  Protected Resource Metadata (RFC 9728). Clients MUST send the `resource`
  parameter (RFC 8707). Servers MUST check the token audience and "MUST
  NOT accept or transit any other tokens." Client ID Metadata Documents
  are preferred, and Dynamic Client Registration is deprecated. STDIO
  servers get credentials from the environment instead.
  https://modelcontextprotocol.io/specification/draft/basic/authorization
- **What the agent sees:** the spec covers the client and server only. It
  does not say whether the model sees tokens. That depends on the host.

### Anthropic: Claude Managed Agents vaults

- You register credentials once and reference them by ID per session.
  Values are write-only. Up to 20 credentials per vault.
  https://platform.claude.com/docs/en/managed-agents/vaults
- MCP credentials (`mcp_oauth` with refresh, `static_bearer`) are keyed by
  MCP server URL and injected on connect. Source as above.
- `environment_variable` credentials sit in the sandbox as an opaque
  placeholder and are substituted at egress. "The agent never sees the
  secret value." `allowed_hosts` scopes the hosts, and
  `injection_location` scopes header or body. A placeholder in a disabled
  location is sent literally. SigV4 and similar signing breaks. Source as
  above.
- Architecture: Claude calls MCP tools through a proxy that fetches
  credentials from the vault, and "the harness is never made aware of any
  credentials." https://www.anthropic.com/engineering/managed-agents
- Warnings: a service that echoes headers can return the secret. A token
  that sandbox code fetches with a stored secret comes back unredacted. Do
  not put a Claude API key in a vault.
  https://platform.claude.com/docs/en/managed-agents/secure-deployment-risks
  **[from search excerpt; page not opened]**

### OpenAI

- Agents API sandboxes: "Sandbox code uses a placeholder; a network proxy
  supplies the real secret for approved hosts." For self-hosted
  sandboxes, run a trusted proxy outside the environment. "Injecting a
  stored secret into the environment still exposes it to agent-generated
  code."
  https://developers.openai.com/api/docs/guides/agents-api/environments/security
- For the internal OpenClaw broker, see section 3.

### Bitwarden Agent Access SDK (alpha, March 2026)

- An open protocol. The agent sends an encrypted request for one
  credential, the user approves it in the CLI, and `run` injects it as
  environment variables into a child process. Each approval releases one
  credential, and the tunnel then closes.
  https://bitwarden.com/blog/introducing-agent-access-sdk/ ,
  https://github.com/bitwarden/agent-access
- **What the agent sees:** with `run`, the child process gets plaintext.
  The OneCLI gateway integration keeps the key from the agent instead.
  Source as above. **[from search excerpt]**

### HashiCorp Vault

- Agentic IAM became GA in Vault Enterprise in September 2026. It adds an
  agent registry, a built-in OAuth resource server, and per-request
  authorization through RAR `authorization_details`. Audit logs record
  both the user and the agent.
  https://www.hashicorp.com/en/blog/hashicorp-vault-agentic-iam-is-now-generally-available
- The Vault MCP server (beta) can return secret values to the model
  through `read_secret`. This is not brokering.
  https://developer.hashicorp.com/vault/tutorials/get-started-for-developers/connect-ai-agent-vault-mcp
  **[from search excerpt]**

### Infisical Agent Vault (preview, April 2026)

- An HTTPS_PROXY forward proxy that terminates TLS (the agent must trust
  its CA), removes any credentials the agent sent, and inserts the real
  one. "Access bundles" are allowlists of hosts and credentials. Sessions
  are time-limited. Agents can only propose new access, and a human
  approves it.
  https://infisical.com/blog/agent-vault-the-open-source-credential-proxy-and-vault-for-agents
  **[from search excerpt]**

### Cloudflare Sandboxes outbound Workers (April 2026)

- Outbound handlers run in Workers, outside the sandbox, and add
  credentials to requests by host. Each sandbox gets its own ephemeral CA
  for TLS interception. Allow and deny lists can change at runtime.
  https://developers.cloudflare.com/changelog/post/2026-04-13-sandbox-outbound-workers-tls-auth/ ,
  https://blog.cloudflare.com/sandbox-auth/ **[from search excerpt]**

### Arcade.dev

- Arcade runs the OAuth flow and stores tokens per user. It injects the
  token into the tool's runtime context when the tool runs. "The client
  and the LLM will never see the token."
  https://docs.arcade.dev/en/home/build-tools/create-a-tool-with-auth
  **[from search excerpt]**

### Composio

- Auth configs and connected accounts per user. Composio adds credentials
  server-side and returns only tool results. Credentials in API responses
  are redacted by default.
  https://docs.composio.dev/docs/authentication **[from search excerpt;
  an older page still shows reading tokens, docs conflict]**

### Aembit

- MCP Identity Gateway: a proxy that checks a short-lived OAuth 2.1 token
  for each request and swaps it for the real downstream credential, which
  the agent never holds. Access decisions combine user identity and agent
  workload identity.
  https://docs.aembit.io/get-started/use-cases/ai-agents/ **[from search
  excerpt]**

### Astrix

- Agent Control Plane: admins define permission profiles in advance.
  Agents get short-lived, narrowly scoped credentials at launch, and the
  credentials expire when the task ends.
  https://astrix.security/learn/blog/meet-ai-agent-control-plane/
  **[vendor marketing; from search excerpt]**

### Google Agent Identity

- Each agent deployed on Agent Runtime gets a SPIFFE ID and an X.509 cert
  that renews every 24 hours. Access tokens are bound to the cert and
  require mTLS. Calls on behalf of a user go through an "auth manager" and
  are attributed to the agent's SPIFFE ID.
  https://docs.cloud.google.com/gemini-enterprise-agent-platform/govern/agent-identity-overview
  **[from search excerpt]**

### Microsoft Entra Agent ID

- An agent identity is a special service principal with no credentials of
  its own. A "blueprint" holds the credentials (managed identity preferred)
  and exchanges tokens for each agent identity. Token claims mark agent
  identities (`xms_act_fct` = 11).
  https://learn.microsoft.com/en-us/entra/agent-id/agent-identities ,
  https://learn.microsoft.com/en-us/entra/agent-id/agent-token-claims
  **[from search excerpt]**
- A sidecar container gets tokens and adds auth headers, so agent code
  can call `/AuthorizationHeader/{service}`.
  https://learn.microsoft.com/en-us/entra/agent-id/identity-platform/microsoft-entra-sdk-for-agent-identities
  **[from search excerpt]**

### Windows: MXC (GA October 7, 2026) and agent workspace

- Microsoft Execution Containers: policy outside the agent, "An agent
  cannot be its own security authority." Isolation levels are process
  (AppContainer, Seatbelt, Bubblewrap), session (a separate Windows
  account and desktop), WSL, and microVM (experimental). Each container
  gets its own local agent identity. Entra separation of agent and user
  activity is "coming soon."
  https://blogs.windows.com/windowsdeveloper/2026/10/07/microsoft-execution-containers-policy-driven-containment-for-ai-agents/
- MXC has no credential vault of its own. Credential management comes from
  the NVIDIA OpenShell integration. Source as above.
- The separate "agent workspace" preview (Copilot Actions) runs agents
  under their own account, with per-agent "Allow always / Ask every time
  / Never" access to known folders. **[unverified: secondary sources only]**

## 5. OS-level precedent

- **TPM:** keys created by the Platform Crypto Provider can be used inside
  the TPM without copying them to system memory. Non-migratable keys never
  leave the TPM. Key attestation proves to a CA that a key cannot be
  exported. https://learn.microsoft.com/en-us/windows/security/hardware-security/tpm/how-windows-uses-the-tpm ,
  https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/component-updates/tpm-key-attestation
- **Apple Secure Enclave:** keys can be used but not exported. The SE
  returns only a wrapped blob that the same SE can unwrap.
  https://developer.apple.com/documentation/security/certificate_key_and_trust_services/keys/storing_keys_in_the_secure_enclave
- **Passkeys:** Apple passkeys are not bound to one device. They sync
  end-to-end encrypted through iCloud Keychain. Every use is still a
  signature that is bound to an origin and cannot be phished or replayed.
  https://support.apple.com/en-us/102195 ,
  https://support.apple.com/guide/security/sec1c89c6f3b/web
- **Android Credential Manager:** a single API for passkeys, passwords,
  federated sign-in, and digital credentials. No API for agents is
  documented. Gemini can fill form data through Autofill with Google
  (opt-in). https://developer.android.com/identity/credential-manager ,
  https://blog.google/security/android-gemini-intelligence-security-privacy/
- **FIDO for agents:** the Agentic Authentication Technical Working Group
  was formed April 28, 2026. It covers verifiable user instructions,
  agent authentication, and trusted delegation for commerce. Google AP2
  and Mastercard Verifiable Intent were contributed. No spec has been
  published.
  https://fidoalliance.org/fido-alliance-to-develop-standards-for-trusted-ai-agent-interactions/
- **Lesson for CrowbarOS:** a passkey or TPM key gives the agent use of a
  credential without possession, which is what our broker wants. Today
  no OS gives an agent its own passkey or delegated authenticator. The
  nearest product is Grok Bot forwarding a security key with approval on
  each use.
