import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { ProvidersResponse, ProviderInfo } from "../types";
import { IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";
import { useToast } from "../components/Toast";

/* ─── Provider presets ───────────────────────────────────────── */
type ApiFormat = "openai" | "anthropic" | "gemini";

interface Preset {
  id: string;
  name: string;
  base_url: string;
  tag?: string;
  api_format: ApiFormat;
}

/** Mirror of the Rust infer_api_format() heuristic. */
function inferApiFormat(base_url: string): ApiFormat {
  const lower = base_url.toLowerCase();
  if (lower.includes("anthropic.com")) return "anthropic";
  if (lower.includes("googleapis.com") || lower.includes("generativelanguage")) return "gemini";
  return "openai";
}

const FORMAT_LABELS: Record<ApiFormat, string> = {
  openai:    "OpenAI-compatible",
  anthropic: "Anthropic Messages",
  gemini:    "Gemini API",
};
const FORMAT_COLORS: Record<ApiFormat, string> = {
  openai:    "#22d3ee",   // cyan
  anthropic: "#f59e0b",  // amber
  gemini:    "#4ade80",  // green
};

const PRESETS: Preset[] = [
  { id: "openai",     name: "OpenAI",           base_url: "https://api.openai.com/v1",                        tag: "Popular",    api_format: "openai" },
  { id: "anthropic",  name: "Anthropic",        base_url: "https://api.anthropic.com",                        tag: "Popular",    api_format: "anthropic" },
  { id: "gemini",     name: "Google Gemini",    base_url: "https://generativelanguage.googleapis.com/v1beta", tag: "Popular",    api_format: "gemini" },
  { id: "openrouter", name: "OpenRouter",       base_url: "https://openrouter.ai/api/v1",                     tag: "Aggregator", api_format: "openai" },
  { id: "tokenrouter", name: "TokenRouter",    base_url: "https://api.tokenrouter.com/v1",                  tag: "Aggregator", api_format: "openai" },
  { id: "groq",       name: "Groq",             base_url: "https://api.groq.com/openai/v1",                   tag: "Fast",       api_format: "openai" },
  { id: "together",   name: "Together AI",      base_url: "https://api.together.xyz/v1",                                        api_format: "openai" },
  { id: "mistral",    name: "Mistral AI",       base_url: "https://api.mistral.ai/v1",                                          api_format: "openai" },
  { id: "deepseek",   name: "DeepSeek",         base_url: "https://api.deepseek.com/v1",                                        api_format: "openai" },
  { id: "perplexity", name: "Perplexity",       base_url: "https://api.perplexity.ai",                                          api_format: "openai" },
  { id: "xai",        name: "xAI / Grok",       base_url: "https://api.x.ai/v1",                                                api_format: "openai" },
  { id: "cohere",     name: "Cohere",           base_url: "https://api.cohere.ai/v1",                                           api_format: "openai" },
  { id: "fireworks",  name: "Fireworks AI",     base_url: "https://api.fireworks.ai/inference/v1",                              api_format: "openai" },
  { id: "anyscale",   name: "Anyscale",         base_url: "https://api.endpoints.anyscale.com/v1",                              api_format: "openai" },
  { id: "ollama",     name: "Ollama (local)",   base_url: "http://localhost:11434/v1",                        tag: "Local",      api_format: "openai" },
  { id: "lmstudio",  name: "LM Studio (local)", base_url: "http://localhost:1234/v1",                         tag: "Local",      api_format: "openai" },
];
const CUSTOM_PRESET: Preset = { id: "__custom__", name: "Custom Provider", base_url: "", api_format: "openai" };

const FIRST_CLASS = ["openai", "anthropic", "gemini"];

/* ─── Eye icons ──────────────────────────────────────────────── */
function IconEye() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/>
    </svg>
  );
}
function IconEyeOff() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94"/>
      <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19"/>
      <line x1="1" y1="1" x2="23" y2="23"/>
    </svg>
  );
}
function IconSearch() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>
    </svg>
  );
}
function IconPlus() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
      <line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>
    </svg>
  );
}
function IconPlug() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M9 2v6"/><path d="M15 2v6"/><path d="M6 8h12v4a6 6 0 0 1-12 0z"/><path d="M12 18v4"/>
    </svg>
  );
}
function IconTrash() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/>
    </svg>
  );
}

