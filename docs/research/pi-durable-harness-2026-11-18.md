# Pi Durable and Harness Alternatives for CrowbarOS (Milestone 3 Research)

**Date:** November 18, 2026  
**Ticket:** [crowbaros/crowbaros#231](https://github.com/crowbaros/crowbaros/issues/231) (blocks [#236](https://github.com/crowbaros/crowbaros/issues/236): *Decide the Harness and IPC boundary (ADR)*)  
**Primary sources inspected at:**
- `earendil-works/pi` `main` @ [`5a14e69c`](https://github.com/earendil-works/pi/commit/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9) (latest release [`v1.5.2`](https://github.com/earendil-works/pi/releases/tag/v1.5.2), published 2026-11-10)
- `void-linux/void-packages` `master` @ [`19e0dff9`](https://github.com/void-linux/void-packages/commit/19e0dff999f34211baf3f4068595cdf1a1597dae) and live `https://repo-default.voidlinux.org/current/x86_64-repodata`
- Reference thesis tested: [`docs/research/pi-durable-fit-brief.md`](https://github.com/crowbaros/crowbaros/blob/research/pi-durable-fit-brief/docs/research/pi-durable-fit-brief.md)

**Read alongside:** [`GLOSSARY.md`](../../GLOSSARY.md), [`docs/VISION.md`](../VISION.md), [`docs/LESSONS.md`](../LESSONS.md), [`base/config.json`](../../base/config.json).

---

## Executive Summary

**Primary-source verification largely confirms the [fit brief](https://github.com/crowbaros/crowbaros/blob/research/pi-durable-fit-brief/docs/research/pi-durable-fit-brief.md), sharpens four operational caveats, and uncovers two unexpectedly strong assets already in `earendil-works/pi`.**

1. **Runtime & Base footprint are manageable on Void Linux.** All published Pi packages (`@earendil-works/pi-durable`, `pi-ai`, `chord`, `pi-env` v1.5.2) require `node >= 22.19.0` and use Node's built-in `node:sqlite`. Void `current` ships `nodejs-24.22.1_1` (and does **not** package Bun). Because `sqlite` and `libicu78` are already in CrowbarOS's `base/config.json` dependency closure, adding `nodejs` adds **11 packages, 20.49 MiB download, and 73.76 MiB installed**—or **~62.1 MiB installed** if Base strips Void's bundled `npm` (11.64 MiB) to uphold "no `npm` on the running OS." Single-file `esbuild` bundling works, provided three bundler-opaque runtime assets (OAuth loaders, Bedrock module if enabled, and Photon's `.wasm` binary) are wired explicitly.
2. **Model access covers cloud API keys, 9 OAuth flows, local Models, tool calling, and vision.** `@earendil-works/pi-ai` ships 30+ API-key providers, 9 built-in OAuth flows (browser loopback redirect + PKCE with manual paste fallback, and RFC 8628 device-code flows) with double-checked-lock token refresh, first-class OpenAI-compatible local-Model support (Ollama, llama.cpp, vLLM via `createProvider()` + `openAICompletionsApi()` + `OpenAICompletionsCompat` + `fetchModels`), and native `ImageContent` blocks paired with `pi-durable`'s worker-thread Photon image resizer (`fitImage` / `createFitImagesExtension`).
3. **Process isolation and hand-verified confirmation are even cleaner than the brief assumed.**
   - `pi-durable` already implements **durable suspendable approval gates** (`beforeTool` returning `{ sleep: { signals, until } }` + `defineSignal` + `api.memo` + `harness.signal()`), so a pending confirmation survives crashes and sleep with zero held threads.
   - `earendil-works/pi` already ships **`@earendil-works/pi-env`**: a small MIT-licensed **Rust** execution daemon (`packages/env/daemon/`) speaking a framed protocol over stdin/stdout that implements Pi Durable's `ExecutionEnv` (`RemoteExecutionEnv`). CrowbarOS can run the Harness daemon under its own `runit` service account while delegating file/shell work to a helper running as the person's account.
   - **Two critical security footguns documented in `spec.md` §12:** `harness.signal()` does not authenticate callers, and `pi-durable` has no registry-wide hook (a conversation/subagent with a custom extension array can omit a guard extension). CrowbarOS's Harness wrapper must enforce both deterministically.
4. **Durability, Btrfs rollback, and IPC require explicit OS-level handling.**
   - SQLite schema migrations are strictly contiguous and **roll-forward-only** (`applySqliteMigrations` throws if DB version > binary version; v2 already rebuilt the `tasks` table). Base Btrfs rollbacks need a pre-migration DB snapshot/reflink strategy so an older Harness never bricks on boot.
   - `openNodeSqliteDatabase` defaults to `WAL` + `synchronous = NORMAL` (crash-safe, not power-loss-proof for uncheckpointed commits), and `pi-durable` defers streaming progress commits by 1,000 ms (`persistWithinMs`).
   - Neither `chord/http` nor `chord/link/ipc` speaks Unix domain sockets. Wrapping `Harness` in a narrow CrowbarOS-owned Node `node:net` Unix socket server with **separate kernel-permissioned sockets per role** (Fulcrum vs. Shell) gives clean privilege separation without native `SO_PEERCRED` addons and insulates Fulcrum and the Shell from Pi Durable's rapid breaking-change cadence (**6 breaking-change releases in 6 weeks**, plus more in `[Unreleased]`).

---

## 1. Runtime, Void Base Footprint, and Bundleability

### 1.1 Pi Durable & Chord runtime requirements, license, and size

Verified directly from `earendil-works/pi` manifests ([root `package.json`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/package.json), [`packages/durable/package.json`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/package.json), [`packages/ai/package.json`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/package.json), [`packages/chord/package.json`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/chord/package.json), [`packages/env/package.json`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/env/package.json)):

| Property | Verified primary-source fact |
| --- | --- |
| License | **MIT** across all packages and `pi-env` Rust daemon ([`LICENSE`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/LICENSE)) |
| Latest release | `v1.5.2` (2026-11-10); monorepo packages versioned in lockstep (`1.5.2`) |
| `engines.node` | `>=22.19.0` for `pi-durable`, `pi-ai`, `chord`, `pi-env`, `pi-codemode` |
| SQLite binding | Node built-in `node:sqlite` (`DatabaseSync`) via [`packages/chord/src/sqlite/node.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/chord/src/sqlite/node.ts) — **zero native C++ npm addons** |
| Bun support | Portable cores (`/storage/sqlite`, `/storage/jsonl`) accept custom facades and work on Bun ([`packages/durable/README.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/README.md)), and `pi-ai` ships [`bun-oauth.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/bun-oauth.ts) for standalone Bun binaries. However, `openNodeSqliteStorage` uses `node:sqlite`, and **Void Linux does not package Bun** |
| Codebase size (`packages/durable/src/`) | **103 TypeScript files, 1,456,033 bytes** (~35,000–40,000+ lines), not "~15,000 lines" as stated in the fit brief |
| Direct runtime dependencies of `pi-durable` | `@earendil-works/chord`, `@earendil-works/pi-ai`, `@earendil-works/pi-codemode` (`quickjs-wasi`), `@earendil-works/pi-schema`, `@earendil-works/pi-telemetry`, `@silvia-odwyer/photon` (`0.3.3`, WASM), `diff` (`8.0.4`) |

**Upstream Node-version trajectory to watch:** On 2026-11-18, `earendil-works/pi`'s internal app `apps/p3` recorded two architecture decisions ([`68791345`](https://github.com/earendil-works/pi/commit/68791345) and [`c870614f`](https://github.com/earendil-works/pi/commit/c870614f)): *"p3 ships with its own Node"* and *"Node 26 or newer everywhere"* (specifically requiring Node 26.3.1+ for V8 zone-memory controls and Node 26's network-blocking permission model). While `packages/durable` itself still declares `node >= 22.19.0`, upstream's own flagship consumer has already moved past Node 24 LTS.

### 1.2 Void Linux `xbps` availability and exact Base footprint

Verified against [`void-linux/void-packages/srcpkgs/nodejs/template`](https://github.com/void-linux/void-packages/blob/19e0dff999f34211baf3f4068595cdf1a1597dae/srcpkgs/nodejs/template), live `https://repo-default.voidlinux.org/current/x86_64-repodata`, and [`nodejs-24.22.1_1.x86_64.xbps`](https://repo-default.voidlinux.org/current/nodejs-24.22.1_1.x86_64.xbps):

- **Node.js in Void `current`:** `nodejs-24.22.1_1` (`nodejs-lts-24.22.1_1` is an empty metapackage depending on `nodejs>=24.22.1_1`). Built with `--shared-sqlite`, `--shared-openssl`, `--shared-libuv`, `--with-intl=system-icu`, `--shared-zlib`, `--shared-zstd`, `--shared-ada`, `--shared-http-parser`, `--shared-nghttp2`, `--shared-nghttp3`, `--shared-ngtcp2`, `--shared-cares`, `--shared-brotli`. Satisfies `>=22.19.0`.
- **Bun in Void `current`:** **Not packaged** (`srcpkgs/bun` returns 404 in `void-packages`; `bun` is absent from `x86_64-repodata`).
- **Exact dependency delta against CrowbarOS [`base/config.json`](../../base/config.json):**
  - Current Base (`base-minimal`, `linux`, `btrfs-progs`, `grub-x86_64-efi`, `mtools`, `openssh`, `dhcpcd`, `iproute2`): **114 packages, 1,107.1 MiB installed** (already includes `sqlite-3.54.1_1` at 3.58 MiB and `libicu78` at 37.74 MiB).
  - Adding `nodejs` adds **11 packages, 20.49 MiB download, 73.76 MiB installed**:

| Package | Download | Installed |
| --- | ---: | ---: |
| `nodejs-24.22.1_1` | 18.38 MiB | 67.73 MiB |
| `nghttp2-1.70.0_1` | 0.96 MiB | 3.31 MiB |
| `brotli-1.2.1_1` | 0.37 MiB | 0.89 MiB |
| `ada-3.4.4_1` | 0.22 MiB | 0.60 MiB |
| `ngtcp2-1.26.0_1` | 0.17 MiB | 0.39 MiB |
| `c-ares-1.34.9_1` | 0.11 MiB | 0.23 MiB |
| `libuv-1.52.1_1` | 0.10 MiB | 0.22 MiB |
| `nghttp3-1.18.0_1` | 0.07 MiB | 0.15 MiB |
| `jansson-2.15.0_1` | 0.06 MiB | 0.11 MiB |
| `libev-4.33_1` | 0.03 MiB | 0.07 MiB |
| `llhttp-9.4.1_1` | 0.02 MiB | 0.07 MiB |
| **Total delta** | **20.49 MiB** | **73.76 MiB** |

- **Important `npm` finding:** Void's `srcpkgs/nodejs/template` does **not** pass `--without-npm`. Inspecting `nodejs-24.22.1_1.x86_64.xbps` directly shows:
  - `/usr/bin/node`: **55.30 MiB** (57,986,552 bytes)
  - `/usr/lib/node_modules/npm/` (plus `/usr/bin/npm`, `/usr/bin/npx` symlinks): **11.64 MiB**
  - To ship "no `npm` on the running OS," Base must either strip `/usr/lib/node_modules/npm` and `/usr/bin/{npm,npx}` during rootfs construction or carry a local Void template built with `--without-npm` (reducing the installed delta to **~62.1 MiB**).
- **RAM footprint note:** Void repodata does not measure live RSS; an idle Node 24 V8 process with `node:sqlite` + HTTP/TLS stacks typically consumes ~40–80 MiB RSS (vs. ~5–15 MiB for a minimal Rust daemon). Photon image resizing runs in a dedicated worker thread that self-terminates after 30 s idle (`idleMs: 30_000`) specifically to release non-shrinking WebAssembly linear memory ([`packages/durable/src/images/node.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/src/images/node.ts)). Actual idle/active RSS on Base should be measured in the VM lab during Milestone 3 spike work.

### 1.3 Single-artifact bundling (no `npm` on the running OS)

Verified in [`packages/ai/README.md` §Bundling and Tree Shaking](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/README.md#bundling-and-tree-shaking), [`packages/ai/src/auth/oauth/load.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/auth/oauth/load.ts), [`packages/ai/src/bun-oauth.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/bun-oauth.ts), and [`packages/durable/src/images/node.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/src/images/node.ts):

A pinned, self-contained Node ESM bundle built at CI/image-build time (`esbuild --bundle --platform=node --format=esm`) is supported, with **four explicit wiring requirements**:

1. **CommonJS `require` banner shim:** Some provider SDK dependencies still emit dynamic `require()` calls; `pi-ai`'s README documents adding `--banner:js='import { createRequire } from "module";const require = createRequire(import.meta.url);'`.
2. **Static OAuth flow registration:** `packages/ai/src/auth/oauth/load.ts` uses bundler-opaque dynamic imports so browser bundles do not pull `node:http`/`node:crypto`. A single-file bundle **must** statically register OAuth loaders via `registerBundledOAuthFlowLoaders(...)` (matching [`packages/ai/src/bun-oauth.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/bun-oauth.ts)).
3. **Explicit Bedrock registration (if Amazon Bedrock is enabled):** Bedrock's AWS SDK loads via a bundler-opaque Node-only import; single-file bundles must call `setBedrockProviderModule(bedrockProviderModule)`.
4. **Sidecar WASM for image resizing:** `createNodePhotonImages()` resolves `@silvia-odwyer/photon/photon_rs_bg.wasm` via `createRequire().resolve(...)` and spawns `./node-worker.js`. Its own JSDoc explicitly warns: *"A single-file binary cannot resolve the package: read its own copy of `photon_rs_bg.wasm` and call `createPhotonImages()`."* Thus CrowbarOS ships one pinned JS bundle + `photon_rs_bg.wasm` (and an optional tiny worker script if off-thread resizing is kept) in `/usr/lib/crowbar-harness/` — still zero `node_modules` and zero `npm` at runtime. (`@earendil-works/pi-codemode` / `quickjs-wasi` is only imported if code mode is enabled and tree-shakes away otherwise.)

---

## 2. Model Access in `@earendil-works/pi-ai` and `@earendil-works/pi-durable`

### 2.1 Cloud API keys and `CredentialStore`

Verified in [`packages/ai/README.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/README.md#auth) and [`packages/ai/src/auth/resolve.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/auth/resolve.ts):

- **30+ built-in cloud providers** across 9 API implementations (`anthropic-messages`, `openai-completions`, `openai-responses`, `openai-codex-responses`, `azure-openai-responses`, `google-generative-ai`, `google-vertex`, `mistral-conversations`, `bedrock-converse-stream`), including Anthropic, OpenAI, Google Gemini, Vertex AI, Amazon Bedrock, Azure OpenAI, Mistral, Groq, Cerebras, DeepSeek, xAI, OpenRouter, Together, Fireworks, Hugging Face, NVIDIA NIM, Moonshot/Kimi, MiniMax, Cloudflare AI Gateway/Workers AI, and Vercel AI Gateway.
- **Auth resolution precedence:** Explicit per-call `apiKey` override → stored credential in `CredentialStore` → ambient environment variables (or AWS profile / gcloud ADC). A stored credential *owns* its provider: if an OAuth refresh fails, `resolveProviderAuth` **never** silently falls back to an environment variable.
- **Persistent storage is the host's job:** `pi-ai` ships an in-memory `CredentialStore` by default. CrowbarOS injects a small file-backed (or SQLite-backed) `CredentialStore` implementing 4 methods (`read`, `list`, `modify`, `delete`), owned exclusively by the Harness service account so secrets never enter the person's home directory or the Model context.

### 2.2 OAuth login flows (exact provider matrix)

Verified directly from every file in [`packages/ai/src/auth/oauth/`](https://github.com/earendil-works/pi/tree/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/auth/oauth) and [`packages/ai/src/auth/resolve.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/src/auth/resolve.ts):

| Provider (`oauth/` file) | Flow type(s) | Loopback / Endpoint details | Token refresh & storage |
| --- | --- | --- | --- |
| **Anthropic** (`anthropic.ts`) | Browser redirect + PKCE, or copy-code fallback | `127.0.0.1:53692/callback` (falls back to ephemeral port if busy); races callback vs. pasted redirect URL/code | `refresh_token` → automatic refresh under lock |
| **OpenAI (Sign in with ChatGPT)** (`openai-chatgpt.ts`) | Browser redirect + PKCE + dynamic client registration | `127.0.0.1` `/auth/callback` (`node:http`); requires a persistent installation UUID (`deviceId`) | `refresh_token` → automatic refresh under lock |
| **OpenAI Codex** (`openai-codex.ts`) | **Selectable:** Browser redirect + PKCE **or** headless device code | Browser: `localhost:1455/auth/callback` + paste fallback; Device: `codex/device` polling | `refresh_token` → automatic refresh under lock |
| **OpenRouter** (`openrouter.ts`) | Browser redirect + PKCE | `127.0.0.1` `/oauth/callback/<uuid>` + paste fallback | Mints a permanent user-controlled API key (`expires: MAX_SAFE_INTEGER`, refresh is a no-op) |
| **GitHub Copilot** (`github-copilot.ts`) | RFC 8628 device code | `https://<domain>/login/device/code` | Refresh token exchanges for short-lived Copilot access token + per-account base URL |
| **xAI** (`xai.ts`) | RFC 8628 device code | `https://auth.x.ai/oauth2/device/code` | `refresh_token` (handles non-rotating refresh tokens) |
| **Kimi Coding** (`kimi-coding.ts`) | RFC 8628 device code | `https://auth.kimi.com/api/oauth/device_authorization` | `refresh_token` → automatic refresh under lock |
| **Meta** (`meta.ts`) | RFC 8628 device code | `https://auth.meta.com/oidc/device/authorization/` | Stores non-renewable identity token as `refresh` to re-mint short-lived API keys; 401/403 requires re-login |
| **Radius** (`radius.ts`) | **Selectable:** Browser redirect + PKCE **or** RFC 8628 device code | `127.0.0.1` `/oauth/callback` or `/v1/oauth/device` | `refresh_token` → automatic refresh under lock |

*(Google Vertex AI uses API key, `gcloud` ADC, or service-account JSON rather than a consumer OAuth flow; Amazon Bedrock uses AWS credential chains or bearer tokens.)*

**Refresh safety and CrowbarOS fit:**
- `resolveStoredOAuth` refreshes any token with `< 5 minutes` remaining via `credentials.modify()` with double-checked locking and a 15 s timeout. Once refresh starts, it **ignores caller abort** so a rotated single-use refresh token is never discarded mid-flight (`resolve.ts` L117–156).
- **Decoupled UI interaction:** Every OAuth flow talks through the provider-neutral `AuthInteraction` (`prompt()` for `text`/`secret`/`select`/`manual_code`, and `notify()` for `auth_url`/`device_code`/`info`/`progress`). On CrowbarOS, the Harness daemon can run the OAuth flow under its own service account, bind the loopback callback listener on `127.0.0.1`, forward `auth_url` / `device_code` to the Shell or Studio Settings over CrowbarOS's local IPC so the person completes sign-in in their own browser on the person's account, receive the loopback redirect directly in the Harness, and persist tokens strictly inside the Harness's `CredentialStore`.

### 2.3 Local Models (Ollama, llama.cpp, OpenAI-compatible) and tool calling

Verified in [`packages/ai/README.md` §Custom Providers & §OpenAI Compatibility Settings](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/README.md#custom-providers) and [`packages/coding-agent/docs/llama-cpp.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/coding-agent/docs/llama-cpp.md):

- **Not pre-registered in `builtinModels()`:** Neither Ollama nor llama.cpp has a built-in file under `packages/ai/src/providers/`. Instead, `pi-ai` treats local OpenAI-compatible servers as a first-class pattern via `createProvider()` + `openAICompletionsApi()` + dynamic `fetchModels` + persisted `ModelsStore`.
- **Compatibility matrix built in:** `OpenAICompletionsCompat` exposes granular flags specifically tuned for Ollama, llama.cpp, vLLM, and SGLang — including `supportsDeveloperRole: false`, `supportsReasoningEffort: false`, `maxTokensField`, `thinkingFormat` (`chat-template`, `qwen-chat-template`, `openai`, etc.), `thinkingTokenBudgetField` (for vLLM/Qwen/llama.cpp), `requiresToolResultName`, `requiresNonNullAssistantContent`, and `requiresAssistantAfterToolResult`.
- **Tool calling:** Built into the core contract (`pi-ai`'s README notes the chat catalog only includes tool-calling models). Tool schemas accept plain JSON Schema or `@earendil-works/pi-schema` definitions; partial tool-call JSON streams via `partial-json` and is validated before execution with `validateToolArguments()`.

### 2.4 Image / vision content blocks (compositor screen capture)

Verified in [`packages/ai/README.md` §Image Input](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/ai/README.md#image-input), [`packages/durable/README.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/README.md), and [`packages/durable/test/examples/47-fit-images.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/test/examples/47-fit-images.ts):

- **Full vision support in both user inputs and tool results:** `ImageContent` (`{ type: "image", data: base64, mimeType }`) is supported in `UserMessage` (e.g., screenshot attached on Summon) and `ToolResultMessage` (e.g., an explicit `capture_screen` tool).
- **Cache-safe pre-storage resizing:** `pi-durable` provides `fitImage()` (for attached inputs) and `createFitImagesExtension(images)` (for tool-returned screenshots), powered by `@silvia-odwyer/photon` in an auto-exiting worker thread. Fitting images to the Model's `inputLimits` **before** committing entries keeps the stored transcript immutable so provider prompt caches are never invalidated on later turns.
- **Non-vision Model fallback:** If the active Model does not list `"image"` in `model.input`, `pi-durable` substitutes a text placeholder per request without mutating the transcript — so switching to a vision-capable Model later still sees the captured screenshot.

---

## 3. Process Isolation and Hand-Verified Confirmation Mechanics

### 3.1 Pausing a tool call for Fulcrum/Shell's hand-verified "yes"

Verified from [`packages/durable/test/examples/44-approval-gate.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/test/examples/44-approval-gate.ts), [`packages/durable/src/harness/types.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/src/harness/types.ts#L1184-L1200), and [`packages/durable/docs/spec.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/docs/spec.md) (§§5.6–5.7, 7.2, 8.4):

Pi Durable has a built-in **suspendable gate pattern** that fits CrowbarOS's *"No model on the trust path"* and *"A yes comes from the person's hand"* rules without holding a live Promise across crashes:

1. **Deterministic gate before intent:** Every tool task (`pi.tool`) begins in `{ phase: "call" }`. It validates arguments, then runs the `beforeTool` hook chain **before** committing execution intent (`{ phase: "execute" }`). A throwing hook blocks the call.
2. **Durable park via `defineSignal` + `api.memo`:** The confirmation extension defines a typed signal (`defineSignal({ name: "crowbar.approval", version: 1, schema: obj({ allow: bool() }) })`), computes a canonical digest `key` of the tool name + arguments (binding approval strictly to the exact arguments shown to the person), fixes a deadline once via first-writer-wins `api.memo("approval.until." + key, api.now() + TIMEOUT, ctx)`, and checks `api.readSignal(Approval, key, ctx)`.
3. **Sleeping in SQLite (zero held invocation):** When `readSignal` returns `undefined`, the extension publishes the pending confirmation request over CrowbarOS's IPC to Fulcrum/Shell and returns `{ sleep: { signals: [Approval.key(key)], until } }`. The Harness atomically commits the tool task as `status = 'sleeping'` at `{ phase: 'call' }`. If the Harness restarts or the Desktop suspends while waiting, the parked approval state survives intact in SQLite.
4. **Hand-verified wakeup:** When Fulcrum confirms a real hardware key/pointer press from the person's hand and notifies the Harness over Fulcrum's privileged socket, the Harness calls `harness.signal(taskId, Approval, key, { allow }, ctx)`.
5. **Re-evaluation and crash-safe execution:** Waking re-runs the entire `call` phase from the top (`beforeTool` runs again, reading the durably stored signal and memoized deadline; any additional policy gates re-check fresh conditions). If `{ allow: true }`, the Harness commits `{ phase: "execute", replay: "unsafe" }` (the default policy) and runs `execute()`. If a crash occurs mid-execution, recovery sees `replay: "unsafe"` and **never silently re-executes** the command — it records an `interrupted` result with any committed partial output back to the Model. If `{ allow: false }` or the deadline expires, `beforeTool` returns `{ block: "..." }` and `execute()` never runs.

**Two mandatory security invariants CrowbarOS must enforce around `pi-durable` (`spec.md` §12):**
- **`harness.signal()` has no built-in caller authentication** (`spec.md` §5.7: *"Neither checks who calls it"*). Only the Fulcrum-authenticated confirmation channel may invoke `harness.signal()` for `crowbar.approval`; neither the Shell's unprivileged channel, nor the person's apps, nor the tool helper may reach it.
- **No registry-wide hook (`spec.md` §12, *"Guards left out of a selection"*):** Pi Durable resolves hooks only from a conversation's selected extensions; a child conversation or subagent configured with an explicit extension array that omits the approval extension runs without it. CrowbarOS must enforce its confirmation gate either by validating/injecting the guard extension into every conversation configuration or by additionally gating privileged operations inside the Harness-side `ExecutionEnv`/IPC dispatcher itself (defense in depth).

### 3.2 Process isolation: Harness `runit` account vs. the person's account via `ExecutionEnv` + `pi-env`

Verified in [`packages/durable/src/env/index.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/src/env/index.ts), [`packages/env/README.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/env/README.md), [`packages/env/src/connection.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/env/src/connection.ts), and [`packages/env/daemon/Cargo.toml`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/env/daemon/Cargo.toml):

- **Clean built-in seam:** `HarnessOptions.env` builds an `ExecutionEnv` (`FileSystem & Shell`) per tool call and prompt render. Built-in coding tools (`read`, `write`, `edit`, `bash`) execute exclusively through `api.env`.
- **Ready-made Rust execution helper (`@earendil-works/pi-env`):** `earendil-works/pi` already ships `pi-env`, a small MIT-licensed Rust daemon (`encoding_rs`, `ignore`, `notify`, `serde_json`, `libc`) that speaks a binary framed protocol over stdin/stdout (16 MiB max frame, 5 s keepalive ping, explicit `CANCEL` frame, process-group kill on abort/close, `fd`-style `listFiles`, streaming output windows with spill files) and pairs with `RemoteExecutionEnv` via `new Connection({ command: [...] })`.
- **CrowbarOS privilege layout:**
  - **Harness service account** (e.g., `_crowbar-harness` under `runit`): owns `/var/lib/crowbar-harness/session.sqlite`, `CredentialStore` (API keys + OAuth refresh tokens), `beforeTool` confirmation hooks, and outbound network access to Model endpoints.
  - **Person-account execution helper** (running as the person's account, either connected over a per-person Unix socket or spawned via a narrow account-transition wrapper): runs `pi-env` (or a CrowbarOS Rust helper speaking the same `ExecutionEnv` contract) for file I/O, shell commands, and app launches.
  - **Custom OS tools** (Fulcrum window/workspace queries, compositor screen capture, Studio Settings mutations, `xbps` package operations): registered via `defineTool()` inside the Harness, talking to Fulcrum/Shell/system helpers over narrow IPCs, and subject to the same `beforeTool` confirmation gate.
  - Because shell commands execute strictly under the person's account, a compromised command or prompt-injected tool call cannot read provider credentials, mutate `session.sqlite`, or forge a `crowbar.approval` signal.

---

## 4. Durability, Btrfs Rollback, and Local Unix Socket IPC

### 4.1 SQLite schema migrations and Base Btrfs rollback

Verified directly in [`packages/chord/src/sqlite/migrations.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/chord/src/sqlite/migrations.ts), [`packages/durable/src/storage/sqlite/migrations.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/src/storage/sqlite/migrations.ts), and [`packages/durable/docs/spec.md` §11.2](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/docs/spec.md):

- **Exact migration semantics:** Migrations are contiguous integer versions starting at `1`, tracked in `durable_schema (singleton = 1, version)` and applied atomically in one transaction on `SqliteStorage.open()`.
- **Strict roll-forward-only refusal:** Line 33 of `chord/src/sqlite/migrations.ts` executes:
  ```ts
  if (row.version > currentVersion) {
    throw new Error(`SQLite schema version ${row.version} is newer than supported version ${currentVersion}`);
  }
  ```
- **Real schema churn already happened:** Schema v2 rebuilt the entire `tasks` table (`CREATE TABLE tasks_v2 ... DROP TABLE tasks ... ALTER TABLE ... RENAME`) to add the `'sleeping'` status to SQLite's `CHECK` constraint.
- **Btrfs rollback failure mode:** On CrowbarOS, Base upgrades snapshot the Btrfs root so the person can boot back into a prior snapshot ([`docs/LESSONS.md`](../LESSONS.md)). If `/var/lib/crowbar-harness/session.sqlite` lives on a persistent subvolume (not rolled back with root), an OS upgrade that bumps `pi-durable` from schema vN to v(N+1) followed by a Btrfs root rollback will leave the rolled-back Harness **unable to open the database at boot**.
- **Required Milestone 3 mitigation:** Before the Harness applies pending migrations on startup (inspecting `durable_schema.version` vs. bundled `CURRENT_SQLITE_SCHEMA_VERSION`), automatically create a lightweight Btrfs reflink copy (`cp --reflink=auto` or `VACUUM INTO`) named `session.sqlite.pre-v<N+1>`. On rollback (when `open()` catches a newer-schema error), the Harness can cleanly archive the newer DB to `session.sqlite.from-v<N+1>` and restore the matching `pre-v<N+1>` backup (or open a fresh DB while preserving the newer DB read-only for later recovery) — so rolling back Base never bricks the Agent.

### 4.2 WAL, checkpointing, deferred progress, and single-writer rules

Verified in [`packages/chord/src/sqlite/node.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/chord/src/sqlite/node.ts), [`packages/durable/README.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/README.md), and [`packages/durable/CHANGELOG.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/CHANGELOG.md):

| Mechanism | Verified behavior | Operational implication for CrowbarOS |
| --- | --- | --- |
| Journal & sync mode | `PRAGMA journal_mode = WAL`, `PRAGMA synchronous = NORMAL`, `PRAGMA wal_autocheckpoint = 1000`, `busyTimeoutMs = 5000`; `close()` runs `PRAGMA wal_checkpoint(TRUNCATE)` | Safe against process crashes without corruption, but under `synchronous = NORMAL` the newest uncheckpointed commits can roll back on sudden power loss (per SQLite WAL semantics and `pi-durable`'s README). CrowbarOS can override `PRAGMA synchronous = FULL` in a 10-line wrapper if power-loss durability is wanted for every commit |
| Deferred progress commits | Streaming answer deltas and running tool output are held in memory and flushed with the next structural commit or after `settings.progress.persistWithinMs` (default **1,000 ms**) | Cuts disk writes ~3–9× during streaming; a crash loses at most ~1 s of streamed partials (completed entries, tool intents, and signals remain strictly durable before observation) |
| Single-writer enforcement | **No cross-process write locking.** Exactly one process may open storage read-write; other processes may open `mode: "read-only"` (`HarnessOptions.readOnly: true`) | Maps cleanly to a single `runit`-supervised Harness daemon; GPUI Shell / Fulcrum never open the SQLite file directly |
| Fatal storage errors | Any write/storage error fails and closes the `Harness` (`harness.closed` resolves with `{ kind: "failed", error }`); no silent retry | `runit` naturally restarts the daemon on exit, and `harness.resume()` recovers cleanly from the last atomic SQLite commit |

### 4.3 Local Unix domain socket IPC to Rust/GPUI clients

Verified in [`packages/chord/src/node/http/node.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/chord/src/node/http/node.ts) and [`packages/chord/src/link/ipc.ts`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/chord/src/link/ipc.ts):

- `@earendil-works/chord/http`'s `NodeListener` listens on TCP `{ host, port }` only, and `@earendil-works/chord/link/ipc` wraps Node's `child_process.fork()` IPC channel. Neither speaks Unix domain sockets.
- **This reinforces the fit brief's core boundary rule:** Do **not** couple Fulcrum or the GPUI Shell to Chord or Pi Durable internals. Instead, run a thin CrowbarOS-owned `node:net` Unix domain socket bridge inside the Harness daemon translating between Pi Durable's `Conversation.watch()` / `submit()` / `abort()` / `signal()` / `models.login()` and a small versioned CrowbarOS wire protocol (e.g., length-prefixed or newline-delimited JSON over Unix stream sockets).
- **Zero-native-addon peer authentication on Linux:** Node's built-in `net.Socket` does not expose `SO_PEERCRED` without a native addon/FFI. Because CrowbarOS already isolates Fulcrum, the Shell, and the person's apps into distinct Linux accounts under `runit`, the Harness daemon can bind **two separate Unix sockets with kernel-enforced filesystem permissions**:
  1. `/run/crowbar-harness/shell.sock` (`0660`, group `_crowbar-shell`): summon context, submit, steer/abort, stream conversation view deltas, list tasks, OAuth login UI prompts, Model selection.
  2. `/run/crowbar-harness/fulcrum.sock` (`0660`, group `_crowbar-fulcrum`): authoritative summon context, screen-capture handoff, and **the only socket permitted to deliver `crowbar.approval` signals** from the person's hand.

---

## 5. Maturity, Breaking-Change Cadence, and Alternatives

### 5.1 Maturity and API churn of Pi Durable

Verified in [`packages/durable/README.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/README.md) and [`packages/durable/CHANGELOG.md`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/durable/CHANGELOG.md):

- **Explicit experimental status:** Line 3 of `packages/durable/README.md` still reads: `> **Experimental.** The API changes without notice between releases.`
- **Measured breaking-change cadence:** Between `v1.0.0` (2026-10-01) and `v1.5.2` (2026-11-10) — **40 days** — `pi-durable` shipped **6 releases with `### Breaking Changes`** (`1.0.3`, `1.0.4`, `1.1.0`, `1.2.0`, `1.4.0`, `1.5.0`), plus another large set of breaking changes already landed in `[Unreleased]` on `main` (removing `watchEvents()`, windowing `ConversationView.entries`, changing `pi.live` idleness checks, removing `Session.documentState()`, deferring progress commits).
- **Implication:** Pin exact versions in CrowbarOS's build, keep Pi Durable strictly behind CrowbarOS's narrow Unix socket boundary, and budget periodic deliberate upgrade PRs rather than rolling updates.

### 5.2 Primary-source comparison of viable Harness alternatives

All entries verified directly from each repository's primary source on 2026-11-18:

| Option | Primary Repo & License | Runtime & Void Base fit | Model access (API / OAuth / Local / Vision) | Crash durability & confirmation park | Cross-account isolation | Verdict for Milestone 3 |
| --- | --- | --- | --- | --- | --- | --- |
| **Pi Durable** (`@earendil-works/pi-durable` + `pi-ai` + `pi-env`) | [`earendil-works/pi`](https://github.com/earendil-works/pi) — **MIT** | Node `>=22.19.0`; Void ships `nodejs-24.22.1_1` (+11 pkgs, ~62–74 MiB installed); zero native npm addons | 30+ cloud APIs, **9 built-in OAuth flows** w/ lock-safe refresh, Ollama/llama.cpp via `OpenAICompletionsCompat`, vision + Photon resizer | **Best-in-class:** atomic SQLite WAL checkpoints, `replay: "unsafe"` default, durable `beforeTool` `{ sleep }` + `defineSignal` | **Native:** `ExecutionEnv` + MIT Rust `pi-env` helper | **Strongest fit**, conditional on narrow CrowbarOS IPC boundary, Btrfs pre-migration snapshot, and Node footprint acceptance |
| **Pi Coding Agent RPC mode** (`@earendil-works/pi-coding-agent --mode rpc`) | [`earendil-works/pi`](https://github.com/earendil-works/pi/blob/5a14e69c067d40dfbff4f2fff7d3b34c897e2dc9/packages/coding-agent/docs/rpc.md) — **MIT** | Node `>=22.19.0` (or Bun compiled binary, not in Void); heavier CLI deps (`tui`, `jiti`, `highlight.js`, `lovely-mermaid`, `undici`) | Same `@earendil-works/pi-ai` layer | JSONL session files, single-client stdin/stdout JSONL; **no** durable SQLite task/signal/park state machine or crash-safe `replay: "unsafe"` scheduler | Tools execute in-process as the CLI account; no built-in `ExecutionEnv` boundary | **Inferior to Pi Durable:** same Node runtime cost without durability, multi-client state, or cross-account `ExecutionEnv` |
| **OpenAI Codex `app-server`** | [`openai/codex`](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md) — **Apache-2.0** | **Native Rust** (0 extra Base runtime packages) | Tightly coupled to OpenAI / ChatGPT accounts & OpenAI Responses wire assumptions; no multi-provider OAuth suite (Anthropic/Copilot/etc.) | Thread store + JSON-RPC `item/commandExecution/requestApproval` with reconnect replay; rejects if no client connected | Built-in OS sandboxing for commands, but designed around OpenAI's CLI/app workflow rather than CrowbarOS service accounts | **Poor fit for vendor-neutral OS:** great Rust JSON-RPC reference, but wrong provider/OAuth model for CrowbarOS |
| **Goose** | [`aaif-goose/goose`](https://github.com/aaif-goose/goose) — **Apache-2.0** | **Native Rust** (0 extra Base runtime packages) | 15+ providers (API keys + Ollama) + MCP; relies on external [ACP](https://goose-docs.ai/docs/guides/acp-providers) wrappers for Claude/ChatGPT/Gemini subscriptions rather than built-in OAuth | Session persistence, but lacks Pi Durable's durable task/signal/park recovery and `replay: "unsafe"` tool semantics | Extension/MCP oriented; no built-in `ExecutionEnv` cross-account split | **Viable Rust fallback** if Base refuses Node.js, but weaker OAuth, durability, and isolation semantics |
| **OpenCode** | [`anomalyco/opencode`](https://github.com/anomalyco/opencode/blob/main/package.json) — **MIT** | **Bun** (`bun@1.4.4`, Effect TS) — **not in Void `xbps`** | Broad providers & local Models | Client/server architecture, SQLite via `@effect/sql-sqlite-bun`, coding-agent oriented | Built around coding workspace sessions + `node-pty`, not OS service-account separation | **Eliminated:** Bun is absent from Void Linux and adds heavier runtime mismatch than Node |
| **Minimal custom Rust Harness** (e.g., on [`0xPlaygrounds/rig`](https://github.com/0xPlaygrounds/rig) MIT or raw `reqwest` + `rusqlite`) | Custom / `rig` (**MIT**) | **Native Rust** (0 extra Base packages, ~5–15 MiB RSS, native `SO_PEERCRED` on Unix sockets, full migration control) | Must build & maintain 9 OAuth flows + refresh locking, provider adapters, Ollama/llama.cpp quirks, streaming partial tool-JSON, image resizing | Must build SQLite state machine, crash recovery, compaction, cancellation, and approval parking from scratch | Total control (native Rust helper & Fulcrum IPC) | **Best long-term purity, worst Milestone 3 schedule risk:** directly conflicts with `LESSONS.md` (*"build what a person can touch first"*). Keeping CrowbarOS's IPC boundary narrow preserves this as a clean post-M3 swap path |

---

## 6. Verification of Assumptions in `pi-durable-fit-brief.md`

| Fit-brief claim | Verdict | Primary-source finding |
| --- | --- | --- |
| *"Requires Node.js 22.19+ or Bun"* | **Confirmed (Node only on Void)** | `packages/*` declare `node >= 22.19.0`; Void ships `nodejs-24.22.1_1` and does **not** package Bun. Note `apps/p3` already moved to Node 26.3.1+ |
| *"Shipping a single pinned bundle with no `npm` on the running OS"* | **Needs explicit work** | Single JS bundle works with `esbuild` + `createRequire` shim + static OAuth/Bedrock loader registration + sidecar `photon_rs_bg.wasm`. However, Void's stock `nodejs` package ships 11.64 MiB of `npm` that Base must strip or omit via `--without-npm` |
| *"MIT-licensed and ~15,000 lines of readable code"* | **Partially corrected** | License is **MIT**, but `packages/durable/src/` is now **103 files / 1.46 MB (~35k–40k+ lines)** plus `chord`, `pi-ai`, `pi-codemode`, `pi-schema`, `pi-telemetry`, `photon` |
| *"`beforeTool` + `api.memo` cleanly pauses for hand-verified confirmation"* | **Confirmed & stronger** | `beforeTool` natively supports durable `{ sleep: { signals, until } }` + `defineSignal` + `api.memo` + `harness.signal()` (`44-approval-gate.ts`). Must enforce caller auth on `signal()` and unconditional guard extension selection |
| *"`ExecutionEnv` enables separate Harness account vs. person's account"* | **Confirmed & accelerated** | `@earendil-works/pi-env` already provides a small MIT Rust stdin/stdout daemon implementing `RemoteExecutionEnv` |
| *"Dynamic `section()` prompt system keeps prompt caches warm"* | **Confirmed** | `spec.md` §7.4 records minimal `pi.system` deltas and head rebaselines; renderers must be deterministic |
| *"Roll-forward-only SQLite migrations conflict with Btrfs rollback"* | **Confirmed** | `chord/src/sqlite/migrations.ts` explicitly throws when DB version > supported version; schema v2 already rebuilt `tasks`. Needs pre-migration DB snapshot/reflink before opening after upgrade |
| *"Experimental API churn → hide behind narrow CrowbarOS Unix socket IPC"* | **Strongly confirmed** | 6 breaking-change releases in 40 days + more in `[Unreleased]`; Chord has no Unix socket listener anyway, making a narrow CrowbarOS `node:net` Unix socket bridge both necessary and protective |
| *"Verify OAuth, local Models, and vision in `@earendil-works/pi-ai`"* | **Confirmed** | 9 OAuth flows (loopback redirect + device code) with lock-safe refresh, full OpenAI-compat local-Model support (via `createProvider()`), and vision `ImageContent` + Photon worker resizing |
