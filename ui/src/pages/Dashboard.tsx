import { useEffect, useState } from "react";
import type { StatusResponse } from "../types";
import { api } from "../api";
import { CopyButton } from "../components/CopyButton";
import { CodeBlock, type CodeLanguage } from "../components/CodeBlock";
import { useToast } from "../components/Toast";
import { isTauri } from "../tauri";
import { IconExternal } from "../components/Icons";

/* ─── Time helpers (mirror what other pages do) ─────────────── */
function formatUptime(s: number) {
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d) return `${d}d ${h}h ${m}m`;
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m ${sec}s`;
  return `${sec}s`;
}

function timeAgoShort(iso: string): string {
  try {
    const t = new Date(iso).getTime();
    const diff = Date.now() - t;
    if (!isFinite(diff) || diff < 0) return "—";
    if (diff < 60_000) return `${Math.max(1, Math.floor(diff / 1000))}s`;
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h`;
    return `${Math.floor(diff / 86_400_000)}d`;
  } catch {
    return "—";
  }
}

/* ─── Per-tool recipe card metadata ─────────────────────────── */
interface ToolRecipe {
  name: string;
  tagline: string;
  language: CodeLanguage;
  build: (bind: string) => string;
  /** Optional badge shown next to the tool name (e.g. "TOML"). */
  tag?: string;
}

const TOOLS: ToolRecipe[] = [
  {
    name: "Claude Code",
    tagline: "Set the Anthropic base URL — Claude Code talks the Anthropic Messages shape natively.",
    language: "sh",
    build: (b) => `export ANTHROPIC_BASE_URL=http://${b}\nclaude-code`,
  },
  {
    name: "Codex CLI",
    tagline: "Add a custom provider block to ~/.codex/config.toml and point model_provider at it.",
    language: "toml",
    build: (b) =>
      `[model_providers.autorouter]\nname = "AutoRouter (local)"\nbase_url = "http://${b}/openai/v1"\napi_key = "any-non-empty-string"   # the gateway ignores the key`,
  },
  {
    name: "Gemini CLI",
    tagline: "Point the CLI at the local Gemini v1beta endpoint via GOOGLE_GENAI_API_BASE.",
    language: "sh",
    build: (b) => `export GOOGLE_GENAI_API_BASE=http://${b}`,
  },
  {
    name: "OpenCode",
    tagline: "Add an `autorouter` provider entry to ~/.config/opencode/config.json.",
    language: "json",
    build: (b) =>
      JSON.stringify(
        {
          providers: {
            autorouter: {
              baseURL: `http://${b}`,
              apiKey: "any-non-empty-string",
            },
          },
        },
        null,
        2,
      ),
  },
  {
    name: "Aider",
    tagline: "Use the OpenAI-compatible base URL with any non-empty API key.",
    language: "sh",
    build: (b) =>
      `export OPENAI_API_BASE=http://${b}/openai/v1\nexport OPENAI_API_KEY=any-non-empty-string`,
  },
  {
    name: "Continue / Cline / Roo Code",
    tagline: "All three read the same OpenAI-compat env vars — set them once and they all see AutoRouter.",
    language: "sh",
    build: (b) =>
      `export OPENAI_API_BASE=http://${b}/openai/v1\nexport OPENAI_API_KEY=any-non-empty-string`,
    tag: "Continue",
  },
  {
    name: "Warp",
    tagline: "Warp Terminal routes AI completions through the standard OpenAI env vars.",
    language: "sh",
    build: (b) =>
      `export OPENAI_API_BASE=http://${b}/openai/v1\nexport OPENAI_API_KEY=any-non-empty-string`,
  },
  {
    name: "Generic OpenAI client (Python)",
    tagline: "Use the OpenAI SDK with a custom base_url and the X-AutoRouter-Source default header.",
    language: "python",
    build: (b) =>
      `from openai import OpenAI\n` +
      `client = OpenAI(\n` +
      `    base_url="http://${b}/openai/v1",\n` +
      `    api_key="any-non-empty-string",  # gateway uses its own key\n` +
      `    default_headers={"X-AutoRouter-Source": "openai"},\n` +
      `)`,
  },
];

/* ─── Helpers ───────────────────────────────────────────────── */
function defaultBind(): string {
  // Same fallback the rest of the app uses when status.bind is missing.
  return "127.0.0.1:4073";
}

/* ─── Live activity strip ───────────────────────────────────── */
interface LiveSession {
  id: string;
  label: string | null;
  request_count: number;
  last_request_at: string;
}

interface LiveRequest {
  provider: string;
  model: string;
  status: number;
  latency_ms: number;
  error: string | null;
  created_at: string | number;
}