/** Pretty-print a "time since" relative duration. */
function timeAgo(epochMs: number, now = Date.now()): string {
  const s = Math.max(1, Math.floor((now - epochMs) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}

/* ─── Add Provider search combobox ───────────────────────────── */
function AddProviderSearch({
  existingIds,
  onSelect,
}: {
  existingIds: Set<string>;
  onSelect: (preset: Preset) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const ref = useRef<HTMLDivElement>(null);

  // Close on outside click
  useEffect(() => {
    function handler(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, []);

  const filtered = PRESETS.filter(
    (p) =>
      !existingIds.has(p.id) &&
      (query === "" || p.name.toLowerCase().includes(query.toLowerCase()) || p.id.includes(query.toLowerCase()))
  );

  const handleSelect = (p: Preset) => {
    onSelect(p);
    setOpen(false);
    setQuery("");
  };

  return (
    <div ref={ref} style={{ position: "relative" }}>
      {!open ? (
        <button
          className="btn primary"
          onClick={() => setOpen(true)}
          style={{ display: "flex", alignItems: "center", gap: 6 }}
        >
          <IconPlus /> Add Provider
        </button>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
          <div
            className="row"
            role="combobox"
            aria-expanded={open}
            aria-haspopup="listbox"
            style={{
              background: "var(--bg-card)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              padding: "6px 10px",
              gap: 8,
              minWidth: 280,
            }}
          >
            <IconSearch />
            <input
              autoFocus
              className="input"
              aria-label="Search providers"
              style={{ border: "none", background: "transparent", padding: 0, flex: 1, outline: "none" }}
              placeholder="Search providers…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Escape" && setOpen(false)}
            />
            <button
              className="btn"
              style={{ padding: "2px 8px", fontSize: 11 }}
              onClick={() => { setOpen(false); setQuery(""); }}
              aria-label="Close search"
            >
              ✕
            </button>
          </div>
          {/* Dropdown */}
          <div
            role="listbox"
            style={{
              position: "absolute",
              top: "calc(100% + 4px)",
              right: 0,
              minWidth: 300,
              background: "var(--bg-card)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              boxShadow: "0 8px 32px rgba(0,0,0,0.35)",
              zIndex: 100,
              overflow: "hidden",
              maxHeight: 340,
              overflowY: "auto",
            }}
          >
            {filtered.length === 0 && (
              <div style={{ padding: "12px 16px", color: "var(--fg-dim)", fontSize: 13 }}>
                No matching providers
              </div>
            )}
            {filtered.map((p) => (
              <button
                key={p.id}
                role="option"
                onClick={() => handleSelect(p)}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 10,
                  width: "100%",
                  padding: "10px 16px",
                  background: "transparent",
                  border: "none",
                  borderBottom: "1px solid var(--border)",
                  cursor: "pointer",
                  textAlign: "left",
                  color: "var(--fg)",
                  transition: "background 0.15s",
                }}
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--bg-elevated)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                <span style={{ flex: 1, fontWeight: 500, fontSize: 14 }}>{p.name}</span>
                {p.tag && (
                  <span
                    style={{
                      fontSize: 10,
                      padding: "2px 7px",
                      borderRadius: 99,
                      background: "var(--accent-dim, rgba(99,102,241,0.15))",
                      color: "var(--accent, #818cf8)",
                      fontWeight: 600,
                      letterSpacing: "0.03em",
                      textTransform: "uppercase",
                    }}
                  >
                    {p.tag}
                  </span>
                )}
              </button>
            ))}
            {/* Always show Custom at the bottom */}
            <button
              role="option"
              aria-label="Add custom provider"
              onClick={() => handleSelect(CUSTOM_PRESET)}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 10,
                width: "100%",
                padding: "10px 16px",
                background: "transparent",
                border: "none",
                cursor: "pointer",
                textAlign: "left",
                color: "var(--fg-dim)",
                fontSize: 13,
                transition: "background 0.15s",
              }}
              onMouseEnter={(e) => (e.currentTarget.style.background = "var(--bg-elevated)")}
              onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
            >
              <span style={{ flex: 1 }}>⚙ Custom Provider</span>
              <span style={{ fontSize: 11, color: "var(--fg-dim)" }}>configure manually</span>
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/* ─── Provider Card ──────────────────────────────────────────── */
interface DraftProvider {
  id: string;
  display_name: string;
  base_url: string;
  api_key_secret_id: string;
  enabled: boolean;
  model_allowlist: string[];
  api_format: ApiFormat;
  isNew?: boolean; // true while not yet saved
}

/* ─── Chevron Icons ─────────────────────────────────────────── */
function IconChevronDown() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ transition: "transform 0.2s" }}>
      <polyline points="6 9 12 15 18 9"/>
    </svg>
  );
}

