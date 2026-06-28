# AutoRouter

**One local gateway. Every AI protocol. Every model. Every tool.**

AutoRouter is a local-first desktop application that lets you
configure your AI providers **once** and then use any model from any
provider inside any supported AI tool — without editing
configuration files again.

It is a **universal AI protocol translation, compatibility, routing,
and provider management layer** that sits between AI coding tools
(Claude Code, Codex, Gemini CLI, Continue, Aider, Cline, OpenCode,
Warp, Antigravity CLI, Roo Code, Cursor, and any future coding
agent) and upstream AI providers (OpenAI, Anthropic, Gemini,
OpenRouter, Groq, Together AI, local servers, and any
OpenAI-compatible endpoint).

The flagship experience is a Tauri 2 desktop application with a
dashboard, provider management, routing rules, live observability,
and per-platform installers. A headless gateway binary is also
shipped for users who want the engine without the GUI.

---

## 1. Vision

Every AI tool speaks exactly one wire format. Every AI provider
speaks exactly one wire format. The result is a combinatorial
explosion of glue code, per-tool configuration, and per-provider
API keys, duplicated across every machine the user owns.

AutoRouter collapses that explosion to a single seam:

1. **The tool side is normalised.** Every AI tool points its base
   URL at the local AutoRouter gateway (`http://127.0.0.1:4073`).
   The tool keeps speaking its native protocol — OpenAI Chat
   Completions, OpenAI Responses, Anthropic Messages, or Gemini
   `generateContent` — and AutoRouter speaks it back.
2. **The provider side is normalised.** Every upstream provider is
   reached through a single **Universal Schema** — a
   provider-neutral intermediate representation of messages,
   system prompts, tools, multimodal content, streaming deltas,
   token usage, finish reasons, safety, and metadata.
3. **The middle decides everything.** A smart routing engine picks
   the best upstream for each request based on rules, model
   capabilities, live health, free-tier availability, cost,
   latency, and user preferences. Failover chains and
   quota-aware scheduling happen here, transparently.

The result: a user configures their providers once, and every tool
on the machine — today and in the future — can use any model from
any provider. The routing engine does the optimisation in the
background, and the user can always override with a single
`X-AutoRouter-Target` header.

---

## 2. Core principles

These are non-negotiable. They are encoded in the architecture and
enforced by the build (see [AGENTS.md](AGENTS.md) for the
companion agent instructions).

1. **No pairwise protocol conversion.** Every request goes through
   `decode → Universal Schema → encode`. We never write
   `OpenAI → Anthropic` style glue. The schema is the only bridge.
2. **The Universal Schema is the contract.** All wire formats are
   adapters to the schema. The schema is the API surface. New
   protocols are added by adding adapters, not by adding
   converters.
3. **The smart router is the only place that picks a target.** No
   handler hardcodes `target = source`. The user can always
   override with `X-AutoRouter-Target`.
4. **Secrets never live in config files.** API keys are resolved
   from the OS keychain (Windows Credential Manager, macOS
   Keychain, Linux Secret Service) or from `env:NAME` references,
   never inlined in `AppConfig` or `config.toml`.
5. **Loopback by default.** The gateway binds to `127.0.0.1:4073`.
   AI tool traffic stays on the device; only the configured
   upstream receives the user's network traffic.
6. **Local-first and observable.** The full request lifecycle is
   inspectable: structured logs, distributed traces, per-request
   correlation IDs, per-provider latency, and translation
   overhead are all first-class.

---

## 3. Supported ecosystems

### 3.1 Source protocols (the tool side)

| Protocol                     | Endpoint                              | `X-AutoRouter-Source` |
| ---------------------------- | ------------------------------------- | --------------------- |
| OpenAI Chat Completions      | `/openai/v1/chat/completions`         | `openai`              |
| OpenAI Responses             | `/openai/v1/responses`                | `openai`              |
| Anthropic Messages           | `/v1/messages`                        | `anthropic`           |
| Gemini `generateContent`     | `/v1beta/models/*path`                | `gemini`              |

