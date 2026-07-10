# AutoRouter — Agent Instructions

AutoRouter is a **Rust + Tauri 2** local-first desktop app that emulates
the OpenAI, Anthropic, and Gemini wire formats on a single local
endpoint and forwards each request through a smart routing engine to
an upstream provider.

These instructions are the always-on context an AI coding agent needs
to be productive in this workspace. They are intentionally short and
link out to the long-form docs.

---

## Where to find things

| Need                                              | File                                                                  |
| ------------------------------------------------- | --------------------------------------------------------------------- |
| High-level overview, crate map, HTTP surface      | [README.md](README.md)                                                |
| User manual (install, config, recipes, FAQ)       | [manual.md](manual.md)                                                |
| **Non-negotiable rules**                             | `AGENTS.md` “Hard rules” section                                       |
| Workspace + dependency versions                   | [Cargo.toml](Cargo.toml)                                              |
| Wire-format adapters + translation pipeline       | `crates/autorouter-translate/`                                        |
| Provider-neutral universal schema                 | `crates/autorouter-core/src/model.rs`                                 |
| Smart router (rules, capabilities, health)        | `crates/autorouter-router/`                                           |
| Axum gateway + `/ui/*` routes                     | `crates/autorouter-server/`                                          |
| Tauri shell, system tray, window                  | `crates/autorouter-desktop/`                                         |
| Dashboard UI (React + Vite)                       | `ui/`                                                                 |

If a question is answered in any of those files, **link, do not
embed**. Inline duplication rots.

---

## Crate boundaries (ownership rules)

| Crate                        | Owns                                                                                       | Must **not** touch                                |
| ---------------------------- | ------------------------------------------------------------------------------------------ | ------------------------------------------------- |
| `autorouter-core`            | Universal Schema: `Message`, `ContentBlock`, `Tool`, `Usage`, `Request`, `Response`, `StreamChunk`. No wire formats. | Wire-format JSON, HTTP, config, secrets           |
| `autorouter-translate`       | The four adapters (`openai_chat`, `openai_responses`, `anthropic_messages`, `gemini_generate_content`) + the pipeline. | HTTP clients, secrets, persistence                |
| `autorouter-config`          | TOML loader, secret store, SQLite storage, config schema, paths                            | Wire formats, routing decisions                   |
| `autorouter-router`          | `SmartRouter`, `IdentityRouter`, `RuleEngine`, capability registry, `HealthTracker`        | HTTP, wire formats                                |
| `autorouter-server`          | Axum gateway, route registration, `/ui/*` dashboard routes, bearer auth middleware, `GatewaySupervisor` (owns the running `TcpListener` + axum task so PATCH `/ui/settings` and `cmd_restart` can hot-rebind) | Translation logic, secret store internals         |
| `autorouter-observability`   | `tracing` setup, Prometheus metrics, Criterion benchmarks                                  | Translation logic                                 |
| `autorouter-app`             | Headless binary entry point                                                                | UI, Tauri                                         |
| `autorouter-desktop`         | Tauri 2 shell that hosts the gateway                                                       | UI styling                                        |

If you find yourself reaching across these boundaries, you are almost
certainly solving the wrong problem.

### UI / Dashboard primitives (`ui/src/components/`)

Three reusable components back the Dashboard's "one-click copy
everywhere" UX. **Reuse them instead of hand-rolling copy buttons,
toasts, or code blocks.**

| Component       | Purpose                                                                                | When to use                                          |
| --------------- | -------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| `CopyButton`    | One-click copy-to-clipboard with toast feedback. `variant: "inline" \| "block"`, `size: "sm" \| "md"`. Pass `className="card bind-card"` on the Bind tile to fuse into a `.card`-shaped click target (see Dashboard). | Any "copy this thing" affordance. `stopPropagation` is already handled for nested clickables. |
| `CodeBlock`     | Monospace code block with a language-tinted header strip and built-in copy button. `language: "sh" \| "toml" \| "json" \| "env" \| "python" \| "text"`. | Any pre-formatted config / command / snippet.        |
| `Toast`         | `ToastProvider` + `useToast()` hook. 2-second auto-dismiss with `id` preempting.        | Any user feedback (save success, copy, error). The `App` page tree is wrapped in `<ToastProvider>` — call `useToast()` anywhere inside. |

