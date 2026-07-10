# AutoRouter — User Manual

A complete, opinionated reference for the AutoRouter project: what it
is, how to install it, how to use every feature of the desktop app,
and how to integrate the headless gateway into your own tooling.

---

## Table of Contents

1. [What is AutoRouter?](#1-what-is-autorouter)
2. [Concepts and Terminology](#2-concepts-and-terminology)
3. [Installation](#3-installation)
   - 3.1 [Windows installer](#31-windows-installer)
   - 3.2 [macOS installer](#32-macos-installer)
   - 3.3 [Linux installer](#33-linux-installer)
   - 3.4 [Headless server (no GUI)](#34-headless-server-no-gui)
   - 3.5 [Building from source](#35-building-from-source)
4. [First Run and Onboarding](#4-first-run-and-onboarding)
5. [The Desktop App — Full Tour](#5-the-desktop-app--full-tour)
   - 5.1 [Top bar, sidebar, and keyboard shortcuts](#51-top-bar-sidebar-and-keyboard-shortcuts)
   - 5.2 [Dashboard page](#52-dashboard-page)
   - 5.3 [Providers page](#53-providers-page)
   - 5.4 [Sessions page](#54-sessions-page)
   - 5.5 [Logs page](#55-logs-page)
   - 5.6 [Settings page](#56-settings-page)
   - 5.7 [System tray and quit behaviour](#57-system-tray-and-quit-behaviour)
6. [Configuring Providers](#6-configuring-providers)
   - 6.1 [OpenAI](#61-openai)
   - 6.2 [Anthropic](#62-anthropic)
   - 6.3 [Gemini](#63-gemini)
   - 6.4 [Custom (OpenAI-compatible) providers](#64-custom-openai-compatible-providers)
   - 6.5 [API key resolution: env vs secret store](#65-api-key-resolution-env-vs-secret-store)
7. [Pointing AI Tools at AutoRouter](#7-pointing-ai-tools-at-autorouter)
   - 7.1 [Quick start: the four protocol endpoints](#71-quick-start-the-four-protocol-endpoints)
   - 7.2 [The X-AutoRouter-* headers](#72-the-x-autorouter--headers)
   - 7.3 [Per-tool configuration recipes](#73-per-tool-configuration-recipes)
   - 7.4 [Streaming responses](#74-streaming-responses)
   - 7.5 [Authentication](#75-authentication)
8. [Configuration File Reference](#8-configuration-file-reference)
9. [HTTP API Reference](#9-http-api-reference)
   - 9.1 [Gateway endpoints](#91-gateway-endpoints)
   - 9.2 [Dashboard / UI endpoints](#92-dashboard--ui-endpoints)
   - 9.3 [Health and metrics](#93-health-and-metrics)
10. [Translation Behaviour and Limitations](#10-translation-behaviour-and-limitations)
11. [Logging, Metrics, and Observability](#11-logging-metrics-and-observability)
12. [Data, Storage, and Backups](#12-data-storage-and-backups)
13. [Security Model](#13-security-model)
14. [Routing Engine](#14-routing-engine)
15. [Troubleshooting](#15-troubleshooting)
16. [Frequently Asked Questions](#16-frequently-asked-questions)
17. [Reference: Environment Variables](#17-reference-environment-variables)
18. [Glossary](#18-glossary)
19. [Getting Help](#19-getting-help)

---

## 1. What is AutoRouter?

AutoRouter is a local-first desktop application that sits between your
AI tools (Claude Code, Codex, Gemini CLI, Continue, Aider, Cline,
OpenCode, Warp, Antigravity, Roo Code, Cursor, and any other tool that
speaks one of the major wire formats) and the upstream AI providers
(OpenAI, Anthropic, Gemini, and any OpenAI-compatible endpoint).

The problem it solves: every AI tool expects a single, fixed API
protocol. If you write a script against the OpenAI Chat Completions
format, you cannot point it at Anthropic or Gemini without rewriting
the wire layer. AutoRouter removes that constraint by emulating every
major protocol on a single local endpoint and translating between them
on the fly.

The flagship user experience is the Tauri-based desktop app
(`autorouter-desktop`), which:

* Runs a local HTTP gateway on `127.0.0.1:4073` (configurable) that
  exposes OpenAI Chat Completions, OpenAI Responses, Anthropic
  Messages, and Gemini `generateContent` simultaneously.
* Translates requests between formats at runtime, so an OpenAI-shaped
  client can hit an Anthropic upstream (and vice versa) with zero
  changes to the client.
* Provides a live dashboard, a system tray, and per-platform
  installers for Windows, macOS, and Linux.
* Stores API keys in a configurable secret store (OS keychain by
  default, with environment variable fallbacks).
* Ships a smart routing engine that picks the best upstream for each
  request based on rules, model capabilities, and live health.

If you only need the protocol-translation gateway without the GUI, a
headless `autorouter-app` binary is also produced.

---

## 2. Concepts and Terminology

Before going further, here are the concepts used throughout the rest
of the manual.

* **Source provider** — the protocol the *client tool* is using to
  talk to AutoRouter. Set via the `X-AutoRouter-Source` request
  header. Possible values: `openai`, `anthropic`, `gemini`. Defaults
  to `openai` if the header is missing.
* **Target provider / target model** — the upstream AutoRouter
  forwards the request to. By default AutoRouter uses an "identity"
  router that preserves the source provider, so a request that comes
  in as OpenAI is forwarded to the OpenAI upstream. Rules can change
  this; see [§14 Routing Engine](#14-routing-engine).
* **Session** — a logical group of requests identified by the
  `X-AutoRouter-Session` request header. If the header is missing
  AutoRouter generates a UUID per request.
* **Label** — an optional human-readable name for a session, set via
  the `X-AutoRouter-Label` header (e.g. "claude-code", "codex").
* **Adapter** — the Rust code that knows how to parse and emit a
  specific protocol. There are four built-in adapters: OpenAI Chat
  Completions, OpenAI Responses, Anthropic Messages, and Gemini
  `generateContent`.
* **Pipeline** — the collection of adapters. AutoRouter picks the
  right one from the `X-AutoRouter-Source` header and uses another
  one to emit the upstream wire format.
* **Smart router** — the rule + capability + health engine that
  decides which upstream receives a given request. In this version it
  ships as the `SmartRouter` Rust type and is used by the headless
  binary; the desktop shell exposes the same logic through the
  Settings page.
* **Loopback bind** — the default is `127.0.0.1:4073`, which means
  AutoRouter only accepts connections from your own machine. This is
  intentional: AI tool traffic stays on the device, and API keys are
  never sent over the network except to the configured upstream
  provider.
* **Provider entry** — a TOML configuration block that tells
  AutoRouter how to reach a single upstream: display name, base URL,
  API key reference, default headers, and a model allowlist.
* **Custom provider** — any OpenAI-compatible endpoint (Together,
  Groq, Fireworks, llama.cpp, OpenRouter, etc.) configured
  under `providers.custom.<name>` in the config file.
* **Secret store** — the local storage for API keys. The default is
  the OS keychain via the `keyring` crate. AutoRouter also supports
  an in-memory store and a JSON file with restricted permissions.
* **Dashboard** — the embedded web UI in the desktop app. It is
  served by the same `autorouter-server` HTTP gateway, on the
  `/ui/*` routes, and rendered in a Tauri webview.

---


## 3. Installation

### 3.1 Windows installer

Two installer formats are produced. Both install the same files; pick
the one that matches your deployment context.

* **NSIS** - `AutoRouter_0.1.0_x64-setup.exe`. A small
  per-machine setup binary. Right-click -> "Run as administrator" if
  you want a per-machine install (the default). Use this for
  individual machines.
* **MSI** - `AutoRouter_0.1.0_x64_en-US.msi`. A WiX-generated
  Windows Installer package. Use this for Group Policy deployment
  (`msiexec /i AutoRouter_0.1.0_x64_en-US.msi /qn`), SCCM, Intune,
  or scripted installs.

Steps for a normal install:

1. Double-click `AutoRouter_0.1.0_x64-setup.exe`.
2. Accept the license (MIT, see `LICENSE`).
3. Choose the install location. The default is
   `%LocalAppData%\Programs\AutoRouter` for per-user installs.
4. The installer embeds the WebView2 bootstrapper, so end users do
   not need to install WebView2 separately.
5. Click "Install" and wait for the progress bar to finish.
6. Launch AutoRouter from the Start menu or the desktop shortcut.

The application then appears in the system tray (a small "A" icon
in the notification area). The main window opens automatically on
first launch.

To uninstall: Settings -> Apps -> Installed apps -> AutoRouter ->
Uninstall, or run the installer again and choose "Remove".

### 3.2 macOS installer

Two artifacts are produced:

* `AutoRouter.app` - the application bundle. Drag it into
  `/Applications`.
* `AutoRouter_0.1.0_aarch64.dmg` (and the matching
  `x86_64` DMG) - a disk image that contains the `.app` plus a
  shortcut to `/Applications`. Double-click the DMG, drag the app
  to the Applications folder shortcut, and eject.

Steps for a normal install:

1. Download the DMG that matches your Mac (Apple silicon for
   `aarch64`, Intel for `x86_64`).
2. Open the DMG; a Finder window appears with `AutoRouter.app` and
   an `Applications` shortcut.
3. Drag `AutoRouter.app` to `/Applications`.
4. Eject the DMG.
5. Open Launchpad or Spotlight and search for AutoRouter. The first
   launch may show a Gatekeeper prompt because the app is not
   notarized by default - right-click the app in Finder and choose
   "Open", then confirm.

The first launch creates:

* `~/Library/Application Support/autorouter/` - the data
  directory (database, backups).
* `~/Library/Logs/autorouter/` - log files.
* `~/Library/Preferences/autorouter/` - config file.

The app appears in the menu bar (top right of the screen) with a
small "A" icon. The main window opens on first launch.

To uninstall: drag `AutoRouter.app` from `/Applications` to the
Trash, then delete the three directories listed above.

To enable notarization for redistribution, set
`bundle.macOS.signingIdentity`, `APPLE_ID`, `APPLE_PASSWORD`, and
`APPLE_TEAM_ID` before running `node scripts/bundle.mjs`, then
submit the signed bundle to Apple with
`xcrun notarytool submit`.

### 3.3 Linux installer

Two artifacts are produced:

* `autorouter_0.1.0_amd64.deb` - a Debian / Ubuntu package.
* `autorouter_0.1.0_amd64.AppImage` - a portable single-file
  executable that does not require installation.

Steps for a Debian / Ubuntu install:

```sh
sudo apt install ./autorouter_0.1.0_amd64.deb
```

Steps for a portable install (any distro):

```sh
chmod +x autorouter_0.1.0_amd64.AppImage
./autorouter_0.1.0_amd64.AppImage
```

The AppImage runs without installing anything. To get a desktop
icon and menu entry, move the AppImage to `~/.local/bin/` and
add a `.desktop` file under `~/.local/share/applications/`.

AutoRouter requires the following system libraries (already
present on most modern Linux desktops):

* `webkit2gtk-4.1`
* `gtk-3`
* `libayatana-appindicator3` (for the tray icon)
* `librsvg2`
* `libsoup-3.0`
* `libjavascriptcoregtk-4.1`

If you are running a minimal install, install the dev packages
with `apt` (the GitHub Actions workflow shows the exact list).

### 3.4 Headless server (no GUI)

If you do not want a desktop UI - for example on a remote server
or in a Docker container - you can run the headless
`autorouter-app` binary instead. It exposes the same HTTP API
without the Tauri shell or the dashboard.

```sh
cargo run -p autorouter-app --release
```

Configuration is the same as for the desktop app (see
[§8 Configuration File Reference](#8-configuration-file-reference)
and [§17 Environment Variables](#17-reference-environment-variables)).
The `AUTOROUTER_BIND` env var is the most useful: it overrides the
bind address without touching any file.

### 3.5 Building from source

If you have a Rust toolchain (1.83 or newer) and Node.js 20+,
you can build everything from source:

```sh
# Clone the repository URL shown on its project page
git clone <repository-url>
cd autorouter

# Run the workspace tests (83+ tests, takes ~30s on a modern laptop)
cargo test --workspace

# Build the headless binary
cargo build --release -p autorouter-app

# Build the desktop shell (produces a runnable .exe / .app / binary)
cargo build --release -p autorouter-desktop

# Build platform installers
npm install --prefix scripts @tauri-apps/cli@^2
node scripts/bundle.mjs
```

Outputs:

* `target/release/autorouter-app` - headless server.
* `target/release/autorouter-desktop` - runnable desktop binary.
* `target/release/bundle/{msi,nsis,macos,dmg,deb,appimage}/*` -
  the installers, depending on the host platform.

---
## 4. First Run and Onboarding

When you launch AutoRouter for the first time:

1. The gateway binds to `127.0.0.1:4073` (override with
   `AUTOROUTER_BIND=127.0.0.1:9000` etc.). The bind address is
   shown in the dashboard's top bar.
2. The main window opens. The dashboard greets you with an
   "Onboarding" panel if the gateway could not be reached (this
   can happen on first launch while dependencies settle).
3. No providers are configured. The Providers page shows three
   "Not configured" cards for OpenAI, Anthropic, and Gemini.
4. No sessions are active. The Sessions page is empty.

To get from a fresh install to a working setup, the standard
sequence is:

1. Open the **Settings** page. Confirm the bind address and any
   other server settings you want to change.
2. Open the **Providers** page. For each provider you want to
   use, fill in the base URL (the default is correct for the
   public endpoints), paste an API key reference, and click
   **Save**.
3. The gateway does not need to be restarted after a config
   change. The Settings page writes are picked up by the running
   gateway immediately.
4. Configure your AI tool to use `http://127.0.0.1:4073` as the
   base URL, and set the `X-AutoRouter-Source` header on each
   request.
5. Send a test request. It appears on the **Sessions** page with
   a request count of 1. The response is forwarded to the
   configured upstream.

---

## 5. The Desktop App - Full Tour

### 5.1 Top bar, sidebar, and keyboard shortcuts

**Top bar (header):**

* **Brand mark** - the violet->magenta "A" gradient logo on the
  left. Sits next to the wordmark "AutoRouter" and a small
  "DESKTOP" pill badge.
* **Status chip** - a green pulsing dot with the gateway version
  and bind address (e.g. `v0.1.0 · localhost:4073`). When the
  gateway is unreachable the chip shows a grey dot and the text
  "connecting...".
* **Theme toggle** - a sun/moon icon button in the top-right.
  Cycles between dark, light, and auto (follows OS preference).
  The choice is persisted in `localStorage`.

**Sidebar (left rail):**

* Fourteen page items, each with a monoline SVG icon and a label:
  Dashboard, Providers, Models, Sessions, Routing, Health,
  Requests, Analytics, Debug, Tool profiles, Import / Export,
  Update, Logs, Settings.
* Hover any item to reveal a keyboard hint chip on the right
  (e.g. `^1`).
* The active page is highlighted with a subtle accent box and a
  filled accent dot.
* At the bottom, after a flexible spacer, is the **Quit** item
  (red quit icon) - clicking it calls the Tauri `quit_app`
  command, which closes the gateway and the window.

**Keyboard shortcuts:**

| Shortcut         | Action              |
| ---------------- | ------------------- |
| `Ctrl/Cmd + 1`   | Open Dashboard      |
| `Ctrl/Cmd + 2`   | Open Providers      |
| `Ctrl/Cmd + 3`   | Open Models         |
| `Ctrl/Cmd + 4`   | Open Sessions       |
| `Ctrl/Cmd + 5`   | Open Routing        |
| `Ctrl/Cmd + 6`   | Open Health         |
| `Ctrl/Cmd + 7`   | Open Requests       |
| `Ctrl/Cmd + 8`   | Open Analytics      |
| `Ctrl/Cmd + 9`   | Open Debug          |
| `Ctrl/Cmd + 0`   | Open Tool profiles  |
| `Ctrl/Cmd + L`   | Open Logs           |
| `Ctrl/Cmd + ,`   | Open Settings       |
| `Ctrl/Cmd + R`   | Refresh status now  |

The current page is persisted in `localStorage` under
`autorouter:page` and is also reflected in the URL as
`?page=dashboard|providers|models|sessions|routing|health|requests|analytics|debug|tool-profiles|import-export|update|logs|settings`. Both are
updated whenever you switch pages, so you can deep-link from an
external tool, a terminal, or a documentation link.

### 5.2 Dashboard page

The dashboard is the home page of the desktop app — a control
center for connecting AI tools and adding custom providers.
Everything that can be copied has a one-click copy button, and
every action shows a toast confirmation. The page is divided into
five sections, top to bottom.

**A. Hero strip.** A wide banner at the top of the page showing
the local gateway endpoint as a large monospace URL
(`http://127.0.0.1:4073` by default). To the right of the URL is
an inline `Copy` button — click it to copy the URL to the
clipboard, and a green "Endpoint copied" toast appears at the
bottom-right for 2 seconds. The same URL appears as a clickable
"Bind" card in the status grid below (the whole card is the copy
target). On the far right of the hero strip is an **Open in
browser** button that launches the gateway UI in the system
browser via Tauri's opener plugin (it falls back to
`window.open` in `vite dev`). A small status pill shows
"Online · v0.1.0" (green) or "Reconnecting…" (amber) when the
gateway is unreachable.

**B. Status grid.** Four cards:

| Card | Value | Notes |
| --- | --- | --- |
| **Status** | "Online" or "Offline" badge | Version shown below as "0.1.0 · gateway live" |
| **Bind** | `127.0.0.1:4073` | Click anywhere on the card to copy the URL |
| **Uptime** | e.g. `2h 17m 4s` | Reset to 0s on app start |
| **Active sessions** | integer | Distinct session IDs seen since startup |

The grid auto-refreshes every 5 seconds with the latest values
from the gateway. Force a refresh with `Ctrl/Cmd + R`.

**C. Connect your tools.** A 3-column card grid (2 columns on
tablet, 1 on phone). One card per supported AI tool, each with a
title, a one-line tagline, and a tinted `CodeBlock` containing the
copy-pasteable snippet for that tool. A `Copy` button is embedded
in the top-right of every code block. The cards are:

| Card | Snippet kind | What gets copied |
| --- | --- | --- |
| **Claude Code** | `sh` | `export ANTHROPIC_BASE_URL=http://…:4073` + invocation |
| **Codex CLI** | `toml` | The `[model_providers.autorouter]` block for `~/.codex/config.toml` |
| **Gemini CLI** | `sh` | `export GOOGLE_GENAI_API_BASE=http://…:4073` |
| **OpenCode** | `json` | The `{"providers":{"autorouter":{…}}}` block for `~/.config/opencode/config.json` |
| **Aider** | `sh` | `OPENAI_API_BASE` + `OPENAI_API_KEY` env vars |
| **Continue / Cline / Roo Code** | `sh` | Same OpenAI env vars (all three read them) |
| **Warp** | `sh` | Same OpenAI env vars |
| **Generic OpenAI client (Python)** | `python` | Full `from openai import OpenAI(…)` snippet with `default_headers={"X-AutoRouter-Source":"openai"}` |

The snippets are templated on the live `status.bind` value — if
you move the gateway to a different port (via `AUTOROUTER_BIND` or
the Settings page), the snippets update automatically on the next
status poll.

Below the tool grid is a **Headers you'll need** panel: four rows
listing `X-AutoRouter-Source`, `X-AutoRouter-Target`,
`X-AutoRouter-Session`, `X-AutoRouter-Label`, each with its own
copy button. The copy payload is `Name: value` (e.g.
`X-AutoRouter-Source: openai | anthropic | gemini`), so you can
paste it directly into an HTTP client that takes a single
"Header: value" string.

For the full per-tool recipes (with the exact file paths and
config-file shapes), see
[§7.3 Per-tool configuration recipes](#73-per-tool-configuration-recipes).

**D. Add a custom provider.** An inline form on the dashboard
itself, with the same fields as the Providers page but compressed
into a single card so you do not have to navigate away:

* **Provider ID** — lowercase letters, numbers, hyphens,
  underscores. Auto-derived from the Display name until you edit
  the field. Cannot contain spaces.
* **Display name** — defaults to the Provider ID if left blank.
* **Base URL** — must start with `http://` or `https://`. A
  **Presets** dropdown (15 popular providers: OpenAI, Anthropic,
  Gemini, OpenRouter, Groq, Together, Mistral, DeepSeek,
  Perplexity, xAI, Cohere, Fireworks, Anyscale, Ollama, LM
  Studio) one-click-fills the field.
* **API key** — optional. Leave empty if the upstream does not
  need auth. If you type a literal key (e.g. `sk-or-v1-…`), the
  form pushes it into the secret store under `<id>_api_key` and
  records the canonical `keychain:<id>_api_key` reference. If you
  type `env:NAME`, the form records that verbatim. Use the
  **Show / Hide** toggle to verify what you typed.
* **Models** — chip input. Type a model id and press **Enter** (or
  `,`) to add it as a chip. Click the `×` on a chip to remove it.
  Leave the list empty to allow all models the upstream
  advertises.

The form has two action buttons:

* **Test** — calls `api.providerTest` with the first allow-listed
  model and shows the result inline (HTTP status + latency, or
  the error message). The test runs even before the provider is
  saved, so you can validate a new upstream before committing it
  to the config.
* **Save** — writes the provider through `PATCH /ui/settings`
  and, on success, fires a "Provider saved" toast and navigates
  to the full Providers page. If the id already exists, the
  button arms a "click again to overwrite" confirmation.

Validation: the **Save** button is disabled until the id and
base URL are both valid. The **Test** button is enabled whenever
the form is valid; the request is the same shape that a real
client would send, so a successful test is a strong signal that
the provider will work in production.

**E. Live activity.** Two compact lists at the bottom of the
page:

* **Recent sessions** — the three most recent session ids
  observed by the gateway, with label, request count, and
  "Xs ago". Click a row to jump to the full Sessions page.
* **Recent requests** — same shape, sourced from the Requests
  page. Shows source → target, HTTP status, latency.

The lists auto-refresh every 5 seconds and are hidden entirely
when the corresponding endpoint returns no data, so a fresh
install does not show empty placeholders.

**One-click copy in practice.** Every copyable element uses the
shared `CopyButton` component. Click → `navigator.clipboard.writeText`
runs in a `try/catch` → on success a green toast ("Endpoint
copied", "Snippet copied", "Header copied", "Session id
copied", "Config JSON copied", "Line copied", etc.) slides in
at the bottom-right for 2 seconds → the button briefly flashes
green. If the clipboard write fails (rare — Tauri WebView2 on
Windows sometimes denies non-secure contexts), a red "Copy
failed" toast appears instead. The button never blocks the UI
on the clipboard; the value remains visible on screen so the
user can still copy manually.

### 5.3 Providers page

Lists every provider the gateway knows about and lets you edit
their settings. There are two sub-sections: the three built-in
provider cards (OpenAI, Anthropic, Gemini), and a Models table.

**Provider card fields:**

* **Display name** - what the UI shows in the provider list.
  Defaults to the provider id (e.g. "OpenAI").
* **Base URL** - the upstream root URL. An inline **Copy**
  button next to the input copies the URL so it can be pasted
  into an AI tool's base URL field. Defaults:
  * OpenAI -> `https://api.openai.com/v1`
  * Anthropic -> `https://api.anthropic.com`
  * Gemini -> `https://generativelanguage.googleapis.com`
* **API key secret id** - the secret reference the gateway uses
  to find the API key. Two formats are supported:
  * `env:NAME` - read the value from the environment variable
    `NAME` (e.g. `env:OPENAI_API_KEY`).
  * A plain id (e.g. `openai-prod`) - look up the value in the
    secret store. See [§6.5](#65-api-key-resolution-env-vs-secret-store)
    for the resolution order.
* **Model allowlist (comma separated)** - the list of model ids
  this provider is allowed to serve. An empty list means "all
  models the upstream advertises". A non-empty list restricts the
  provider to only those model ids.
* **Enabled** - checkbox. Disabled providers still appear in
  lists but are skipped by the router.

Click **Save** to push the change. The save is hot - the running
gateway picks it up without a restart.

The "Not configured" empty state shows when a provider has no
entry at all. Click into a card and fill in at least the base URL
and an API key reference to enable it.

**Models table:**

A wide table showing every model the gateway has discovered
across all enabled providers. Columns:

| Column        | Meaning                                                  |
| ------------- | -------------------------------------------------------- |
| ID            | The model id as the upstream calls it (e.g. `gpt-5`). An inline **Copy** button next to the id copies the literal string for use in tool configs. |
| Provider      | The owning provider (`openai`, `anthropic`, `gemini`).   |
| Context       | Maximum context window in tokens.                        |
| Max out       | Maximum output tokens per response.                      |
| Tools         | yes if the model supports tool use.                      |
| Vision        | yes if the model can accept image inputs.                |
| Audio         | yes if the model can accept audio inputs.                |
| Stream        | yes if the model supports streaming responses.           |

The model list is sourced from the adapter's static
`models()` list, which is a snapshot of the well-known models
each provider offers. If a new model is released, you can
manually add it to a provider's `model_allowlist` to use it
even if it is not yet in the list.

### 5.4 Sessions page

Lists every distinct session the gateway has observed. The page
auto-refreshes every 3 seconds.

A session is identified by the `X-AutoRouter-Session` request
header. The first time the gateway sees a new id, a row appears
in the table. The table shows:

* **ID** - the session UUID, truncated to `abcdef12...ef34` for
  readability. Hover for the full id. An inline **Copy** button
  copies the full id to the clipboard.
* **Label** - the value of the optional `X-AutoRouter-Label`
  header (e.g. "claude-code"). Shows "-" if no label was sent.
* **Source** - the source provider as a coloured badge.
* **Requests** - the number of requests seen on this session.
* **Last request** - the relative time of the most recent
  request (e.g. "12s ago").
* **Created** - the local time the session was first seen.

The empty state gives a clear next step:

> No sessions yet. Connect an AI tool (Claude Code, Codex, Aider...)
> pointing at the local endpoint to see it here.

### 5.5 Logs page

A live tail of the gateway's in-process log lines. Updates once
per second.

**Toolbar:**

* **Level dropdown** - `all`, `debug`, `info`, `warn`, `error`.
  Filters lines whose level is at or above the selected one.
* **Filter input** - a free-text filter on the message body and
  the log target. The filter is applied client-side; it does not
  affect what the gateway stores.
* **Level counts** - four small pills showing the number of
  `debug` / `info` / `warn` / `error` lines currently in the
  buffer. They update with each fetch.
* **Pause / Resume** - stop the tail from growing (useful for
  inspecting a specific state).
* **Clear** - empty the in-memory log buffer (the gateway keeps
  its own tail; this only clears the UI display).

**Each line shows:**

* The timestamp in `HH:MM:SS.mmm` form.
* The log level, colour-coded (info blue, warn amber, error red,
  debug dim).
* The tracing target (e.g. `gateway`, `ui`, `desktop`).
* The message.
* A small copy icon on hover. Click to copy the full line
  (timestamp + level + target + message) to the clipboard.

The gateway keeps the last 2000 log lines in memory; older lines
are dropped. For a full audit trail, configure a log file
(`AUTOROUTER_LOG_FILE` or the `logging.file` config field).

### 5.6 Settings page

Hot-reloadable configuration. Every change is written through the
`/ui/settings` PATCH endpoint and applied to the running gateway
without a restart.

**Action bar (top):**

* **Reload** - re-fetch the current settings from the gateway.
* **Restart server** - emit a `/ui/restart` request. The headless
  binary reboots; the desktop shell shows a "Restart requested"
  confirmation. (Restarting the in-process desktop gateway
  requires an app relaunch - use the tray menu's "Quit" then
  relaunch the app for a full restart.)
* **Reveal data dir** - opens the configuration directory in
  the OS file manager (Explorer / Finder / Nautilus).

**Server card:**

* **Bind address** - the loopback address the gateway listens
  on. An inline **Copy** button next to the input copies the
  full gateway URL (`http://<bind>`) so it can be pasted
  directly into an AI tool's base URL field. Changing this is
  honoured on the next restart of the binary, not the
  in-process gateway.
* **Max body size (bytes)** - the request body limit. Defaults
  to 16 MB. A request that exceeds this is rejected with
  `413 Payload Too Large`.
* **Request timeout (seconds)** - the maximum total request
  time, including translation and upstream forwarding. Default
  300s.
* **Enable CORS** - adds a permissive CORS layer. The desktop
  app always enables CORS for the local webview; turn this off
  only if you also disable the webview-based UI.
* **Require auth token** - if on, every gateway request must
  carry `Authorization: Bearer <token>`. The token is checked
  against `auth_token`.
* **Auth token** - the shared secret used by the bearer auth
  check. Treat it like any other API key.

**Defaults card:**

* **Default model** - the model id AutoRouter uses when a
  request does not specify one.
* **Default provider** - the provider id AutoRouter uses when
  a request does not specify one.
* **Max total tokens** - the request context budget. The
  translation layer uses this to clip message history.
* **Stream by default** - if on, responses are streamed unless
  the client sets `stream: false` in the request body.

**Logging card:**

* **Level** - the tracing filter (`trace`, `debug`, `info`,
  `warn`, `error`).
* **JSON format** - emit structured JSON log lines instead of
  the human-readable form.

**Storage card** (read-only):

* **Data dir** - the local directory AutoRouter uses for the
  SQLite database and backups. An inline **Copy** button copies
  the absolute path. Empty means the OS default (see
  [§12 Data, Storage, and Backups](#12-data-storage-and-backups)).
* **Database** - the database filename (default
  `autorouter.db`). An inline **Copy** button copies the
  absolute path.
* **Backup on shutdown** - if on, the database is snapshotted
  to `<data_dir>/backups/autorouter.db.<timestamp>` on every
  clean shutdown.
* **Backups kept** - the maximum number of historical backups
  to retain. Older backups are pruned.

### 5.7 System tray and quit behaviour

The system tray icon (notification area on Windows, menu bar on
macOS, top-bar on Linux) is the gateway's "always-on" presence.
The tray menu has three items:

* **Open Dashboard** - show and focus the main window.
* **Open Logs** - raise the window and switch to the Logs page.
* **Quit AutoRouter** - close the gateway and exit.

Closing the main window with the X (or `Cmd+W` on macOS) **hides**
the window to the tray; the gateway keeps running. To stop the
gateway, use the tray menu's "Quit AutoRouter" item, or pick
"Quit" from the sidebar in the app.

This is intentional: AI tools keep making requests to the
gateway even when the dashboard window is not visible. The
window is a viewer, not a controller.

---
## 6. Configuring Providers

AutoRouter ships with three built-in providers (OpenAI, Anthropic,
Gemini) and supports any number of custom OpenAI-compatible
endpoints under `providers.custom.<name>`.

### 6.1 OpenAI

* **Base URL** - `https://api.openai.com/v1` (default).
* **API key** - get one at <https://platform.openai.com/api-keys>.
* **Recommended env var** - `OPENAI_API_KEY`.
* **Settings entry** - set
  `api_key_secret_id = "env:OPENAI_API_KEY"`.

Example config snippet:

```toml
[providers.openai]
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key_secret_id = "env:OPENAI_API_KEY"
enabled = true
```

### 6.2 Anthropic

* **Base URL** - `https://api.anthropic.com` (default).
* **API key** - get one at <https://console.anthropic.com/>.
* **Recommended env var** - `ANTHROPIC_API_KEY`.

```toml
[providers.anthropic]
display_name = "Anthropic"
base_url = "https://api.anthropic.com"
api_key_secret_id = "env:ANTHROPIC_API_KEY"
enabled = true
```

### 6.3 Gemini

* **Base URL** - `https://generativelanguage.googleapis.com`
  (default).
* **API key** - get one at
  <https://aistudio.google.com/app/apikey>.
* **Recommended env var** - `GEMINI_API_KEY` or
  `GOOGLE_API_KEY`.

```toml
[providers.gemini]
display_name = "Gemini"
base_url = "https://generativelanguage.googleapis.com"
api_key_secret_id = "env:GEMINI_API_KEY"
enabled = true
```

### 6.4 Custom (OpenAI-compatible) providers

Any endpoint that implements the OpenAI Chat Completions or
OpenAI Responses shape can be wired in as a custom provider.
Examples: Together, Groq, Fireworks, OpenRouter, llama.cpp's
local server, LM Studio, vLLM, Ollama's OpenAI-compat mode.

```toml
[providers.custom.groq]
display_name = "Groq"
base_url = "https://api.groq.com/openai/v1"
api_key_secret_id = "env:GROQ_API_KEY"
enabled = true
model_allowlist = ["llama-3.1-70b-versatile", "llama-3.1-8b-instant"]

[providers.custom.local-llama]
display_name = "Local llama.cpp"
base_url = "http://127.0.0.1:8081/v1"
api_key_secret_id = "env:EMPTY"   # the upstream ignores the header
enabled = true
```

The custom provider is referenced by its key (e.g. `groq`) when
targeting it through routing rules or the `X-AutoRouter-Target`
header. See [§14 Routing Engine](#14-routing-engine).

### 6.5 API key resolution: env vs secret store

When AutoRouter needs an API key for a request, it resolves
`api_key_secret_id` in the following order:

1. **Explicit prefix.** If the value starts with `env:`, the
   remainder is used as the name of an environment variable to
   read. If the value starts with `keychain:`, the remainder is
   used as a secret-store id.
2. **Bare id (no prefix).** The bare value is first looked up in
   the secret store. If that misses **and** the value looks like
   an environment variable name (`ALL_CAPS_SNAKE_CASE` — an
   ASCII letter followed by ASCII letters, digits, and
   underscores), it is then resolved from `std::env`. This means
   you can write either `env:OPENROUTER_API_KEY` or just
   `OPENROUTER_API_KEY` and both work.
3. If neither yields a value, the request fails with
   `upstream_key_missing`.

When you save a provider through the dashboard's **Add key**
form, AutoRouter runs the same logic on the value you type:

* A value that **matches a name already in the environment** is
  saved as `env:NAME` (no secret-store entry is written — the
  value lives only in your shell).
* A value that does **not** match an env var name (a vendor
  prefix like `sk-or-v1-…` or a long mixed-case string) is
  written to the secret store under the id
  `<provider_id>_api_key` and recorded as `keychain:…`.
* If you also type a separate `api_key_secret_id`, that id is
  classified first. `env:NAME` and `keychain:ID` are honoured
  verbatim; any other string is treated as a secret-store id and
  the typed value is stored under it.

This means you can usually just paste your raw key (e.g.
`sk-or-v1-53f7c1…`) into the dashboard and let AutoRouter decide
where to put it.

The default secret store is the OS keychain (Windows Credential
Manager / macOS Keychain / Linux Secret Service via the `keyring`
crate). The Desktop binary uses the keychain transparently; no
manual setup is required.

To override the keychain with a JSON file, set
`AUTOROUTER_SECRET_STORE=file` and `AUTOROUTER_SECRET_FILE=...`.
The file is created with `0600` permissions on Unix.

---

## 7. Pointing AI Tools at AutoRouter

### 7.1 Quick start: the four protocol endpoints

AutoRouter exposes the same four endpoints a public AI provider
would. Pick the endpoint that matches the protocol your tool
speaks.

| Tool protocol              | Endpoint                                                    |
| -------------------------- | ----------------------------------------------------------- |
| OpenAI Chat Completions    | `POST http://127.0.0.1:4073/v1/chat/completions`            |
| OpenAI Responses           | `POST http://127.0.0.1:4073/v1/responses`                   |
| Anthropic Messages         | `POST http://127.0.0.1:4073/v1/messages`                    |
| Gemini generateContent     | `POST http://127.0.0.1:4073/v1beta/models/<m>:generateContent` |

> [!NOTE]
> The legacy endpoints `/openai/v1/chat/completions` and `/openai/v1/responses` remain fully supported for backward compatibility with existing tools and configurations.

`Content-Type: application/json` is required for all endpoints.

### 7.2 The X-AutoRouter-* headers

Three optional headers affect routing and session correlation:

* `X-AutoRouter-Source: openai|anthropic|gemini` - declares the
  protocol the *client* is using. Defaults to `openai` if
  missing. AutoRouter uses this to pick the inbound adapter.
* `X-AutoRouter-Target: openai|anthropic|gemini|<custom>` -
  forces a specific upstream provider. By default the identity
  router preserves the source, so an OpenAI request goes to the
  OpenAI upstream. Setting this header to `anthropic` makes
  AutoRouter translate an OpenAI request to Anthropic wire format
  and forward to the Anthropic upstream. Useful for testing
  translations without changing the client.
* `X-AutoRouter-Session: <uuid>` - group multiple requests
  into one session. AutoRouter creates a session row the first
  time it sees a new id; subsequent requests with the same id
  update the same row. If the header is missing a UUID is
  generated per request.
* `X-AutoRouter-Label: <name>` - human-readable label for the
  session. Shown in the dashboard's Sessions page.

### 7.3 Per-tool configuration recipes

The exact incantation differs per tool, but the pattern is the
same: change the base URL to `http://127.0.0.1:4073` and add
`X-AutoRouter-Source` as a default header.

**Claude Code** - set the `ANTHROPIC_BASE_URL` environment
variable:

```sh
export ANTHROPIC_BASE_URL=http://127.0.0.1:4073
claude-code
```

The Anthropic Messages endpoint on the gateway is the same shape
Claude Code expects, so this works without any other config.

**Codex CLI** - Codex reads `~/.codex/config.toml`. Add a
custom provider:

```toml
[model_providers.autorouter]
name = "AutoRouter (local)"
base_url = "http://127.0.0.1:4073/openai/v1"
api_key = "any-non-empty-string"   # the gateway ignores the key
```

Then set `model_provider = "autorouter"` in the same file. The
gateway authenticates the upstream with your real OpenAI key, not
the placeholder here.

> **Codex bridge behavior.** When routing Codex's Responses API
> traffic to a chat-style upstream, the gateway:
>
> - **Canonicalizes tool names.** Upstreams sometimes emit the
>   alias `shell`, which Codex rejects ("unsupported call: shell").
>   The gateway rewrites it to `exec_command` so the call
>   round-trips. It also normalizes the argument key between
>   `command` (upstream contract) and `cmd` (Codex's
>   `exec_command` schema) on the way in and out.
> - **Relays tool results.** Codex sends each
>   `function_call_output` as a separate `/v1/responses` request.
>   The gateway pairs it with the originating assistant `tool_call`
>   before forwarding, so chat upstreams don't reject an orphan
>   `tool` message.
> - **Guards against runaway loops.** A per-run guard normalizes
>   each shell command to a stable "intent family" and stops
>   relaying further calls once the same family repeats past a
>   threshold, returning a truthful completion instead. This
>   catches a model stuck re-issuing a hanging/interactive command
>   (e.g. a scaffolder prompting with no TTY) without suppressing
>   legitimate multi-step iteration, because a genuinely different
>   subsequent command resets the prior family's counters.

**Gemini CLI** - set `GOOGLE_GENAI_API_BASE`:

```sh
export GOOGLE_GENAI_API_BASE=http://127.0.0.1:4073
```

The CLI expects the Gemini v1beta URL; the gateway serves the
same shape.

**OpenCode** - `~/.config/opencode/config.json`:

```json
{
  "provider": {
    "autorouter": {
      "name": "Autorouter",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "http://127.0.0.1:4073/v1"
      },
      "models": {
        "autorouter": {
          "name": "Autorouter"
        }
      }
    }
  },
  "model": "autorouter/autorouter"
}
```

Notes:

- The top-level key is `provider` (singular). opencode hard-rejects
  unknown keys, so the older `providers` shape will not load.
- A custom gateway needs the `@ai-sdk/openai-compatible` integration
  (`npm`) and its base URL under `options.baseURL`.
- No `apiKey` is required: by default the gateway does not enforce
  client auth (`server.require_auth` is off) and resolves the upstream
  key from its own secret store. If you enable `require_auth`, re-add
  `options.apiKey` set to your gateway token.
- The routed model is decided inside AutoRouter, so set the editor to
  the single `autorouter` model id and switch the real model / provider
  at runtime from AutoRouter — no editor restart needed.

**Aider** - environment variables:

```sh
export OPENAI_API_BASE=http://127.0.0.1:4073/v1
export OPENAI_API_KEY=any-non-empty-string
aider --model openai/gpt-5
```

For Anthropic-shaped clients, use `ANTHROPIC_API_BASE` instead.

**Generic OpenAI client** (Python, Node, etc.):

```python
from openai import OpenAI
client = OpenAI(
    base_url="http://127.0.0.1:4073/v1",
    api_key="any-non-empty-string",  # gateway uses its own key
    default_headers={"X-AutoRouter-Source": "openai"},
)
resp = client.chat.completions.create(
    model="gpt-5",
    messages=[{"role": "user", "content": "Hello"}],
)
```

### 7.4 Streaming responses

Set `"stream": true` in the request body (the default is
controlled by the `defaults.stream_by_default` setting). The
gateway returns the same Server-Sent-Events shape the upstream
provider would, translated on the fly.

Example OpenAI streaming call:

```python
stream = client.chat.completions.create(
    model="gpt-5",
    stream=True,
    messages=[{"role": "user", "content": "Stream a haiku"}],
)
for chunk in stream:
    print(chunk.choices[0].delta.content or "", end="")
```

The gateway keeps the upstream stream open until the upstream
closes it, then closes its own response stream. The stream-idle
timeout (600s by default) is enforced server-side.

### 7.5 Authentication

When `server.require_auth = true` in the config, the gateway
requires every gateway request (including `/ui/*`) to carry a
bearer token. Set it on the client:

```
Authorization: Bearer <auth_token>
```

The Settings page lets you toggle the requirement and set the
token. On shared hosts or multi-user environments, you should
enable authentication to prevent other users on the same machine
from accessing your local gateway.

---
## 8. Configuration File Reference

AutoRouter reads configuration from a stack of sources, in
precedence order:

1. Built-in defaults (compiled into the binary).
2. System-wide TOML - `/etc/autorouter/config.toml` on Linux,
   `/Library/Application Support/autorouter/config.toml` on
   macOS, `%ProgramData%\autorouter\config.toml` on Windows.
   Override with `AUTOROUTER_SYSTEM_CONFIG`.
3. User-level TOML - `<config_dir>/config.toml`. The config dir
   is `directories::ProjectDirs::from("com", "",
   "autorouter").config_dir()`. Override with
   `AUTOROUTER_USER_CONFIG`.
4. Environment variables - see [§17](#17-reference-environment-variables).
5. Runtime overrides - written by the Settings page PATCH
   endpoint and the `autorouter_config::ConfigLoader::with_override`
   API.

Higher-numbered sources override lower-numbered ones, with one
exception: empty string values in a higher layer do *not*
overwrite a value in a lower layer (this lets the system file
set defaults while the user file selectively overrides).

The full schema is:

```toml
[server]
bind = "127.0.0.1:4073"
max_body_bytes = 16777216
request_timeout_seconds = 300
stream_idle_timeout_seconds = 600
enable_cors = true
require_auth = false
auth_token = null

[defaults]
# Empty by default — the first provider you configure automatically
# becomes the default. Override manually from the Settings page or
# the TOML below if you want a specific model.
default_model = ""
default_provider = ""
stream_by_default = false
max_total_tokens = 1000000

[features]
# Opt-in background features. All default to false so a fresh
# install never makes unexpected outbound connections.
model_scraping = false   # fetch OpenRouter + Artificial Analysis data

[storage]
data_dir = ""        # empty = OS default
database_file = "autorouter.db"
backup_on_shutdown = true
backup_keep = 3

[logging]
level = "info"       # trace|debug|info|warn|error
json = false
file = null          # absolute path; null = stdout

[routing]
rules = []           # list of RoutingRule; see below
default_tags = []

[providers.openai]
display_name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key_secret_id = "env:OPENAI_API_KEY"
enabled = true
model_allowlist = []
default_headers = {}

[providers.anthropic]
display_name = "Anthropic"
base_url = "https://api.anthropic.com"
api_key_secret_id = "env:ANTHROPIC_API_KEY"
enabled = true
model_allowlist = []
default_headers = {}

[providers.gemini]
display_name = "Gemini"
base_url = "https://generativelanguage.googleapis.com"
api_key_secret_id = "env:GEMINI_API_KEY"
enabled = true
model_allowlist = []
default_headers = {}

[providers.custom.<name>]
display_name = "..."
base_url = "..."
api_key_secret_id = "env:..."   # or a secret-store id
enabled = true
model_allowlist = ["..."]
default_headers = {}
```

A `RoutingRule` supports **two matcher shapes**:

1. **Modern (`needs` + dedicated match fields)** — preferred. Each
   field is optional; rules fire when all of the present fields
   match.

   ```json
   {
     "name": "use-haiku-for-tools",
     "priority": 10,
     "match_model_contains": ["gpt-4o"],
     "needs": { "tools": true, "vision": false, "audio": false },
     "prefer_free": false,
     "match_latency_below_ms": 1500,
     "match_cost_below_per_million": 30.0,
     "max_context_tokens": 200000,
     "target": { "provider": "anthropic", "model": "claude-haiku-4-5" },
     "targets": [
       { "provider": "anthropic", "model": "claude-haiku-4-5" },
       { "provider": "openai", "model": "gpt-4o-mini" }
     ]
   }
   ```

2. **Legacy (`when`)** — kept for back-compat with the examples in
   earlier versions of this manual. Folded into the modern matcher
   at evaluation time; the modern form always wins when both are
   present.

   ```json
   {
     "name": "use-haiku-for-tools",
     "priority": 10,
     "when": { "needs_tools": true },
     "target": { "provider": "anthropic", "model": "claude-haiku-4-5" }
   }
   ```

`when` accepts a small matcher language: `needs_tools`,
`needs_vision`, `needs_audio`, `approx_input_tokens_gt`.

The modern matcher fields are:

- `match_tags_any: [string]` — request tags include any of these
- `match_tags_all: [string]` — request tags include all of these
- `match_model_contains: [string]` — model name contains any substring
- `needs: { tools?, vision?, audio?, min_context? }` — capability needs
- `prefer_free: bool` — caller asked for a free-tier model
- `match_latency_below_ms: u64` — health-tracker p95 is below this
- `match_cost_below_per_million: f64` — per-million-token cost cap
- `match_quota_below_pct: f32` — remaining quota is above this
- `match_benchmark_above: f32` — quality score is above this
- `max_context_tokens: u32` — request's max total tokens is at or below
- `when_multimodal: { image?, audio?, document? }` — content type filter
- `targets: [RouteTarget]` — fallback chain; the first healthy entry wins

---

## 9. HTTP API Reference

The gateway listens on `127.0.0.1:4073` by default. Every
endpoint below is JSON in, JSON out, unless noted otherwise.

### 9.1 Gateway endpoints

| Method | Path                                              | Purpose                                    |
| ------ | ------------------------------------------------- | ------------------------------------------ |
| POST   | `/v1/chat/completions`                            | OpenAI Chat Completions shape.              |
| POST   | `/v1/chat/completions` (`stream: true`)           | Streaming variant, returns SSE.            |
| POST   | `/v1/responses`                                   | OpenAI Responses shape.                    |
| POST   | `/v1/responses` (`stream: true`)                  | Streaming variant.                         |
| POST   | `/v1/messages`                                    | Anthropic Messages shape.                  |
| POST   | `/v1/messages` (`stream: true`)                   | Streaming variant.                         |
| POST   | `/v1beta/models/{*rest}`                          | Gemini generateContent.                    |
| GET    | `/v1/models`                                      | List advertised models.                    |
| GET    | `/v1/sessions`                                    | List active sessions.                      |
| GET    | `/healthz`                                        | Health probe; returns `200 OK`.            |
| GET    | `/metrics`                                        | Prometheus metrics.                        |

> [!NOTE]
> The legacy endpoints `/openai/v1/chat/completions` and `/openai/v1/responses` remain fully supported as backward-compatible aliases for existing configurations.

**OpenAI Chat Completions example:**

```sh
curl -X POST http://127.0.0.1:4073/v1/chat/completions \
  -H "content-type: application/json" \
  -H "x-autorouter-source: openai" \
  -H "x-autorouter-session: $(uuidgen)" \
  -d '{
    "model": "gpt-5",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

**Anthropic Messages example:**

```sh
curl -X POST http://127.0.0.1:4073/v1/messages \
  -H "content-type: application/json" \
  -H "x-autorouter-source: anthropic" \
  -H "anthropic-version: 2023-06-01" \
  -d '{
    "model": "claude-sonnet-4-5",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

**Gemini generateContent example:**

```sh
curl -X POST "http://127.0.0.1:4073/v1beta/models/gemini-2.5-pro:generateContent" \
  -H "content-type: application/json" \
  -H "x-autorouter-source: gemini" \
  -d '{
    "contents": [{"parts": [{"text": "Hello"}]}]
  }'
```

### 9.2 Dashboard / UI endpoints

These are the endpoints the desktop app's React UI calls. They
are also useful for scripting and integration tests.

| Method | Path                  | Purpose                                    |
| ------ | --------------------- | ------------------------------------------ |
| GET    | `/ui/status`          | Gateway status snapshot.                   |
| GET    | `/ui/providers`       | Providers + models list.                  |
| GET    | `/ui/sessions`        | Sessions list (alias of `/v1/sessions`).  |
| GET    | `/ui/settings`        | Full `AppConfig` as JSON.                  |
| PATCH  | `/ui/settings`        | Partial update of `AppConfig`.             |
| GET    | `/ui/logs`            | Log tail with `since`, `limit`, `level`.   |
| GET    | `/ui/server`          | Server build info.                         |
| POST   | `/ui/restart`         | Request a graceful restart.                |

**`/ui/logs` query parameters:**

* `since` - Unix epoch milliseconds. Return only lines after
  this timestamp. Use the previous response's `next_since` for
  incremental tails.
* `limit` - Maximum number of lines to return (clamped 1..5000,
  default 500).
* `level` - Filter by level: `debug`, `info`, `warn`, `error`.

**`/ui/settings` PATCH payload:**

A partial `AppConfig`. Only the fields you send are updated.
For example, to change just the bind address:

```sh
curl -X PATCH http://127.0.0.1:4073/ui/settings \
  -H "content-type: application/json" \
  -d '{"server": {"bind": "127.0.0.1:9000"}}'
```

### 9.3 Health and metrics

`GET /healthz` returns `200 OK` with `{"status": "ok"}` as long
as the gateway is alive. Use it as a Docker / Kubernetes liveness
probe.

`GET /metrics` returns Prometheus text format. Useful series:

* `autorouter_requests_total{source_provider,target_provider,model}` - counter.
* `autorouter_failures_total{source_provider,target_provider,reason}` - counter.
* `autorouter_translation_seconds_bucket{direction,provider}` - histogram.
* `autorouter_translation_seconds_sum`,
  `autorouter_translation_seconds_count` - histogram totals.
* `autorouter_upstream_seconds_bucket{provider,model,outcome}` - histogram.
* `autorouter_translation_overhead_seconds_bucket{source_provider,target_provider}` - histogram (translation time minus upstream).
* `autorouter_active_sessions{source_provider}` - gauge.

A minimal Prometheus scrape config:

```yaml
scrape_configs:
  - job_name: autorouter
    static_configs:
      - targets: ['127.0.0.1:4073']
    metrics_path: /metrics
```

---

## 10. Translation Behaviour and Limitations

AutoRouter's translation layer converts between four wire formats
in real time. The current state:

* **OpenAI Chat Completions** - full read/write. Supports text,
  tool use, streaming, and the standard finish reasons.
* **OpenAI Responses** - full read/write. The newer Responses
  shape is supported as a first-class inbound and outbound
  format. Tools and streaming are translated.
* **Anthropic Messages** - full read/write. The `system` prompt
  is split out per the Anthropic shape. Tool use maps between
  OpenAI's `tool_calls` and Anthropic's `tool_use`. Streaming
  uses Anthropic's `message_start` / `content_block_delta` /
  `message_stop` event sequence.
* **Gemini `generateContent`** - full read/write. Tools map to
  `functionCall` parts. Streaming is supported. Safety settings
  pass through unchanged.

**Cross-format translation:**

When the source and target providers differ (e.g. an OpenAI
client hits an Anthropic upstream), the translation pipeline:

1. Parses the inbound body into the universal `UniversalRequest`
   schema.
2. Picks the outbound adapter based on the routing decision.
3. Serialises the universal request into the outbound wire
   format.
4. Forwards to the upstream.
5. Parses the upstream response back into `UniversalResponse`.
6. Re-encodes the response in the inbound wire format the
   client expects.

Known caveats:

* `image_url` (OpenAI) maps to Anthropic's `image` source type
  and Gemini's `inlineData` part. URL fetching is left to the
  upstream; the gateway does not download URLs.
* Anthropic's `prompt_caching` and OpenAI's `prompt_cache_key`
  pass through. The gateway does not implement its own cache.
* `response_format` (OpenAI JSON mode) is a hint; the gateway
  does not enforce it. If you need strict JSON, use the
  provider-specific field.
* Tool definitions: the gateway preserves the JSON schema as
  the source provides it. Custom keywords (e.g. `strict` on
  OpenAI) are passed through to the upstream if the target
  protocol supports them.

**Reasoning / thinking content (chain-of-thought):**

Reasoning-tuned models (Anthropic extended thinking, OpenAI
o-series, Gemini thoughts, DeepSeek R1, Qwen QwQ, etc.) emit
a model's "thinking" content alongside its final answer.
AutoRouter preserves this end-to-end. Two wire shapes are
supported:

1. **Separate field** - the upstream uses a dedicated
   reasoning field. AutoRouter reads it on the inbound side
   and re-emits it on the outbound side in the target format:

   | Source wire format | Reasoning field | Outbound when targeting OpenAI Chat | Outbound when targeting Anthropic | Outbound when targeting Gemini |
   |---|---|---|---|---|
   | OpenAI Chat Completions | `delta.reasoning_content` / `message.reasoning_content` | `message.reasoning_content` (omitted if empty) | `type: "thinking"` content block | `parts: [{ text, thought: true }]` |
   | OpenAI Responses | `response.reasoning_text.delta` / `output[]` `type: "reasoning"` | `message.reasoning_content` | `type: "thinking"` content block | `parts: [{ text, thought: true }]` |
   | Anthropic Messages | `content_block_delta` `type: "thinking_delta"` / `type: "thinking"` content block | `message.reasoning_content` | `type: "thinking"` content block | `parts: [{ text, thought: true }]` |
   | Gemini generateContent | `parts: [{ thought: true, text }]` | `message.reasoning_content` | `type: "thinking"` content block | `parts: [{ text, thought: true }]` |

2. **Inline tags** - some upstreams (notably community
   proxies that speak OpenAI Chat Completions) embed the
   thinking inline in `content` as `<think>...</think>` or
   `<thinking>...</thinking>` blocks. AutoRouter's
   `reasoning_extractor` module strips those tags at decode
   time and reclassifies their contents as reasoning, so
   they reach the client as `reasoning_content` (or the
   appropriate target-format field) instead of appearing as
   visible text in the chat. Inline tags split across SSE
   chunks are correctly reassembled by the streamer's
   carry-buffer state machine (carry cap 64 KiB; a runaway
   carry is flushed as plain text to bound memory).

Clients that render reasoning should subscribe to
`message.reasoning_content` (non-streaming) or
`delta.reasoning_content` (streaming) on the OpenAI Chat
shape, and the analogous field/block on the other target
shapes. AutoRouter does not invent reasoning content where
the upstream produced none.

---

## 11. Logging, Metrics, and Observability

**Logging:**

* The default format is a human-readable line per record.
* Set `logging.json = true` (or `AUTOROUTER_LOG_JSON=1`) for
  structured JSON suitable for log shippers.
* The level filter is `tracing_subscriber::EnvFilter`-compatible.
  Examples:
  * `info` - info and above.
  * `info,gateway=debug` - info everywhere, debug for the
    `gateway` target.
  * `autorouter_server=trace` - trace-level for the server
    crate.
* The desktop app keeps the last 2000 log lines in memory for
  the Logs page.
* The headless binary logs to stdout (or to `logging.file` if
  set). Use `journalctl` / `pm2` / `nssm` to capture it.

**Metrics:**

* `/metrics` exposes Prometheus text format. See
  [§9.3](#93-health-and-metrics) for the series.
* The `record_request` and `record_failure` helpers are
  exposed from the `autorouter_observability` crate for custom
  instrumentation.
* The translation overhead histogram
  (`autorouter_translation_overhead_seconds`) is the most
  useful for performance work - it strips out upstream latency
  so you can see the cost of the translation itself.

**Health tracking:**

* The `HealthTracker` in `autorouter-router` keeps a sliding
  window of upstream health samples. The smart router consults
  it when picking between competing upstreams.
* In the current version the smart router is wired into both
  the headless binary and the desktop shell. **Routing rules
  hot-reload on the next request** — there is no need to restart
  the gateway after editing a rule from the Routing page or
  editing `config.toml` and re-saving from the dashboard. The
  built-in `IdentityRouter` is still available as a fallback
  when the smart router is not configured.

---

## 12. Data, Storage, and Backups

AutoRouter keeps a small SQLite database for runtime state
(sessions, recent request metadata, cached model descriptors).
The database is not the source of truth for configuration -
that lives in TOML files - but it is used for things that
should survive restarts (recent sessions, request counters).

**Default data directories:**

| OS      | Path                                                                    |
| ------- | ----------------------------------------------------------------------- |
| Windows | `%LocalAppData%\autorouter\data`                                      |
| macOS   | `~/Library/Application Support/autorouter/`                            |
| Linux   | `~/.local/share/autorouter/`                                            |

The config directory is one level up (Windows:
`%LocalAppData%\autorouter\config`, macOS:
`~/Library/Preferences/autorouter/`, Linux:
`~/.config/autorouter/`).

**Backups:**

* When `storage.backup_on_shutdown = true` (the default), the
  database is copied to `<data_dir>/backups/autorouter.db.<UTC
  timestamp>` on every clean shutdown.
* Only the last `storage.backup_keep` backups are retained
  (default 3). Older ones are pruned.
* To restore: stop the gateway, copy the desired backup over
  the live `autorouter.db`, and start the gateway again.
* To back up manually while the gateway is running, use the
  SQLite `.backup` command: `sqlite3 autorouter.db ".backup
  /path/to/backup.db"`.

**Revealing the data dir:** the Settings page has a "Reveal
data dir" button that opens the directory in the OS file
manager. The Tauri shell uses the `opener` plugin to do this.

**Database size:** the database is small (a few MB after weeks
of use). The `sessions` table grows linearly with traffic and
is bounded by the gateway's in-memory tail; old sessions can
be pruned with `DELETE FROM sessions WHERE last_seen < ?`.

---

## 13. Security Model

AutoRouter is designed for **local-first** operation. The
default bind address is `127.0.0.1:4073`, which only accepts
connections from the local machine. The full security model:

* **No network exposure by default.** A request from a remote
  host is rejected by the kernel before it reaches the gateway.
* **API keys never leave the device.** They are either read
  from environment variables on the local machine or stored in
  the OS keychain. The gateway forwards them only to the
  configured upstream.
* **CSP on the desktop webview** - the React UI is served from
  a Tauri-internal origin (`tauri://localhost`) with a strict
  Content Security Policy that only allows `connect-src` to
  `http://localhost:*` and `http://127.0.0.1:*`. A malicious
  page loaded inside the webview cannot exfiltrate data.
* **No remote code loading.** The webview serves a static
  bundle from the binary. The gateway does not fetch or eval
  any remote scripts.
* **Optional bearer auth.** Set `server.require_auth = true`
  to require `Authorization: Bearer <token>` on every request.
  This is mostly useful when binding to a non-loopback address.
* **Secrets in the keychain.** API keys in the secret store go
  through the OS keychain (Windows Credential Manager, macOS
  Keychain, Linux Secret Service). The keychain enforces
  per-user access; another user on the same machine cannot
  read them.
* **macOS hardened runtime.** The macOS bundle is built with
  the hardened runtime enabled and an entitlements file that
  allows network client/server and user-selected file read.
  Disable these entitlements only if you know what you are
  doing.
* **File permissions.** On Unix, the secret-store JSON file
  is created with `0600` (owner read/write only). The data
  directory is `0700`.
* **Background scraping is opt-in.** When
  `[features] model_scraping = true`, the gateway periodically
  fetches model pricing and benchmark data from
  `openrouter.ai/api/v1/models` and
  `artificialanalysis.ai/leaderboards/models`. The scraped JSON
  is cached locally at `<data_dir>/models_data.json` and refreshed
  at most once per 24 hours. **This is off by default** — a
  fresh install never makes outbound connections to third-party
  domains. The gateway sends only a `GET` request with no API
  keys or request payloads; the responses are capped at 8 MiB to
  prevent resource exhaustion.

**Recommended hardening for non-loopback bindings:**

* Always enable `require_auth` when binding to a non-loopback
  address.
* Run the gateway behind a reverse proxy (Caddy, nginx,
  traefik) that adds TLS, rate limiting, and request logging.
* Set `enable_cors = false` unless you need cross-origin
  access from a specific origin.
* Use a long, random `auth_token` (32+ bytes from
  `openssl rand -hex 32`).

---

## 14. Routing Engine

The default routing engine is the **identity router**: a
request that comes in as OpenAI goes to the OpenAI upstream,
a request that comes in as Anthropic goes to the Anthropic
upstream, and so on. The model id is forwarded as-is.

The **smart router** adds:

* A rule engine that evaluates `RoutingRule`s in priority
  order. The first rule whose `when` clause matches wins.
* A capability registry that knows the context window, tool
  support, vision support, etc. for every model.
* A health tracker that records success / failure samples
  per upstream and prefers healthier upstreams.

**Rule examples:**

```json
[
  {
    "name": "small-tasks-go-to-haiku",
    "priority": 10,
    "when": { "approx_input_tokens_lt": 4000 },
    "target": { "provider": "anthropic", "model": "claude-haiku-4-5" }
  },
  {
    "name": "vision-goes-to-gemini",
    "priority": 5,
    "when": { "needs_vision": true },
    "target": { "provider": "gemini", "model": "gemini-2.5-pro" }
  },
  {
    "name": "tagged-routes",
    "priority": 1,
    "when": { "tag_includes": "experimental" },
    "target": { "provider": "openai", "model": "gpt-5-mini" }
  }
]
```

Rules live in the `routing.rules` array in the config file. The
desktop app exposes a full Routing page UI for editing rules
(CRUD, drag-to-reorder, templates, live test runner); the
config file is still the source of truth, but it is
re-evaluated by the running smart router on the next request
after any save — no gateway restart required.

**Available `when` matchers:**

* `needs_tools: bool` - the request includes tool definitions
  or a tool call.
* `needs_vision: bool` - the request contains at least one
  image input.
* `needs_audio: bool` - the request contains at least one
  audio input.
* `approx_input_tokens_lt: u32` /
  `approx_input_tokens_gt: u32` - coarse input size check
  (4 chars ~= 1 token).
* `tag_includes: string` - the request has the given tag in
  its `X-AutoRouter-Tag` header (comma-separated list).
* `model_matches: string` - regex on the inbound model id.

**The `target` field:**

* `provider` - one of the configured provider ids, including
  custom ones (e.g. `groq`).
* `model` - the model id on the upstream.
* `headers` (optional) - extra headers to add to the upstream
  request, e.g. `{"x-trace-id": "..."}`.

---
## 15. Troubleshooting

**The gateway does not start / the dashboard says "connecting..."**

* Check the Settings page's bind field. Make sure no other
  process is on the port: `netstat -ano | grep 4073` (Windows)
  or `lsof -i :4073` (macOS / Linux).
* If you changed the bind, restart the app - the new bind is
  only honoured on launch.
* If you see "permission denied" on a Unix-like system, the
  port number may be in the privileged range (< 1024). Use a
  port above 1024 or run as root (not recommended).

**Provider requests return `upstream_key_missing`**

* The provider's `api_key_secret_id` is not resolving to a
  value. Two common causes:
  * The `env:NAME` value references a variable that is not
    set in the shell that launched AutoRouter. Set the
    variable in the same shell session, or use a system-wide
    environment definition (System Properties on Windows,
    `launchctl setenv` on macOS, `systemd` `EnvironmentFile`
    on Linux).
  * The plain id is not in the secret store. The desktop app
    uses the OS keychain - verify the key exists in
    Credential Manager / Keychain Access / `secret-tool`.

**Anthropic requests return `400 invalid_request_error`**

* The `anthropic-version` header is required by Anthropic and
  must be passed through. The gateway does not add it
  automatically; the client must. If your client does not
  send it, add it in the request: `-H "anthropic-version:
  2023-06-01"`.

**The dashboard is blank / the React app did not load**

* The webview may have failed to fetch the static bundle.
  Reinstall the app or run `node scripts/bundle.mjs` to
  rebuild the bundle. On the dev path, run
  `cargo run -p autorouter-desktop` after rebuilding the
  UI with `cd ui && npm run build`.

**Streaming stalls mid-response**

* The upstream is slow or disconnected. The gateway has a
  stream-idle timeout (default 600s). Increase it via
  `AUTOROUTER_STREAM_IDLE_TIMEOUT_SECONDS` if your upstream
  is known to be slow.
* Some upstreams have aggressive idle disconnects; wrap the
  call in a retry loop on the client side.

**Translation is lossy / a field is dropped**

* Open an issue with the request and response bodies (redact
  secrets). The translation layer handles the common case
  but exotic fields (provider-specific extensions, custom
  metadata) are not always preserved.

**The headless binary does not honour the system tray**

* The tray is part of the desktop shell. The headless
  `autorouter-app` is a plain HTTP server with no GUI; it
  logs to stdout.

**Mac users see "AutoRouter is damaged and can't be opened"**

* Gatekeeper is rejecting the unsigned bundle. Open it from
  Finder: right-click -> Open -> Confirm. Or run
  `xcrun --attr com.apple.quarantine -d /Applications/AutoRouter.app`
  to remove the quarantine flag.

**Linux users see "could not load webview"**

* Install the runtime libraries listed in [§3.3](#33-linux-installer).
  On Debian / Ubuntu: `sudo apt install libwebkit2gtk-4.1-0
  libgtk-3-0 libayatana-appindicator3-1 librsvg2-2 libsoup-3.0-0
  libjavascriptcoregtk-4.1-0`.

**Windows users see "WebView2 is required"**

* The installer embeds the WebView2 bootstrapper, but if
  the install was blocked (e.g. by an existing policy), run
  the bundled `MicrosoftEdgeWebview2Setup.exe` from the
  install directory or download it from
  <https://developer.microsoft.com/microsoft-edge/webview2/>.

---

## 16. Frequently Asked Questions

**Q: Does AutoRouter send my prompts to anyone besides the configured upstream?**

No. The gateway only forwards requests to the upstream
provider you configured for that route. There is no
telemetry, no analytics, no "phone home" - the desktop app
does not even make outbound calls except to the configured
upstreams.

**Q: Can I run multiple instances?**

Yes, on different bind addresses. Use `AUTOROUTER_BIND` to
give each instance a unique address (e.g.
`127.0.0.1:4073` and `127.0.0.1:8081`). Each instance keeps
its own config and data dir; the second instance's data dir
should be set with `AUTOROUTER_DATA_DIR` to avoid collision.

**Q: Can AutoRouter act as a model gateway for a team?**

The current version is single-user: one machine, one user,
one secret store. For team use, run a separate instance per
user.

**Q: Is the gateway HTTPS?**

No. The local bind is plain HTTP because TLS on a self-signed
cert for `127.0.0.1` adds complexity without much security
benefit (the loopback interface is not exposed to the
network). If you bind to a non-loopback address, run
AutoRouter behind a reverse proxy that adds TLS.

**Q: Can I plug in a local model (llama.cpp, Ollama)?**

Yes - add a custom provider that points at the local
OpenAI-compatible endpoint (llama.cpp's server, Ollama in
`OPENAI_COMPAT` mode, vLLM, LM Studio). Use the
`providers.custom.<name>` section. See [§6.4](#64-custom-openai-compatible-providers).

**Q: What about tool use across formats?**

Tool definitions are translated between the four wire
formats. A tool defined in OpenAI shape (`tools: [...]`) is
sent to Anthropic as `tools: [...]` in the Anthropic shape,
and to Gemini as `tools: [{functionDeclarations: [...]}]`.
Tool call results flow back the same way. Streaming tool
use is supported on the four protocols.

**Q: Why does my streaming response lag?**

The gateway does not buffer chunks - it forwards them as
they arrive. If you see lag, the upstream is the bottleneck.
Look at the `autorouter_upstream_seconds` histogram in
`/metrics` for per-upstream latency percentiles.

**Q: How do I add a new model that is not in the list?**

Add it to the provider's `model_allowlist` in the config
file. The gateway will accept requests for that model id
even though it is not in the static `models()` snapshot.

**Q: Is the source code available under a permissive license?**

Yes - MIT or Apache-2.0, at your option. See `LICENSE`.

**Q: Can I contribute?**

Yes - pull requests welcome. The architecture is described
in `README.md`; the rust workspace has 80+ tests that you
should extend when adding features.

**Q: Do I have to write `env:NAME` or `keychain:ID` prefixes
for `api_key_secret_id`?**

No. AutoRouter auto-detects the format of the value you type
in the dashboard. If the value matches an existing environment
variable name (e.g. `OPENROUTER_API_KEY`), it is saved as an
`env:` reference and no secret is written to the keychain. If
the value looks like a raw key (e.g. `sk-or-v1-…`), it is
written to the secret store. See [§6.5](#65-api-key-resolution-env-vs-secret-store)
for the full rules.

---

## 17. Reference: Environment Variables

All variables are read at startup. Runtime overrides via the
Settings page do not affect the env-var layer; they are
applied on top of it.

> **Bare env-var lookups in `api_key_secret_id`.** Because of the
> auto-detect behaviour described in [§6.5](#65-api-key-resolution-env-vs-secret-store),
> any environment variable in the table below (and any other
> `ALL_CAPS_SNAKE_CASE` name in your shell) can be referenced from
> a provider's `api_key_secret_id` field with or without the
> `env:` prefix. Both `env:OPENROUTER_API_KEY` and
> `OPENROUTER_API_KEY` resolve to the same value at request time.

| Variable                                | Default                    | Meaning                                                              |
| --------------------------------------- | -------------------------- | -------------------------------------------------------------------- |
| `AUTOROUTER_BIND`                       | `127.0.0.1:4073`           | Bind address for the gateway.                                        |
| `AUTOROUTER_LOG_LEVEL`                   | `info`                     | Tracing filter (e.g. `info,gateway=debug`).                          |
| `AUTOROUTER_LOG_JSON`                   | `0`                        | `1` / `true` / `yes` for JSON logs.                                 |
| `AUTOROUTER_LOG_FILE`                   | unset                      | Path to a log file; stdout if unset.                                 |
| `AUTOROUTER_DATA_DIR`                   | OS default                 | Override the data directory.                                         |
| `AUTOROUTER_SYSTEM_CONFIG`              | OS default path            | Override the system TOML file path.                                  |
| `AUTOROUTER_USER_CONFIG`                | OS default path            | Override the user TOML file path.                                    |
| `AUTOROUTER_DEFAULT_MODEL`              | `gpt-5`                    | Default model when a request omits it.                              |
| `AUTOROUTER_DEFAULT_PROVIDER`           | `openai`                   | Default provider when a request omits it.                           |
| `AUTOROUTER_MAX_TOKENS`                 | `1000000`                  | Max total tokens per request.                                        |
| `AUTOROUTER_AUTH_TOKEN`                 | unset                      | Set to require bearer auth; the value is the expected token.        |
| `AUTOROUTER_REQUIRE_AUTH`               | `0`                        | `1` / `true` / `yes` to enforce the bearer check.                   |
| `AUTOROUTER_SECRET_STORE`               | `keyring`                  | `keyring`, `file`, or `memory`.                                      |
| `AUTOROUTER_SECRET_FILE`                | unset                      | Path to the JSON secret file (when store=file).                      |
| `AUTOROUTER_SKIP_SIGNING`               | unset                      | CI opt-out for code signing.                                         |
| `RUST_LOG`                              | (forwarded to tracing)     | Equivalent to `AUTOROUTER_LOG_LEVEL` if set.                         |

---

## 18. Glossary

* **Adapter** - a Rust type that knows how to parse and emit one
  wire format.
* **Capability** - metadata about a model: context window, max
  output, support for tools, vision, audio, streaming.
* **CLI** - a tool that talks to the gateway, e.g. Claude Code.
* **Custom provider** - a user-defined OpenAI-compatible upstream
  in `providers.custom.<name>`.
* **Dashboard** - the embedded web UI in the desktop app.
* **DSL** - the small `when` matcher language for routing rules.
* **Gateway** - the HTTP server that AutoRouter runs.
* **Identity router** - the default router that preserves the
  source provider when picking the upstream.
* **Loopback** - `127.0.0.1` (or `::1`), the local-only bind.
* **OpenAI-compatible** - any endpoint that speaks the OpenAI
  Chat Completions or OpenAI Responses shape.
* **Provider** - an upstream AI service (OpenAI, Anthropic,
  Gemini, or a custom one).
* **RouteDecision** - the result of routing: which upstream
  provider and model to use, and why.
* **Session** - a logical group of requests identified by the
  `X-AutoRouter-Session` header.
* **Smart router** - the rule + capability + health engine.
* **Source** - the protocol the client tool speaks.
* **Target** - the upstream the gateway forwards to.
* **Upstream** - the AI provider that AutoRouter forwards to.
* **Wire format** - the JSON shape used by a specific API
  (OpenAI Chat Completions, Anthropic Messages, etc.).

---

## 19. Getting Help

* **Project README** - `README.md`. Architecture overview, build
  instructions, crate table.
* **Source code** - `crates/` for the Rust workspace,
  `ui/` for the React app, `scripts/` for the bundler.
* **Tests** - `cargo test --workspace` runs 80+ unit and
  integration tests. They serve as runnable examples of
  almost every public API.
* **Logs** - the Logs page in the dashboard, or stdout from
  the headless binary.
* **Metrics** - `curl http://127.0.0.1:4073/metrics` for
  Prometheus output.
* **Issues** - file bugs and feature requests on the project
  issue tracker. Include the gateway version (`/ui/status`
  shows it) and a redacted log excerpt.

Happy routing!