function IconChevronRight() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" style={{ transition: "transform 0.2s" }}>
      <polyline points="9 18 15 12 9 6"/>
    </svg>
  );
}

function ProviderCard({
  draft: initial,
  onSave,
  onDelete,
}: {
  draft: DraftProvider;
  onSave: (d: DraftProvider) => Promise<void>;
  onDelete: () => Promise<void>;
}) {
  const [draft, setDraft] = useState<DraftProvider>(initial);
  const [expanded, setExpanded] = useState(initial.isNew);
  const { show: showToast } = useToast();
  const [showKey, setShowKey] = useState(false);
  const [revealedKey, setRevealedKey] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedMsg, setSavedMsg] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const deleteTimer = useRef<number | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<
    | { ok: true; status: number; latency_ms: number; model?: string }
    | { ok: false; status?: number; error: string; latency_ms?: number }
    | null
  >(null);
  const [savedAt, setSavedAt] = useState<number | null>(
    initial.isNew ? null : Date.now()
  );
  const [, forceTick] = useState(0);
  // Re-render every 30s so "Last updated Xm ago" stays current
  useEffect(() => {
    const t = window.setInterval(() => forceTick((n) => n + 1), 30_000);
    return () => window.clearInterval(t);
  }, []);

  useEffect(() => {
    setDraft(initial);
    setShowKey(false);
    setRevealedKey(null);
    setTestResult(null);
    setDeleteArmed(false);
    setSavedAt(initial.isNew ? null : Date.now());
    setExpanded(initial.isNew);
  }, [initial.id]);

  // Auto-disarm delete confirmation after 3 seconds of inactivity
  useEffect(() => {
    if (!deleteArmed) return;
    deleteTimer.current = window.setTimeout(() => {
      setDeleteArmed(false);
    }, 3000);
    return () => {
      if (deleteTimer.current !== null) {
        window.clearTimeout(deleteTimer.current);
        deleteTimer.current = null;
      }
    };
  }, [deleteArmed]);

  const isDirect = draft.api_key_secret_id !== "" && !draft.api_key_secret_id.startsWith("env:");

  // Reveal the actual secret stored under `api_key_secret_id`.
  // The textbox shows the *reference* (e.g. `keychain:openai_api_key`),
  // so the eye button used to only flip input type — the operator
  // never saw the real key. Call `cmd_secret_get` to fetch the value
  // and display it; on second click clear the revealed value.
  // Fall back to the password/text toggle when the id is empty,
  // env-prefixed, or the backend can't resolve it.
  const toggleKeyReveal = async () => {
    if (revealedKey !== null) {
      setRevealedKey(null);
      return;
    }
    if (showKey) {
      setShowKey(false);
      return;
    }
    const id = draft.api_key_secret_id;
    if (!id || id.startsWith("env:")) {
      setShowKey((s) => !s);
      return;
    }
    try {
      const r = await api.secretGet(id);
      setRevealedKey(r.value);
    } catch {
      setShowKey(true);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setSavedMsg(null);
    setTestResult(null);
    try {
      await onSave(draft);
      setSavedAt(Date.now());
      setSavedMsg("Saved ✓");
      setTimeout(() => setSavedMsg(null), 2000);
    } catch (e) {
      setSavedMsg(null);
      showToast(String(e instanceof Error ? e.message : e), "err");
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!draft.isNew) {
      // Inline arm/disarm confirmation (replaces window.confirm())
      if (!deleteArmed) {
        setDeleteArmed(true);
        return;
      }
      setDeleteArmed(false);
    }
    setDeleting(true);
    try {
      await onDelete();
    } catch (e) {
      showToast(String(e instanceof Error ? e.message : e), "err");
    } finally {
      setDeleting(false);
    }
  };

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    const t0 = performance.now();
    try {
      // Probe with the first allow-listed model if any, otherwise let the
      // server pick a sensible default for the provider's wire format.
      const model = draft.model_allowlist[0];
      const r = await api.providerTest(draft.id, model);
      const latency_ms = Math.round(performance.now() - t0);
      if (r.ok) {
        setTestResult({ ok: true, status: r.status, latency_ms, model });
      } else {
        setTestResult({ ok: false, status: r.status, error: r.error, latency_ms });
      }
    } catch (e) {
      const latency_ms = Math.round(performance.now() - t0);
      setTestResult({ ok: false, error: String(e), latency_ms });
    } finally {
      setTesting(false);
    }
  };

  return (
    <div
      className="card"
      role="region"
      aria-label={`Provider: ${draft.display_name || draft.id}`}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 0,
        padding: 0,
        overflow: "hidden",
        border: draft.isNew ? "1px solid var(--accent, #818cf8)" : undefined,
      }}
    >
      {/* Card header */}
      <div
        className="row between"
        onClick={() => setExpanded(!expanded)}
        style={{
          padding: "12px 16px",
          borderBottom: expanded ? "1px solid var(--border)" : "none",
          background: "var(--bg-elevated, rgba(255,255,255,0.03))",
          cursor: "pointer",
          userSelect: "none",
        }}
      >
        <div className="row" style={{ gap: 8 }}>
          {expanded ? <IconChevronDown /> : <IconChevronRight />}
          <span style={{ fontWeight: 600, fontSize: 15 }}>{draft.display_name || draft.id || "New Provider"}</span>
          {draft.isNew && (
            <span style={{ fontSize: 10, padding: "2px 7px", borderRadius: 99, background: "var(--accent-dim, rgba(99,102,241,0.15))", color: "var(--accent, #818cf8)", fontWeight: 600 }}>
              NEW
            </span>
          )}
          {/* API format badge */}
          <span style={{
            fontSize: 10,
            padding: "2px 8px",
            borderRadius: 99,
            background: FORMAT_COLORS[draft.api_format] + "22",
            color: FORMAT_COLORS[draft.api_format],
            fontWeight: 600,
            letterSpacing: "0.02em",
            border: `1px solid ${FORMAT_COLORS[draft.api_format]}44`,
          }}>
            {FORMAT_LABELS[draft.api_format]}
          </span>
        </div>
        <span className={"badge " + (draft.enabled ? "ok" : "warn")}>
          {draft.enabled ? "Enabled" : "Disabled"}
        </span>
      </div>

      {expanded && (
        <>
          {/* Fields */}
          <div style={{ padding: "14px 16px", display: "flex", flexDirection: "column", gap: 10, flex: 1 }}>
            {/* Show ID input only for custom/new providers */}
            {(draft.isNew || !FIRST_CLASS.includes(draft.id)) && (
              <div className="field" style={{ margin: 0 }}>
                <label style={{ fontSize: 11, marginBottom: 4, display: "block" }}>Provider ID</label>
                <input
                  className="input mono"
                  value={draft.id === "__custom__" ? "" : draft.id}
                  placeholder="e.g. groq"
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => setDraft({ ...draft, id: e.target.value.toLowerCase().replace(/\s+/g, "-") })}
                  disabled={!draft.isNew && draft.id !== "__custom__"}
                />
              </div>
            )}

            <div className="field" style={{ margin: 0 }}>
              <label style={{ fontSize: 11, marginBottom: 4, display: "block" }}>Display Name</label>
              <input
                className="input"
                value={draft.display_name}
                onClick={(e) => e.stopPropagation()}
                onChange={(e) => setDraft({ ...draft, display_name: e.target.value })}
              />
            </div>

            <div className="field" style={{ margin: 0 }}>
              <label style={{ fontSize: 11, marginBottom: 4, display: "block" }}>Base URL</label>
              <div className="row" style={{ gap: 6, alignItems: "center" }}>
                <input
                  className="input mono"
                  value={draft.base_url}
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => {
                    const url = e.target.value;
                    setDraft({ ...draft, base_url: url, api_format: inferApiFormat(url) });
                  }}
                  placeholder="https://api.example.com/v1"
                />
                <CopyButton
                  text={draft.base_url}
                  title="Copy base URL"
                  successMsg="Base URL copied"
                  size="sm"
                  variant="inline"
                />
              </div>
            </div>

            <div className="field" style={{ margin: 0 }}>
              <label style={{ fontSize: 11, marginBottom: 4, display: "block" }}
                title="Auto-detected from Base URL. Override manually only if the provider uses a non-standard format.">
                Wire Format <span style={{ color: "var(--fg-dim)", fontWeight: 400 }}>(auto-detected)</span>
              </label>
              <select
                className="input"
                value={draft.api_format}
                onClick={(e) => e.stopPropagation()}
                onChange={(e) => setDraft({ ...draft, api_format: e.target.value as ApiFormat })}
                style={{ fontFamily: "inherit" }}
              >
                <option value="openai">OpenAI-compatible (default)</option>
                <option value="anthropic">Anthropic Messages API</option>
                <option value="gemini">Google Gemini API</option>
              </select>
            </div>

            <div className="field" style={{ margin: 0 }}>
              <label style={{ fontSize: 11, marginBottom: 4, display: "block" }}>API Key</label>
              <div className="row" style={{ gap: 6 }}>
                <input
                  className="input mono"
                  style={{ flex: 1 }}
                  type={revealedKey !== null ? "password" : showKey ? "text" : "password"}
                  value={draft.api_key_secret_id}
                  placeholder="env:OPENAI_API_KEY or paste key"
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => {
                    setDraft({ ...draft, api_key_secret_id: e.target.value });
                    if (revealedKey !== null) setRevealedKey(null);
                  }}
                  onBlur={() => { if (revealedKey !== null) setRevealedKey(null); }}
                />
                {draft.api_key_secret_id !== "" && (
                  <button
                    type="button"
                    className="btn"
                    title={revealedKey !== null || showKey ? "Hide" : "Show"}
                    aria-label={revealedKey !== null || showKey ? "Hide API key" : "Show API key"}
                    onClick={(e) => { e.stopPropagation(); toggleKeyReveal(); }}
                    style={{ padding: "0 10px", height: 34 }}
                  >
                    {revealedKey !== null || showKey ? <IconEyeOff /> : <IconEye />}
                  </button>
                )}
              </div>
              {revealedKey !== null && (
                <div className="row" style={{ gap: 6, marginTop: 6, alignItems: "center" }}>
                  <code className="mono" style={{
                    flex: 1, padding: "4px 8px", background: "var(--bg-card)",
                    borderRadius: 4, wordBreak: "break-all", fontSize: 12
                  }}>
                    {revealedKey}
                  </code>
                  <CopyButton text={revealedKey} successMsg="API key copied" variant="inline" size="sm" />
                </div>
              )}
              {!isDirect && draft.api_key_secret_id === "" && (
                <div className="hint" style={{ marginTop: 3 }}>Use <code>env:VAR_NAME</code> or paste the key directly</div>
              )}
            </div>

            <ModelAllowlistEditor
              values={draft.model_allowlist}
              onChange={(next) => setDraft({ ...draft, model_allowlist: next })}
            />
          </div>

          {/* Footer actions */}
          <div className="provider-card-footer" onClick={(e) => e.stopPropagation()}>
            <label className="row" style={{ gap: 6, color: "var(--fg-dim)", fontSize: 13, cursor: "pointer" }}>
                <input
                  type="checkbox"
                  aria-label="Enabled"
                  checked={draft.enabled}
                  onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
                />
                Enabled
            </label>
            <div className="spacer" />
            {savedMsg && (
              <span className="footer-meta" style={{ color: "var(--success, #34d399)" }}>
                {savedMsg}
              </span>
            )}
            {savedAt && !savedMsg && (
              <span
                className="saved-at"
                title={`Last saved at ${new Date(savedAt).toLocaleString()}`}
              >
                Updated <strong>{timeAgo(savedAt)}</strong>
              </span>
            )}
            {testResult && (
              <span
                className={"test-result " + (testResult.ok ? "ok" : "err")}
                title={
                  testResult.ok
                    ? `HTTP ${testResult.status} in ${testResult.latency_ms}ms`
                    : testResult.error
                }
              >
                {testResult.ok
                  ? `OK · HTTP ${testResult.status} · ${testResult.latency_ms}ms`
                  : `Failed: ${testResult.error}`}
              </span>
            )}
            <button
              className="btn"
              aria-label="Test connection"
              onClick={handleTest}
              disabled={testing || draft.isNew}
              title={
                draft.isNew
                  ? "Save the provider first, then test the connection"
                  : "Send a lightweight probe request to the upstream"
              }
              style={{ fontSize: 13 }}
            >
              <IconPlug /> {testing ? "Testing…" : "Test connection"}
            </button>
            <button
              className={"btn " + (deleteArmed ? "danger" : "")}
              onClick={handleDelete}
              disabled={deleting}
              style={{ fontSize: 13 }}
              aria-label={deleteArmed ? "Confirm delete" : "Delete provider"}
              title={
                deleteArmed
                  ? "Click again within 3 seconds to confirm"
                  : `Remove provider "${draft.display_name || draft.id}"`
              }
            >
              <IconTrash />{" "}
              {deleting ? "Removing…" : deleteArmed ? "Click again to confirm" : "Delete"}
            </button>
            <button
              className="btn primary"
              aria-label="Save provider"
              onClick={handleSave}
              disabled={saving}
              style={{ fontSize: 13, minWidth: 88 }}
            >
              {saving ? (
                <>
                  <span
                    className="spinner"
                    style={{
                      width: 12,
                      height: 12,
                      border: "2px solid currentColor",
                      borderTopColor: "transparent",
                      borderRadius: "50%",
                      display: "inline-block",
                      animation: "spin 0.7s linear infinite",
                    }}
                  />
                  Saving…
                </>
              ) : (
                "Save"
              )}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