`App.tsx` is wrapped in `<ToastProvider>` at the page-tree root. The
Dashboard's `LiveStrip` polls `api.sessions()` + `api.events()` every
5s and surfaces both **Recent sessions** and **Recent requests** in a
2-column card grid. Do not roll a parallel implementation.

### Copy affordances — where they live

One-click copy is wired into the pages where users actually need
to copy something into another tool:

| Page                | What is copyable                                                                  |
| ------------------- | --------------------------------------------------------------------------------- |
| `Dashboard`         | Local gateway URL, every tool snippet (Claude Code, Codex, Gemini CLI, OpenCode, Aider, Continue, Cline, Roo Code, Warp, generic Python), X-AutoRouter-* header rows, custom-provider Base URL, model id |
| `Onboarding`        | Gateway URL, X-AutoRouter-Source header, Python quick-call snippet                |
| `Sessions`          | Session id (per row)                                                              |
| `Models`            | Model id (per row, inline button next to the name)                                |
| `Providers`         | Base URL (per provider card, inline button next to the input)                     |
| `Requests`          | Request id and session id in the expanded row detail panel                        |
| `Settings`          | Gateway URL (next to Bind address), data dir path, database path                  |
| `Health`            | Health endpoint URL (`/healthz`) — sourced from the live bind, not hardcoded       |
| `Import / Export`   | Full `config.toml` (block-style "Copy" button next to Download)                    |
| `Logs`              | Each log line (timestamp + level + target + message)                              |
| `Tool profiles`     | Test sandbox result JSON (inline button next to the `<pre>` block)                |
| `Debug`             | Raw request / response JSON (the whole pane has a copy pill)                      |

Adding a new copy target? Use `CopyButton` with a `successMsg` that
describes the value ("Config JSON copied", "Header copied", etc.) —
the toast is what confirms the copy actually reached the clipboard.

---

## Hard rules

These exist because violating them has already shipped broken code


1. **No pairwise protocol conversion.** Every request goes
   `decode → Universal Schema → encode`. Never `OpenAI → Anthropic`.
   Add or extend an adapter instead.
2. **All wire-format code lives in `autorouter-translate`.** The server
   crate dispatches by `ProviderKind`; it never inspects JSON shape.
3. **Every adapter overrides `encode_stream_chunk`.** The default
   returns `None` and produces an invalid SSE payload for clients.
   The trailing sentinel must match the **source** provider:
   `data: [DONE]\n\n` for OpenAI-compat, `event: message_stop\n…` for
   Anthropic.
4. **No `Box::leak`.** Rule names, session labels, and any other
   owned string must stay owned (`String`) or `Cow<'static, str>`.
5. **Routing goes through `SmartRouter` or `IdentityRouter`.** Do not
   hardcode `target = source` in a handler. The router is the only
   place that decides a target.
6. **Secrets come from the secret store.** Resolve `api_key_secret_id`
   (`env:NAME` or stored id) before attaching an `Authorization` or
   `x-api-key` header. Never inline a key into `AppConfig`.
7. **Limits are enforced per-layer.** Use `RequestBodyLimitLayer` for
   `max_body_bytes`. Apply in-handler `tokio::time::timeout` for
   `request_timeout_seconds` and per-chunk `tokio::time::timeout` for
   `stream_idle_timeout_seconds` — a router-level `TimeoutLayer` would
   kill active SSE streams on timeout, so streaming paths use
   per-chunk timeouts instead.
8. **Every gateway route, including `/ui/*`, runs through
   `maybe_authorize()`.** No exceptions, even for "internal" routes.
9. **Settings PATCH must persist.** After mutating the in-memory
   `AppConfig`, serialise to `user_config_path(&paths)` with an atomic
   temp-file + rename. Do not write in place.
