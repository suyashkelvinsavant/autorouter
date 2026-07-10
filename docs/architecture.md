# AutoRouter Architecture

AutoRouter is a local-first desktop gateway that emulates the OpenAI,
Anthropic, and Gemini wire formats on a single endpoint and forwards
each request to an upstream provider via a smart router.

## Crate map

| Crate | Role |
|-------|------|
| `autorouter-core` | Universal Schema: `UniversalRequest`, `UniversalResponse`, `StreamEvent`. |
| `autorouter-translate` | Wire-format adapters (OpenAI Chat, OpenAI Responses, Anthropic Messages, Gemini GenerateContent). |
| `autorouter-config` | TOML loader, secret store, SQLite storage, project paths. |
| `autorouter-router` | `SmartRouter`, `IdentityRouter`, `RuleEngine`, capability registry, `HealthTracker`. |
| `autorouter-server` | Axum gateway, `/ui/*` dashboard routes, auth middleware. |
| `autorouter-observability` | `tracing` setup, Prometheus metrics, rolling backups. |
| `autorouter-desktop` | Tauri 2 shell, system tray, window. |
| `autorouter-app` | Headless binary entry point. |
| `ui/` | React + Vite dashboard. |

## Request flow

1. The client sends an OpenAI/Anthropic/Gemini request to the
   gateway. The shape is parsed by the matching adapter and turned
   into a `UniversalRequest`.
2. The smart router evaluates rules (first-match-wins) and capability
   filters, consults the health tracker, and emits a
   `RouteDecision`.
3. The gateway serialises the `UniversalRequest` to the target
   provider's wire format and calls the matching `HttpUpstream`.
4. The response is decoded back to `UniversalResponse`, then
   re-encoded to the source provider's wire format.

See `README.md §4` for the diagrams and `manual.md §9` for the
full HTTP surface.