/* ─── Model Allowlist Editor ──────────────────────────────────── */
function ModelAllowlistEditor({
  values,
  onChange,
}: {
  values: string[];
  onChange: (next: string[]) => void;
}) {
  const [draft, setDraft] = useState("");
  return (
    <div className="field" style={{ margin: 0 }}>
      <label style={{ fontSize: 11, marginBottom: 4, display: "block" }}>
        Model allowlist <span style={{ color: "var(--fg-dim)" }}>(press Enter after each model; leave blank for all)</span>
      </label>
      {values.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: 6 }}>
          {values.map((v, i) => (
            <div
              key={`${v}-${i}`}
              onClick={(e) => e.stopPropagation()}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                background: "var(--bg-elev-2, rgba(255,255,255,0.04))",
                border: "1px solid var(--border-strong, rgba(255,255,255,0.08))",
                borderRadius: "var(--radius-sm, 4px)",
                padding: "6px 10px",
              }}
            >
              <span className="mono" style={{ flex: 1, fontSize: 13 }}>{v}</span>
              <button
                className="chip-x"
                aria-label={`Remove ${v}`}
                onClick={() => {
                  const next = values.filter((_, j) => j !== i);
                  onChange(next);
                }}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
      <input
        className="input mono"
        value={draft}
        onClick={(e) => e.stopPropagation()}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === ",") {
            e.preventDefault();
            const t = draft.trim();
            if (t && !values.includes(t)) onChange([...values, t]);
            setDraft("");
          } else if (e.key === "Backspace" && !draft && values.length) {
            onChange(values.slice(0, -1));
          }
        }}
        placeholder="model id, press Enter to add"
      />
    </div>
  );
}