### 3.2 Upstream providers (the provider side)

| Provider                            | Auth style                          | How to configure                       |
| ----------------------------------- | ----------------------------------- | -------------------------------------- |
| **OpenAI**                          | `Authorization: Bearer …`           | First-class (built-in adapter)         |
| **Anthropic**                       | `x-api-key: …` + `anthropic-version` | First-class (built-in adapter)         |
| **Gemini**                          | `?key=…`                            | First-class (built-in adapter)         |
| OpenRouter                          | OpenAI-compat                       | `providers.custom.openrouter`          |
| Groq                                | OpenAI-compat                       | `providers.custom.groq`                |
| Together AI                         | OpenAI-compat                       | `providers.custom.together`            |
| Fireworks                           | OpenAI-compat                       | `providers.custom.fireworks`           |
| Local (llama.cpp, LM Studio, Ollama) | OpenAI-compat                     | `providers.custom.local`               |
| Any OpenAI-compatible endpoint      | OpenAI-compat                       | `providers.custom.<name>`              |

Configuring an OpenAI-compatible provider is a **config-only**
change (no code). Adapting a brand-new wire format is a **code**
change — add an adapter under `crates/autorouter-translate/`. See
the `add-provider-adapter` skill for the workflow.

### 3.3 AI tools (the client side)

AutoRouter is a drop-in local endpoint for every major AI tool.
Set the base URL to `http://127.0.0.1:4073` and the right
`X-AutoRouter-Source` header. See [manual.md](manual.md) §7 for
per-tool recipes.

Supported: **Claude Code, Codex, Gemini CLI, Continue, Aider,
Cline, OpenCode, Warp, Antigravity CLI, Roo Code, Cursor** (where
OpenAI is accepted), and any future tool that speaks one of the
four source protocols.

---

## 4. How it works (request lifecycle)

```
┌──────────────┐    wire format    ┌────────────────────────────────────┐
│  AI tool     │ ───────────────▶ │  AutoRouter gateway (127.0.0.1)    │
│  (Claude,    │                  │                                    │
│   Codex, …)  │                  │  1. Auth + rate limit + body cap    │
└──────────────┘                  │  2. Decode (adapter by X-Source)    │
                                  │  3. SmartRouter.decide()            │
                                  │  4. Encode (adapter for upstream)   │
                                  │  5. Upstream call + streaming       │
                                  │  6. Decode response → Universal     │
                                  │  7. Encode response (source wire)   │
                                  │  8. Trace + log + metric            │
                                  └────────────────────────────────────┘
                                                                  │
                                                                  ▼
                                                          ┌──────────────┐
                                                          │  Upstream    │
                                                          │  provider    │
                                                          └──────────────┘
```

Every step is observable, every step is testable, every step is
pluggable.

---

## 5. The Universal Schema

Defined in
[`crates/autorouter-core/src/model.rs`](crates/autorouter-core/src/model.rs).
This is the contract that every adapter speaks and the contract
that the routing engine reasons about.

| Type           | Fields                                                                                    |
| -------------- | ----------------------------------------------------------------------------------------- |
| `Request`      | `messages`, `system`, `tools`, `tool_choice`, `temperature`, `top_p`, `max_tokens`, `stop`, `stream`, `metadata`, provider extensions. |
| `Message`      | `role` (`system` / `user` / `assistant` / `tool`), `content` (`Vec<ContentPart>`).        |
| `ContentPart`  | `Text`, `Image`, `Audio`, `Document`, `ToolCall`, `ToolCallRaw`, `ToolResult`, `ToolUse`, **`Reasoning`**, `Unknown`. |
| `Tool`         | `name`, `description`, `input_schema` (JSON Schema).                                       |
| `ToolCall`     | `id`, `name`, `arguments` (JSON value).                                                   |
| `Usage`        | `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`.               |
| `FinishReason` | `Stop` / `Length` / `ToolCalls` / `ContentFilter` / `Error` / provider extensions.        |
| `StreamChunk`  | `delta` (`TextDelta`, `ReasoningDelta`, `ToolCallDelta`, `UsageDelta`, `Finish`), `index`, `event`. |