interface LiveState {
  sessions: LiveSession[];
  requests: LiveRequest[];
  hasData: boolean;
}

function statusTone(status: number): "ok" | "warn" | "err" {
  if (status >= 200 && status < 300) return "ok";
  if (status >= 400 && status < 500) return "warn";
  return "err";
}

function LiveStrip({
  onNavigate,
}: {
  onNavigate: (p: "sessions" | "requests") => void;
}) {
  const [data, setData] = useState<LiveState | null>(null);

  useEffect(() => {
    let alive = true;
    async function poll() {
      try {
        const [sessR, evR] = await Promise.all([
          api.sessions().catch(() => ({ sessions: [] as any[] })),
          api.events(undefined, 50).catch(() => ({ events: [] as any[] })),
        ]);
        if (!alive) return;
        const sessions: LiveSession[] = (sessR.sessions ?? [])
          .slice(0, 3)
          .map((s) => ({
            id: s.id,
            label: s.label,
            request_count: s.request_count,
            last_request_at: s.last_request_at ?? s.created_at,
          }));
        const requests: LiveRequest[] = (evR.events ?? [])
          .slice(0, 4)
          .map((e: any) => ({
            provider: String(e.provider ?? "—"),
            model: String(e.model ?? ""),
            status: typeof e.status === "number" ? e.status : 0,
            latency_ms: typeof e.latency_ms === "number" ? e.latency_ms : 0,
            error: e.error ?? null,
            created_at: e.created_at ?? e.ts ?? new Date().toISOString(),
          }));
        setData({
          sessions,
          requests,
          hasData: sessions.length > 0 || requests.length > 0,
        });
      } catch {
        if (alive) setData((prev) => prev ?? { sessions: [], requests: [], hasData: false });
      }
    }
    poll();
    const id = window.setInterval(poll, 5000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, []);

  if (!data || !data.hasData) return null;

  return (
    <div className="live-strip">
      <div className="live-card" role="region" aria-label="Recent sessions">
        <div className="live-head">
          <h3>Recent sessions</h3>
          <span className="live-count">{data.sessions.length}</span>
        </div>
        {data.sessions.length === 0 ? (
          <div className="live-empty">No sessions yet.</div>
        ) : (
          data.sessions.map((s) => (
            <div
              key={s.id}
              className="row"
              role="button"
              tabIndex={0}
              onClick={() => onNavigate("sessions")}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") onNavigate("sessions");
              }}
            >
              <span className="row-label">{s.label || shortId(s.id)}</span>
              <span className="row-meta">{s.request_count} req</span>
              <span className="row-meta">{timeAgoShort(s.last_request_at)}</span>
            </div>
          ))
        )}
      </div>

      <div className="live-card" role="region" aria-label="Recent requests">
        <div className="live-head">
          <h3>Recent requests</h3>
          <span className="live-count">{data.requests.length}</span>
        </div>
        {data.requests.length === 0 ? (
          <div className="live-empty">No upstream calls yet.</div>
        ) : (
          data.requests.map((r, i) => {
            const tone = statusTone(r.status);
            const rowKey = `${r.provider}-${r.model}-${r.status}-${r.created_at}-${i}`;
            return (
              <div
                key={rowKey}
                className="row"
                role="button"
                tabIndex={0}
                onClick={() => onNavigate("requests")}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") onNavigate("requests");
                }}
                title={r.error ? r.error : `${r.provider}/${r.model} → HTTP ${r.status}`}
              >
                <span className="row-label">
                  {r.provider}
                  {r.model ? <span className="row-sub">/{r.model}</span> : null}
                </span>
                <span className={"row-status " + tone}>{r.status || "—"}</span>
                <span className="row-meta">
                  {r.latency_ms > 0 ? `${r.latency_ms}ms` : ""}{" "}
                  {timeAgoShort(toIso(r.created_at))}
                </span>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

/** Normalize the event timestamp to an ISO string for timeAgoShort. */
function toIso(t: string | number): string {
  if (typeof t === "number") {
    const ms = t < 1e12 ? t * 1000 : t;
    try {
      return new Date(ms).toISOString();
    } catch {
      return new Date().toISOString();
    }
  }
  return t;
}

function shortId(id: string) {
  return id.length > 12 ? id.slice(0, 8) + "…" + id.slice(-4) : id;
}

/* ─── Page component ────────────────────────────────────────── */
export function Dashboard({
  status,
  onNavigate,
}: {
  status: StatusResponse | null;
  onNavigate: (p: any) => void;
}) {
  const bind = status?.bind ?? defaultBind();
  const baseUrl = `http://${bind}`;
  const { show } = useToast();
  const headerRows = [
    { name: "X-AutoRouter-Source", value: "openai | anthropic | gemini" },
    { name: "X-AutoRouter-Target", value: "<provider>" },
    { name: "X-AutoRouter-Session", value: "<uuid>" },
    { name: "X-AutoRouter-Label", value: "<name>" },
  ];

  const openInBrowser = async () => {
    const url = `${baseUrl}/ui/?page=dashboard`;
    try {
      if (isTauri()) {
        // Lazy import so the non-Tauri (vite dev) bundle stays small.
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(url);
        return;
      }
    } catch {
      // fall through to window.open
    }
    try {
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      show("Could not open browser", "err");
    }
  };

  return (
    <>
      <div className="page-header">
        <h1>Dashboard</h1>
        <div className="sub">
          A control center for connecting AI tools and adding custom providers.
        </div>
      </div>

      {/* A — Hero strip */}
      <section className="dash-hero" aria-label="Local gateway endpoint">
        <div className="dash-hero-meta">
          <div className="dash-hero-eyebrow">Local gateway endpoint</div>
          <div className="dash-hero-title">
            <span className="bind">{baseUrl}</span>
            <CopyButton
              text={baseUrl}
              size="md"
              variant="inline"
              title="Copy endpoint URL"
              successMsg="Endpoint copied"
            />
          </div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            <span className="status-pill">
              <span className={"pill-dot " + (status ? "online" : "reconnecting")} />
              {status ? `Online · v${status.version}` : "Reconnecting…"}
            </span>
          </div>
        </div>
        <div className="dash-hero-actions">
          <button
            type="button"
            className="btn"
            onClick={openInBrowser}
            title="Open the gateway UI in the system browser"
          >
            <IconExternal /> Open in browser
          </button>
        </div>
      </section>

      {/* B — Status grid */}
      <div className="grid cols-4 status-grid" style={{ marginBottom: 20 }}>
        <div className="card">
          <h3>Status</h3>
          <div className="value">
            <span className="badge ok lg">
              <span className="dot" />
              {status ? "Online" : "Offline"}
            </span>
          </div>
          <div className="delta">{status?.version ?? "—"} · gateway live</div>
        </div>
        <CopyButton
          text={baseUrl}
          variant="block"
          label=""
          successMsg="Endpoint copied"
          title="Click to copy endpoint URL"
          className="card bind-card"
        >
          <h3 style={{ pointerEvents: "none" }}>Bind</h3>
          <div className="value" style={{ pointerEvents: "none" }}>
            <span className="bind-addr">{bind}</span>
          </div>
          <div className="delta" style={{ pointerEvents: "none" }}>
            Local endpoint · click to copy
          </div>
        </CopyButton>
        <div className="card">
          <h3>Uptime</h3>
          <div className="value">
            {status ? formatUptime(status.uptime_seconds) : "—"}
          </div>
          <div className="delta">Since launch</div>
        </div>
        <div className="card">
          <h3>Active sessions</h3>
          <div className="value">{status?.session_count ?? 0}</div>
          <div className="delta">Connected AI tools</div>
        </div>
      </div>

      {/* C — Connect your tools */}
      <div className="section-head">
        <div>
          <h2>Connect your tools</h2>
          <div className="lead">
            Copy-paste these snippets into your AI tool's config to point it at the local gateway.
          </div>
        </div>
      </div>
      <div className="connect-grid">
        {TOOLS.map((tool) => {
          const snippet = tool.build(bind);
          return (
            <div key={tool.name} className="tool-card">
              <div className="head">
                <span className="tool-name">{tool.name}</span>
                {tool.tag ? <span className="tool-tag">{tool.tag}</span> : null}
              </div>
              <div className="tagline">{tool.tagline}</div>
              <div className="snippet">
                <CodeBlock
                  code={snippet}
                  language={tool.language}
                  copyLabel="Snippet copied"
                />
              </div>
            </div>
          );
        })}
      </div>

      <div className="headers-sheet" aria-label="X-AutoRouter headers">
        <div className="headers-title">Headers you'll need</div>
        {headerRows.map((h) => (
          <div key={h.name} className="header-row">
            <span className="header-name">{h.name}</span>
            <span className="header-value">{h.value}</span>
            <CopyButton
              text={`${h.name}: ${h.value}`}
              size="sm"
              variant="inline"
              successMsg="Header copied"
            />
          </div>
        ))}
      </div>

      {/* D — Live activity */}
      <div className="section-head" style={{ marginTop: 24 }}>
        <div>
          <h2>Live activity</h2>
          <div className="lead">Auto-refreshing every 5 seconds.</div>
        </div>
      </div>
      <LiveStrip onNavigate={onNavigate} />
    </>
  );
}