10. **Logs come from `tracing`.**
11. **The gateway listener is owned by `GatewaySupervisor`.** The
    `TcpListener` and the running `axum::serve` task are wrapped in
    `autorouter_server::supervisor::GatewaySupervisor`. PATCH
    `/ui/settings` and Tauri `cmd_settings_patch` /
    `cmd_restart` must go through `rebind_if_needed` so changing
    `server.bind` actually moves the listening socket. Never
    capture the bind string into a `let` and spawn the server with
    that captured value - the listener becomes pinned and "Save
server" looks like a no-op. Push events into the
    `UiState.log_lines` buffer via a custom `MakeWriter`; do not
    hand-push four lines and call it done.

---

## The Universal Schema (what every adapter must speak)

Defined in [`crates/autorouter-core/src/model.rs`](crates/autorouter-core/src/model.rs).
The minimum surface an adapter must produce and consume:

| Type           | Fields                                                                                    |
| -------------- | ----------------------------------------------------------------------------------------- |
| `Request`      | `messages`, `system`, `tools`, `tool_choice`, `temperature`, `top_p`, `max_tokens`, `stop`, `stream`, `metadata`, provider extensions. |
| `Message`      | `role` (`system` / `user` / `assistant` / `tool`), `content` (`Vec<ContentBlock>`).        |
| `ContentBlock` | `Text`, `Image { mime, data }`, `Document { mime, data }`, `ToolUse`, `ToolResult`.        |
| `Tool`         | `name`, `description`, `input_schema` (JSON Schema).                                       |
| `ToolCall`     | `id`, `name`, `arguments` (JSON value).                                                   |
| `Usage`        | `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`.               |
| `FinishReason` | `Stop` / `Length` / `ToolCalls` / `ContentFilter` / `Error` / provider extensions.        |
| `StreamChunk`  | `delta` (`TextDelta`, `ReasoningDelta`, `ToolCallDelta`, `UsageDelta`, `Finish`), `index`, `event`. |

Anything an upstream returns that does not fit is wrapped in a
provider extension on `Request.metadata` / `Response.metadata` and
round-tripped losslessly.

---

## Routing model

| Layer                  | Lives in                                                       |
| ---------------------- | -------------------------------------------------------------- |
| Source selection       | `X-AutoRouter-Source` header → which adapter decodes.          |
| Target override        | `X-AutoRouter-Target` header → short-circuit the rule engine.  |
| Default target         | `config.defaults.default_provider` + `default_model`.          |
| Rule engine            | `autorouter_router::RuleEngine::evaluate` → ordered first match. |
| Capability filter      | `CapabilityRegistry::supports(model, requirement)`.           |
| Health penalty         | `HealthTracker::score(provider, model)`.                       |
| Final decision         | `RouteDecision { provider, model, reason: String }`.           |

Rule precedence is **first match wins**. Conflicts between rules are
resolved by listing the more specific rule first. The user can always
override by sending `X-AutoRouter-Target`.

---

## Build, test, benchmark

```sh
cargo test --workspace                 # 290+ tests across all crates
cargo run -p autorouter-desktop        # full GUI (recommended dev loop)
cargo run -p autorouter-app            # headless gateway (no UI)
cargo bench -p autorouter-observability # Criterion: translate + streaming
node scripts/bundle.mjs                # produce platform installers
```

**Performance targets (measured in `autorouter-observability` benches):**

| Metric                       | Target    |
| ---------------------------- | --------- |
| Translation overhead p95     | < 5 ms    |
| Streaming first-byte latency | < 20 ms   |
| Cold-start time              | < 2 s     |
| Resident memory              | < 200 MB  |

---

## Conventions

- **Rust 2021**, MSRV `1.83`. CI uses the pinned `rust-toolchain.toml`
  if present.
- **Errors**: `thiserror` for libraries; `anyhow` only at process
  boundaries (`autorouter-app`, `autorouter-desktop`).