Anything an upstream returns that does not fit is wrapped in a
provider extension on `Request.metadata` / `Response.metadata` and
round-tripped losslessly.

**Reasoning / thinking content** is preserved end-to-end through
three layers (all in `autorouter-translate`):
1. **Separate-field decode** — the dedicated reasoning field on
   each wire format (`delta.reasoning_content`,
   `output[]` `type: "reasoning"`, `content_block_delta`
   `type: "thinking_delta"`, `parts: [{ thought: true }]`) becomes
   `ContentPart::Reasoning` / `StreamEvent::ReasoningDelta`.
2. **Inline-tag extraction** — upstreams that embed reasoning
   inline in `content` as `<think>...</think>` or
   `<thinking>...</thinking>` blocks have those tags stripped and
   reclassified as reasoning via the
   `reasoning_extractor` module (a state machine with a 64 KiB
   carry buffer that handles tags split across SSE chunks).
3. **Response-side re-emit** — `ContentPart::Reasoning` is
   serialised back out in each target wire format.

---

## 6. Provider model

Each upstream is configured as a `ProviderEntry` in `config.toml`:

```toml
[providers.openai]
display_name        = "OpenAI"
base_url            = "https://api.openai.com"
api_key_secret_id   = "env:OPENAI_API_KEY"   # or a stored secret id
default_headers     = {}                     # e.g. custom org/project headers
model_allowlist     = ["gpt-5*", "gpt-4o*", "o1-*"]
```

The four provider kinds (`OpenAI`, `Anthropic`, `Gemini`, `Custom`)
share the same shape. `Custom` is any OpenAI-compatible endpoint
(Together, Groq, OpenRouter, llama.cpp, …) and is a config-only
addition.

For first-class providers, AutoRouter ships:

- A wire-format **adapter** in `crates/autorouter-translate/`
- An `HttpUpstream` in `crates/autorouter-server/` that resolves
  the secret, attaches the right auth header, and enforces
  `model_allowlist`
- A capability-registry entry (context window, pricing, tool/vision
  support, streaming, reasoning, latency, health)

For `Custom` providers, only the `HttpUpstream` and the
`config.toml` block are needed — the OpenAI Chat Completions
adapter is reused.

---

## 7. Routing engine

The smart router decides which upstream receives each request.
Decisions are made in a fixed precedence order — top wins.

| Layer                  | Source                                                |
| ---------------------- | ----------------------------------------------------- |
| **Target override**    | `X-AutoRouter-Target` request header                  |
| **Default target**     | `config.defaults.default_provider` + `default_model`  |
| **Rule engine**        | `config.routing.rules` (first match wins)              |
| **Capability filter**  | Drop models that do not meet the request's needs       |
| **Health penalty**     | Lower score for providers with recent failures         |
| **Final decision**     | `RouteDecision { provider, model, reason: String }`    |

### 7.1 Rule types

| Strategy               | Example rule                                              |
| ---------------------- | --------------------------------------------------------- |
| Manual                 | `Coding → claude-sonnet-4-5`                              |
| Profile-based          | `profile:reasoning → o3 / claude-opus / gpt-5`            |
| Capability-aware       | `requires:vision → gemini-2.5-pro / gpt-4o`               |
| Context-length-aware   | `tokens > 200k → gemini-2.5-pro`                          |
| Multimodal-aware       | `image_present → gpt-4o / gemini-2.5-pro`                 |
| Latency-aware          | `prefer:p95 < 1s → groq / cerebras`                       |
| Free-tier              | `free → gemini-2.5-flash / openrouter:free`               |
| Cost-aware             | `budget:low → haiku / flash / mini`                       |
| Fallback chain         | `primary → fallback_a → fallback_b`                       |
| Failover               | Retry on 5xx / 429 across the chain                       |
| Health-based           | Penalise providers with recent 5xx; auto-remove on N fails |
| Quota-aware            | Avoid providers near their daily quota                    |
| Benchmark (future)     | `bench:reasoning → <highest-scoring model>`               |