/* ─── Providers page ─────────────────────────────────────────── */
export function Providers() {
  const [data, setData] = useState<ProvidersResponse | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // Pending "new" cards not yet saved
  const [pending, setPending] = useState<DraftProvider[]>([]);
  const [pageToast, setPageToast] = useState<
    { kind: "ok" | "err"; msg: string } | null
  >(null);

  const reload = async () => {
    try {
      setData(await api.providers());
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => { reload(); }, []);

  // Convert backend ProviderInfo list into DraftProvider, filtering out unconfigured ones.
  // Dedupe by id so a stale first-class + stale custom row that share an id never
  // appears twice in the rendered list.
  const configured: DraftProvider[] = (() => {
    const seen = new Map<string, DraftProvider>();
    for (const p of (data?.providers ?? []) as ProviderInfo[]) {
      if (!p.base_url || p.base_url.trim() === "") continue;
      if (seen.has(p.id)) continue;
      seen.set(p.id, {
        id: p.id,
        display_name: p.display_name,
        base_url: p.base_url,
        api_key_secret_id: p.api_key_secret_id ?? "",
        enabled: p.enabled,
        model_allowlist: p.model_allowlist,
        api_format: (p.api_format ?? inferApiFormat(p.base_url)) as ApiFormat,
      });
    }
    return Array.from(seen.values());
  })();

  const existingIds = new Set([
    ...configured.map((p) => p.id),
    ...pending.map((p) => p.id),
  ]);

  const handleAddPreset = (preset: Preset) => {
    // Don't add if already configured
    if (existingIds.has(preset.id) && preset.id !== "__custom__") return;
    const draft: DraftProvider = {
      id: preset.id === "__custom__" ? "" : preset.id,
      display_name: preset.name === "Custom Provider" ? "" : preset.name,
      base_url: preset.base_url,
      api_key_secret_id: "",
      enabled: true,
      model_allowlist: [],
      api_format: preset.api_format,
      isNew: true,
    };
    setPending((prev) => {
      // Defensive: dedupe pending by id so a double-click never adds the same preset twice
      if (draft.id && prev.some((p) => p.id === draft.id)) return prev;
      return [...prev, draft];
    });
  };

  const handleSave = async (d: DraftProvider, isNew: boolean) => {
    const id = d.id.trim().toLowerCase();
    if (!id) throw new Error("Provider ID is required");
    if (!d.base_url.trim()) throw new Error("Base URL is required");

    const isFirstClass = FIRST_CLASS.includes(id);
    const payload = {
      display_name: d.display_name || id,
      base_url: d.base_url.trim(),
      api_key_secret_id: d.api_key_secret_id.trim() || null,
      enabled: d.enabled,
      model_allowlist: d.model_allowlist,
      api_format: d.api_format,
    };

    const patch = isFirstClass
      ? { providers: { [id]: payload } }
      : { providers: { custom: { [id]: payload } } };

    await api.patchSettings(patch);

    // Auto-default: if this is the first provider being configured,
    // automatically set it as the default provider and pick a model
    // if the allowlist has one.
    if (isNew && configured.length === 0) {
      const defaults: any = {
        default_provider: id,
      };
      // If the provider has a model allowlist, use the first model
      if (d.model_allowlist && d.model_allowlist.length > 0) {
        defaults.default_model = d.model_allowlist[0];
      }
      await api.patchSettings({ defaults });
    }

    if (isNew) {
      setPending((prev) => prev.filter((p) => p !== d && !(p.id === d.id)));
    }
    await reload();
    const isFirstProviderSetup = isNew && configured.length === 0;
    setPageToast({
      kind: "ok",
      msg: isFirstProviderSetup
        ? `Saved "${payload.display_name}" and set as default`
        : `Saved "${payload.display_name}"`,
    });
    setTimeout(() => setPageToast(null), 2000);
  };

  const handleDelete = async (d: DraftProvider, isNew: boolean) => {
    if (isNew) {
      setPending((prev) => prev.filter((p) => p !== d));
      return;
    }
    const id = d.id;
    const isFirstClass = FIRST_CLASS.includes(id);
    if (isFirstClass) {
      // For first-class providers, clear base_url + disable so they disappear from the filtered list
      const patch = { providers: { [id]: { base_url: "", api_key_secret_id: null, enabled: false } } };
      await api.patchSettings(patch as any);
    } else {
      const patch = { providers: { custom: { [id]: { delete: true } } } };
      await api.patchSettings(patch as any);
    }
    await reload();
    setPageToast({ kind: "ok", msg: `Removed "${d.display_name || id}"` });
    setTimeout(() => setPageToast(null), 2000);
  };

  // Split the cards into built-in (first-class) and custom groups
  const builtInCards = configured.filter((d) => FIRST_CLASS.includes(d.id));
  const customCards = configured.filter((d) => !FIRST_CLASS.includes(d.id));
  const pendingCards = pending.map((d) => ({ d, isNew: true }));

  // A "Built-in" header should only appear when at least one first-class
  // provider exists OR we have pending new cards that target a first-class id
  // (rare, but possible if the user opens the New card with a preset id).
  const showBuiltInHeader =
    builtInCards.length > 0 ||
    pendingCards.some(({ d }) => FIRST_CLASS.includes(d.id));
  const showCustomHeader = customCards.length > 0 || pendingCards.length > 0;

  if (err)
    return (
      <div className="empty">
        <div className="empty-title">Could not load providers</div>
        <div className="empty-sub">{err}</div>
      </div>
    );

  return (
    <>
      {/* Page header */}
      <div className="page-header">
        <h1>Providers</h1>
        <div className="sub">Upstream AI providers the router can forward to.</div>
        <div className="spacer" />
        <AddProviderSearch existingIds={existingIds} onSelect={handleAddPreset} />
        <button className="btn" onClick={reload} style={{ marginLeft: 8 }}>
          <IconRefresh /> Refresh
        </button>
      </div>

      {/* Provider cards */}
      {configured.length === 0 && pendingCards.length === 0 && !data ? (
        <div className="empty" style={{ padding: 48 }}>
          <div className="empty-title">Loading…</div>
        </div>
      ) : configured.length === 0 && pendingCards.length === 0 ? (
        <EmptyProvidersState onAdd={handleAddPreset} />
      ) : (
        <>
          {showBuiltInHeader && (
            <div className="provider-section-head">
              <span>Built-in providers</span>
              <span className="count">{builtInCards.length}</span>
              <span className="divider" />
            </div>
          )}
          {(builtInCards.length > 0 ||
            pendingCards.some(({ d }) => FIRST_CLASS.includes(d.id))) && (
            <div className="grid cols-2" style={{ marginBottom: 8 }}>
              {[
                ...builtInCards.map((d) => ({ d, isNew: false })),
                ...pendingCards.filter(({ d }) => FIRST_CLASS.includes(d.id)),
              ].map(({ d, isNew }) => (
                <ProviderCard
                  key={`b-${isNew ? `new-${d.id || pending.indexOf(d)}` : d.id}`}
                  draft={{ ...d, isNew }}
                  onSave={(updated) => handleSave(updated, isNew)}
                  onDelete={() => handleDelete(d, isNew)}
                />
              ))}
            </div>
          )}

          {showCustomHeader && (
            <div className="provider-section-head">
              <span>Custom providers</span>
              <span className="count">
                {customCards.length + pendingCards.filter(({ d }) => !FIRST_CLASS.includes(d.id)).length}
              </span>
              <span className="divider" />
            </div>
          )}
          {(customCards.length > 0 ||
            pendingCards.some(({ d }) => !FIRST_CLASS.includes(d.id))) && (
            <div className="grid cols-2" style={{ marginBottom: 24 }}>
              {[
                ...customCards.map((d) => ({ d, isNew: false })),
                ...pendingCards.filter(({ d }) => !FIRST_CLASS.includes(d.id)),
              ].map(({ d, isNew }) => (
                <ProviderCard
                  key={`c-${isNew ? `new-${d.id || pending.indexOf(d)}` : d.id}`}
                  draft={{ ...d, isNew }}
                  onSave={(updated) => handleSave(updated, isNew)}
                  onDelete={() => handleDelete(d, isNew)}
                />
              ))}
            </div>
          )}
        </>
      )}

      {/* Models table */}
      {data && data.models.length > 0 && (
        <>
          <div className="section-head">
            <h2>Models</h2>
            <span className="sub">
              {data.models.length} model{data.models.length === 1 ? "" : "s"} registered across all providers.
            </span>
          </div>
          <div className="card" style={{ padding: 0, marginBottom: 24 }}>
            <table className="table">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Provider</th>
                  <th>Context</th>
                  <th>Max out</th>
                  <th>Tools</th>
                  <th>Vision</th>
                  <th>Stream</th>
                </tr>
              </thead>
              <tbody>
                {data.models.map((m) => (
                  <tr key={`${m.provider}:${m.id}`}>
                    <td className="mono">{m.id}</td>
                    <td>{m.provider}</td>
                    <td>{m.context_window.toLocaleString()}</td>
                    <td>{m.max_output_tokens.toLocaleString()}</td>
                    <td>{m.supports_tools ? "✓" : ""}</td>
                    <td>{m.supports_vision ? "✓" : ""}</td>
                    <td>{m.supports_streaming ? "✓" : ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {pageToast && (
        <div className={"toast " + (pageToast.kind === "ok" ? "ok" : "err")}>
          {pageToast.msg}
        </div>
      )}
    </>
  );
}

/* ─── Empty state for Providers page ──────────────────────────── */
function EmptyProvidersState({ onAdd }: { onAdd: (preset: Preset) => void }) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        padding: "64px 0",
        gap: 14,
        color: "var(--fg-dim)",
        textAlign: "center",
      }}
    >
      <svg
        width="56"
        height="56"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        opacity="0.4"
      >
        <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />
        <path d="M3.27 6.96L12 12.01l8.73-5.05" />
        <path d="M12 22.08V12" />
      </svg>
      <div style={{ fontWeight: 500, fontSize: 18, color: "var(--fg)" }}>
        No providers configured
      </div>
      <div style={{ fontSize: 13, maxWidth: 360 }}>
        AutoRouter needs at least one upstream provider to forward requests to.
        Pick a preset from the catalog to get started in seconds.
      </div>
      <button
        className="btn primary"
        aria-label="Add your first provider"
        onClick={() => onAdd(PRESETS[3] /* OpenRouter, the friendliest free default */)}
        style={{ marginTop: 6, fontSize: 14, padding: "8px 16px" }}
      >
        <IconPlus /> Add your first provider
      </button>
      <div style={{ fontSize: 11, color: "var(--fg-faint)" }}>
        Or click <strong>Add Provider</strong> above to pick a different one.
      </div>
    </div>
  );
}