- **Async**: `tokio` everywhere; `#[tokio::main]` only in binaries.
- **HTTP**: `reqwest` with `rustls-tls` (never `native-tls`).
- **Configuration**: every value comes from `AppConfig` via the
  loader. No ad-hoc `std::env::var` outside `autorouter-config`.
- **Tracing**: structured fields only (`tracing::info!(session = %id, model = %model, "translate")`).
  No string interpolation in event messages.
- **Naming**: adapters are unit structs named `<Provider><Format>Adapter`
  (e.g. `AnthropicMessagesAdapter`).
- **Tests**: every adapter has a `decode_*` / `encode_*` unit test
  per round-tripped type, and an SSE-shape test for streaming.

### Reasoning / thinking content (chain-of-thought)

Reasoning content is end-to-end-preserved. The pipeline has THREE
layers of reasoning handling, all in `autorouter-translate`:

1. **Separate-field decode** — `openai_chat` reads
   `delta.reasoning_content` / `message.reasoning_content`,
   `openai_responses` reads `response.reasoning_text.delta` and
   `output[]` `type: "reasoning"`, `anthropic` reads
   `content_block_delta` `type: "thinking_delta"` and
   `type: "thinking"` blocks, `gemini` reads `parts: [{ thought: true }]`.
2. **Inline-tag extraction** — `reasoning_extractor::split_reasoning`
   strips `<think>...</think>` and `<thinking>...</thinking>` blocks
   from the regular `content` field, reclassifying the inside as
   reasoning. The streaming variant `ReasoningStreamer` is a state
   machine that handles tags split across SSE chunks via a carry
   buffer (64 KiB cap; a runaway carry is flushed as plain text to
   bound memory). The streamer is keyed per-request via a static
   `STREAMERS: Mutex<HashMap<usize, ReasoningStreamer>>` in
   `reasoning_extractor.rs`; entries are created lazily on first
   `streamer_feed` and removed on `streamer_finish` (called on
   `StreamEvent::Finish`). Do not call `streamer_drop` from
   production code — it's `#[allow(dead_code)]` and exists only for
   future cleanup paths.
3. **Response-side re-emit** — the four `encode_*_response` helpers
   in `autorouter-server/src/routes.rs` emit `ContentPart::Reasoning`
   in each target wire format. The streaming encoders in
   `autorouter-translate/src/streaming.rs` emit
   `StreamEvent::ReasoningDelta` as `delta.reasoning_content`
   (OpenAI Chat), `content_block_delta` `type: "thinking_delta"`
   (Anthropic), or `parts: [{ text, thought: true }]` (Gemini).

**Do not add reasoning handling outside `autorouter-translate` and
`autorouter-server/src/routes.rs`.** All three layers live in those
two crates by design.

---

## Definition of done (for any change)

- [ ] All affected crates still pass `cargo test --workspace`.
- [ ] If you touched an adapter, its streaming test still asserts the
      wire-format SSE shape and the trailing sentinel.
- [ ] If you touched routing, at least one rule-fixture test in
      `crates/autorouter-router/tests/` covers the new path.
- [ ] `cargo bench -p autorouter-observability` shows no regression
      on the translation or streaming benches.
- [ ] If you added a new env var, it is in `manual.md` §17.
- [ ] If you added a new HTTP route, it is in `README.md` HTTP
      surface and `manual.md` §9.

---

## Pointing AI tools at AutoRouter (cheat sheet)

Set the tool's base URL to `http://127.0.0.1:4073` and one of:

| Tool                  | Base path                          | `X-AutoRouter-Source` |
| --------------------- | ---------------------------------- | --------------------- |
| Claude Code           | `/openai/v1/chat/completions`      | `openai`              |
| Codex                 | `/openai/v1/responses`             | `openai`              |
| Gemini CLI            | `/v1beta/models/*path`             | `gemini`              |
| Continue / Aider / Cline / OpenCode / Warp / Roo Code | same as the tool's own URL | `openai` / `anthropic` |

Optional headers: `X-AutoRouter-Session`, `X-AutoRouter-Label`,
`X-AutoRouter-Target`.

For full per-tool recipes see `manual.md` §7.