### 7.2 Capability registry

Each model has a registered capability profile:

- Context window (input + output)
- Pricing (per 1M input tokens, per 1M output tokens, cache
  read / write)
- Tool support (function calling, parallel calls, structured
  outputs)
- Vision support (image inputs, document inputs, image outputs)
- Streaming support (SSE shape, sentinel style)
- Reasoning capabilities (chain-of-thought, hidden reasoning)
- Latency metrics (p50, p95, last observed)
- Provider health (rolling 1m / 5m / 1h success rate)

Rule precedence is **first match wins**. Conflicts between rules
are resolved by listing the more specific rule first.

---

## 8. Desktop application

The flagship experience is a Tauri 2 shell that hosts the gateway
and a built-in React + Vite dashboard. Every page exposes
one-click copy-to-clipboard for every value users need to paste
into another tool (gateway URL, tool snippets, model ids,
provider base URLs, request / session / config JSON), and every
action fires a 2-second toast confirmation.

| Page            | What it does                                                                 |
| --------------- | ---------------------------------------------------------------------------- |
| Dashboard       | Live status, bind address, uptime, session count, provider health, throughput, 10 tool snippets, LiveStrip (Recent sessions + Recent requests), custom provider form. |
| Providers       | Add / edit / remove providers; key testing; model picker; inline copy on Base URL. |
| Models          | View and edit the capability registry; inline copy on every model id. |
| Sessions        | Active AI-tool connections, request counts, source / target provider, label; inline copy on session id. |
| Routing         | CRUD for routing rules; profile manager; live rule-test runner. |
| Health          | Per-provider latency, success rate, error histogram; live `/healthz` URL with inline copy. |
| Requests        | Request inspector: full request / response / trace / timing; inline copy on request id and session id in the expanded row. |
| Analytics       | Token usage, cost estimation, error rate, top tools, top models. |
| Logs            | Live tail with level filter and message search; inline copy on every log line. |
| Debug           | Replay a captured request; compare two requests; export bundle. |
| Tool profiles   | Pre-built configs for Claude Code, Codex, Aider, …; inline copy on the test result JSON. |
| Settings        | Server, defaults, logging, storage, auto-update. Hot-reloads on save. Inline copy on bind URL, data dir path, and database path. |
| Import / Export | Backup and restore the full configuration (TOML, keys excluded); Copy button next to Download. |
| Update          | Check for new version, signed release, changelog, install.                   |

Keyboard shortcuts: `Ctrl/Cmd + 1..9` for the first nine pages, `Ctrl/Cmd + 0` for Tool profiles, `Ctrl/Cmd + L` for Logs, `Ctrl/Cmd + ,` for Settings, `Ctrl/Cmd + R` to refresh status.

---

## 9. HTTP surface

```
POST /openai/v1/chat/completions
POST /openai/v1/responses
POST /v1/messages                       (Anthropic)
POST /v1beta/models/*path               (Gemini)
GET  /healthz
GET  /v1/sessions
GET  /v1/models
GET  /metrics
GET  /ui/status                         (desktop)
GET  /ui/providers                      (desktop)
GET  /ui/sessions                       (desktop)
GET  /ui/settings                       (desktop)
GET  /ui/logs                           (desktop)
POST /ui/restart                        (desktop)
```

Set `X-AutoRouter-Source: openai|anthropic|gemini` to tell the
gateway which provider the tool is impersonating. The
`X-AutoRouter-Session`, `X-AutoRouter-Label`, and
`X-AutoRouter-Target` headers are used for session correlation
and target override.

---

## 10. Security model

- **Loopback by default.** The gateway binds to `127.0.0.1:4073`.
  External connections are rejected.
- **Bearer auth on every route, including `/ui/*`.** The
  `maybe_authorize()` middleware is enforced on the entire router
  stack, not just the public API.
- **Secrets in the OS keychain.** API keys are resolved from
  Windows Credential Manager, macOS Keychain, or Linux Secret
  Service. A file-backed store with `0600` permissions is the
  fallback. `env:NAME` references are also supported.
- **Atomic config writes.** `config.toml` is updated via
  tmp-file + rename to avoid corruption on crash.
- **No telemetry.** AutoRouter does not phone home. The only
  outbound traffic is to the configured upstream providers.

See [manual.md](manual.md) §13 for the full security model.

---

## 11. Observability

- **Structured logging** via `tracing` with a custom `MakeWriter`
  that pipes every event into the desktop Logs page.
- **Distributed tracing** with per-request correlation IDs
  (`X-Request-Id`, `traceparent`).
- **Per-provider latency tracking** (p50, p95, p99) and translation
  overhead measurement.
- **Prometheus `/metrics`** endpoint for throughput, error rate,
  token usage, and rate-limit hits.
- **Request inspector** in the desktop UI — view the full request,
  full response, full trace, and per-step timing for any captured
  request.
- **Crash recovery** — on restart, the gateway restores the last
  known good configuration and replays any in-flight session state.
- **Safe shutdown** — SIGINT / SIGTERM trigger a graceful drain
  (finish in-flight requests, close upstream connections, flush
  storage).

---

## 12. Performance targets

Measured by `cargo bench -p autorouter-observability` (Criterion).

| Metric                       | Target    |
| ---------------------------- | --------- |
| Translation overhead p95     | < 5 ms    |
| Streaming first-byte latency | < 20 ms   |
| Cold-start time              | < 2 s     |
| Resident memory              | < 200 MB  |
| Gateway throughput           | > 500 rps on commodity hardware |

The benchmarks cover the full `decode → encode` path for each
adapter and the SSE streaming path end-to-end.

---

## 13. Architecture and crate structure

The workspace is organised as a small set of crates with clear
ownership boundaries. Each crate compiles, tests, and benches
independently.

| Crate                       | Responsibility                                                                 |
| --------------------------- | ------------------------------------------------------------------------------ |
| `autorouter-core`           | Provider-neutral universal schema: messages, content, tools, usage, streaming. |
| `autorouter-translate`      | OpenAI Chat, OpenAI Responses, Anthropic Messages, Gemini adapters and pipeline. |
| `autorouter-config`         | Configuration loader, secret store, SQLite-backed persistent state.            |
| `autorouter-server`         | Local HTTP gateway that emulates provider APIs and forwards upstream. Includes the dashboard API under `/ui/*`. |
| `autorouter-router`         | Smart routing engine: rules, capability registry, health, smart router.        |
| `autorouter-observability`  | Tracing, metrics, benchmarks, recovery primitives.                             |
| `autorouter-app`            | Headless binary entry point that wires the crates together.                    |
| `autorouter-desktop`        | Tauri 2 desktop shell that hosts the gateway and the dashboard UI.             |

The dependency graph is strictly acyclic:

```
        core ← translate ← server → app
          ↑                ↑
   config → router → observability
                          ↑
                      desktop (Tauri shell)
```

Cross-crate boundaries are enforced by code review and by the
agent-customization rules in [AGENTS.md](AGENTS.md). The Hard Rules
there prevent the most common boundary violations.

---

## 14. Plugin and extension model

AutoRouter is designed to be extended without modifying the core:

| You want to …                                              | What to do                                                              |
| ---------------------------------------------------------- | ----------------------------------------------------------------------- |
| Add a new OpenAI-compatible provider                       | Add a `providers.custom.<name>` block in `config.toml`. No code change. |
| Add a brand-new wire-format protocol                       | Add an adapter in `crates/autorouter-translate/`. Register it in `pipeline_adapters()` and in `ProviderKind`. |
| Add a new routing strategy                                 | Add a new rule type in `crates/autorouter-router/`. Rules are data — users compose them in `config.toml`. |
| Add a new dashboard page                                   | Add a route under `/ui/*` and a page under `ui/src/pages/`. Hot-loaded by the Vite dev server. |
| Add a new tool profile                                     | Add a `tool-profiles/<name>.json` fixture; link from Settings.          |

Static linking through the `pipeline_adapters()` factory is used and keeps the binary
small and fast to compile.

---

## 15. Build, test, install

### 15.0 Prerequisites

Before building, ensure the following are installed and on `PATH`:

| Tool | Minimum version | Install |
| ---- | --------------- | ------- |
| Rust toolchain | 1.83 (see `rust-toolchain.toml`) | `rustup update stable` |
| Node.js | 18 LTS or later | https://nodejs.org |
| NSIS (Windows) | 3.x | https://nsis.sourceforge.io (auto-downloaded by Tauri if absent) |
| WiX Toolset (Windows) | 3.x | auto-downloaded by Tauri on first run |
| Tauri CLI (Node) | `^2.11.3` | already in `scripts/node_modules`; install once with `npm install --prefix scripts` |

### 15.1 Step-by-step: production desktop build (Windows)

The authoritative build script is `scripts/bundle.mjs`. It handles
every step in order and is the only supported way to produce
release installers.

```powershell
# 1. Install Tauri CLI into scripts/node_modules (one-time, already done
#    if scripts/node_modules/@tauri-apps/cli exists)
npm install --prefix scripts

# 2. Install UI dependencies (one-time, already done if ui/node_modules exists)
npm install --prefix ui

# 3. (Optional) Regenerate theme-adaptive tray icons from the SVG source.
#    Requires @resvg/resvg-js (already in ui/package.json devDependencies).
#    Run this whenever ui/public/logo_square.svg changes.
npm install --prefix ui   # installs @resvg/resvg-js
node scripts/gen-theme-icons.mjs
#    Writes to crates/autorouter-desktop/icons/:
#      tray-dark.png / tray-dark@2x.png   (white, for OS dark mode)
#      tray-light.png / tray-light@2x.png (black, for OS light mode)

# 4. Run the full production build (UI + Rust release + NSIS + MSI)
#    AUTOROUTER_SKIP_SIGNING=1 disables code-signing (no cert required)
$env:AUTOROUTER_SKIP_SIGNING="1"; node scripts/bundle.mjs
```

What `bundle.mjs` does internally:

1. Runs `npm run build` in `ui/` — Vite/TypeScript build → `ui/dist/`
2. Validates `ui/dist/` exists and is non-empty
3. Patches signing fields in `tauri.conf.json` from env vars (skipped
   when `AUTOROUTER_SKIP_SIGNING=1`)
4. Invokes `node scripts/node_modules/@tauri-apps/cli/tauri.js build
   --bundles msi,nsis` from `crates/autorouter-desktop/`
5. Tauri CLI compiles `autorouter-desktop` in `--release` mode (Rust)
   and bundles both installers

> **Note on exit code**: On Windows, PowerShell's `2>&1` redirect
> causes `NativeCommandError` when Node writes informational text to
> stderr (e.g. "Info Looking up installed tauri packages…"). This
> is a PowerShell artefact — Tauri's "Finished 2 bundles at:" output
> confirms a successful build. The exit code from `bundle.mjs` itself
> (`process.exit(r.status)`) is the authoritative signal.

### 15.2 Installer output paths

After a successful build the installers are at:

```
target/release/bundle/
  msi/AutoRouter_0.1.0_x64_en-US.msi      ← WiX MSI installer
  nsis/AutoRouter_0.1.0_x64-setup.exe     ← NSIS per-machine installer
```

The NSIS installer uses `installMode: perMachine` and embeds the
WebView2 bootstrapper (`webviewInstallMode: embedBootstrapper`) so
it works on machines without a pre-existing WebView2 runtime.

### 15.3 Installer targets per platform

| Platform | Targets                                                                          |
| -------- | -------------------------------------------------------------------------------- |
| Windows  | NSIS setup `AutoRouter_<v>_x64-setup.exe`, WiX MSI `AutoRouter_<v>_x64_en-US.msi` |
| macOS    | `.app` bundle and `.dmg` disk image (one per arch: arm64 + x86_64)              |
| Linux    | `.deb` and portable `.AppImage`                                                  |

The Windows installer is verified end-to-end on the project host.
The macOS and Linux installer configurations are wired up and the
GitHub Actions matrix in `.github/workflows/build.yml` produces
them on the matching runners. Cross-compiling from Windows to
macOS / Linux is not feasible.

To sign the installers for distribution, set these env vars before
running `bundle.mjs` (omit `AUTOROUTER_SKIP_SIGNING`):

```powershell
$env:WINDOWS_CERT_FILE = "path\to\cert.pfx"  # Windows code-signing cert
# macOS only:
$env:APPLE_ID          = "your@apple.id"
$env:APPLE_TEAM_ID     = "XXXXXXXXXX"
# APPLE_PASSWORD / APPLE_API_KEY configured in notarytool keychain
node scripts/bundle.mjs
```

### 15.4 Development loop

```powershell
# Run tests across all crates
cargo test --workspace

# Headless gateway (no Tauri, no UI)
cargo run -p autorouter-app

# Full desktop shell with hot-reload UI (recommended dev loop)
cargo run -p autorouter-desktop

# UI-only hot reload (no Rust rebuild)
npm run dev --prefix ui

# Regenerate tray icons after SVG changes
node scripts/gen-theme-icons.mjs
```

### 15.5 Configuration sources (in precedence order)

1. Built-in defaults
2. `/etc/autorouter/config.toml` (or `%ProgramData%\autorouter\config.toml`)
3. `<user config dir>/config.toml`
4. Environment variables prefixed with `AUTOROUTER_`
5. Runtime overrides (set by the desktop UI)

### 15.6 Benchmarks

`cargo bench -p autorouter-observability` runs Criterion
benchmarks over the translation layer and the SSE streaming path.

---

## 16. Roadmap

| Milestone | Status | What ships |
| --- | --- | --- |
| **M0** — Core (schema + 4 adapters)        | `shipped`    | Universal Schema, four adapters, gateway skeleton, Tauri shell. |
| **M1** — Real upstream + secret resolution  | `shipped`    | `HttpUpstream`, OS keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service), `providers.custom.<name>`, `model_allowlist`. |
| **M2** — Smart router                       | `shipped`    | `RuleEngine`, `CapabilityRegistry`, `HealthTracker`, `SmartRouter`, `X-AutoRouter-Target` override, fallback chains. |
| **M3** — Observability                      | `shipped`    | `tracing` + `LogBridge` piped to UI, `RequestContext` with correlation id, Prometheus `/metrics`, request inspector. See [manual.md](manual.md) §11. |
| **M4** — Desktop app v1                     | `shipped`    | 14-page dashboard (Dashboard, Providers, Models, Sessions, Routing, Health, Requests, Analytics, Debug, Tool profiles, Import/Export, Update, Logs, Settings), sessions tail, import/export, auto-update. See [manual.md](manual.md) §5. |
| **M5** — Compatibility hardening            | `shipped`    | Per-adapter `encode_stream_chunk` overrides the default, trailing sentinels match the source wire format, tool-call + multimodal round-trips, `X-AutoRouter-Target` cost-aware fallback chains. See [manual.md](manual.md) §10 and §14. |


---

## 17. Documentation

- [manual.md](manual.md) — the complete user manual: install
  walkthrough for Windows, macOS, and Linux; full tour of the
  desktop dashboard; provider and per-tool configuration recipes;
  HTTP API reference; troubleshooting; and FAQ.
- [AGENTS.md](AGENTS.md) — always-on agent instructions for AI
  coding agents working in this codebase.

---
