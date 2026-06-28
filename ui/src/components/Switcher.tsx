// Global Provider/Model Switcher overlay.
//
// A Windows-Alt-Tab-style picker that pops up on top of every other
// surface (above the Tauri main window, above the tray, above other
// apps' windows) when the user presses the configured global keyboard
// shortcut (`Ctrl+Shift+P`, registered in the desktop shell).
//
// Lifecycle:
//   1. The Tauri shell emits a `show-switcher` event when the chord
//      is pressed (see `crates/autorouter-desktop/src/lib.rs`,
//      `SWITCHER_SHORTCUT`).
//   2. `Switcher` subscribes to the event in its mount effect and
//      flips a single boolean `open` state.
//   3. While `open`, the overlay is mounted into a fixed-position
//      wrapper at `z-index: 9999999` so it sits above every other
//      element in the document tree. The wrapper owns the keyboard
//      listener so the page underneath can't intercept keys.
//   4. Enter dispatches `api.setDefaultProviderModel(...)` and
//      closes; Esc / outside click / cancel button all just close.
//
// Data flow:
//   - Providers / models come from `GET /ui/providers` (or the Tauri
//     `cmd_providers` mirror). The same response the Dashboard
//     uses — see `cmd_providers` in the desktop crate for the
//     shape contract.
//   - Current default comes from `GET /ui/settings`
//     (`defaults.default_provider` + `defaults.default_model`).
//   - Recents + favorites are local to the browser, persisted in
//     `localStorage`. They are best-effort — losing them is not a
//     data-loss event.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "../tauri";
import { api } from "../api";
import type {
  AppConfig,
  ModelInfo,
  ProviderInfo,
  ProvidersResponse,
} from "../types";
import { useToast } from "./Toast";

// ─── Constants ────────────────────────────────────────────────

const RECENTS_KEY = "autorouter:switcher:recents";
const FAVORITES_KEY = "autorouter:switcher:favorites";
const MAX_RECENTS = 8;

/** Provider badge colour per wire format / provider id. The colours
 *  match the convention from the spec (OpenAI green, Anthropic
 *  orange, Gemini blue, OpenRouter purple, Custom gray). */
const BADGE_COLOR: Record<string, string> = {
  openai: "#22c55e",      // green
  anthropic: "#f97316",   // orange
  gemini: "#3b82f6",      // blue
  openrouter: "#a855f7",  // purple
  ollama: "#94a3b8",      // gray
  lmstudio: "#94a3b8",    // gray
  groq: "#ec4899",        // pink
  together: "#14b8a6",    // teal
  mistral: "#fb923c",     // light orange
  deepseek: "#0ea5e9",    // sky
  perplexity: "#06b6d4",  // cyan
  xai: "#64748b",         // slate
  cohere: "#84cc16",      // lime
  fireworks: "#f43f5e",   // rose
  anyscale: "#10b981",    // emerald
};
const FALLBACK_BADGE_COLOR = "#64748b"; // slate gray for custom

/** Friendly display name; fall back to id if the operator hasn't
 *  named the provider. */
function providerDisplayName(p: ProviderInfo): string {
  if (p.display_name && p.display_name.trim().length > 0) return p.display_name;
  return p.id;
}

/** Look up the colour for a provider. Tries the id first, then the
 *  wire format. Falls back to slate. */
function providerColor(p: ProviderInfo): string {
  return (
    BADGE_COLOR[p.id.toLowerCase()] ??
    BADGE_COLOR[p.api_format?.toLowerCase() ?? ""] ??
    FALLBACK_BADGE_COLOR
  );
}

/** Tiny fuzzy match: every char in the needle must appear in the
 *  haystack in order. Lowercase, case-insensitive. Returns true /
 *  false. This is intentionally simpler than a full fuzzy library —
 *  the dataset is small and we want zero new dependencies. */
function fuzzy(haystack: string, needle: string): boolean {
  if (needle.length === 0) return true;
  const h = haystack.toLowerCase();
  const n = needle.toLowerCase();
  let i = 0;
  for (let j = 0; j < h.length && i < n.length; j += 1) {
    if (h[j] === n[i]) i += 1;
  }
  return i === n.length;
}

interface RecentEntry {
  provider: string;
  model: string;
  ts: number;
}

function loadRecents(): RecentEntry[] {
  try {
    const raw = localStorage.getItem(RECENTS_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return [];
    return arr
      .filter(
        (x): x is RecentEntry =>
          x &&
          typeof x.provider === "string" &&
          typeof x.model === "string" &&
          typeof x.ts === "number",
      )
      .slice(0, MAX_RECENTS);
  } catch {
    return [];
  }
}

function saveRecents(recents: RecentEntry[]) {
  try {
    localStorage.setItem(RECENTS_KEY, JSON.stringify(recents.slice(0, MAX_RECENTS)));
  } catch {
    /* storage quota / private mode — drop silently */
  }
}

function loadFavorites(): string[] {
  try {
    const raw = localStorage.getItem(FAVORITES_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return [];
    return arr.filter((x): x is string => typeof x === "string");
  } catch {
    return [];
  }
}

function saveFavorites(favs: string[]) {
  try {
    localStorage.setItem(FAVORITES_KEY, JSON.stringify(favs));
  } catch {
    /* ignore */
  }
}

/** Composite key for a (provider, model) pair. The switcher works
 *  on these everywhere — both for the recent list and for matching
 *  the current default. */
function pairKey(provider: string, model: string): string {
  return `${provider}\u0000${model}`;
}

interface DataState {
  providers: ProviderInfo[];
  models: ModelInfo[];
  defaults: { provider: string; model: string } | null;
}

async function fetchData(): Promise<DataState> {
  const [providersResp, settings] = await Promise.all([
    api.providers() as Promise<ProvidersResponse>,
    api.settings() as Promise<AppConfig>,
  ]);
  return {
    providers: providersResp.providers ?? [],
    models: providersResp.models ?? [],
    defaults: {
      provider: settings.defaults?.default_provider ?? "",
      model: settings.defaults?.default_model ?? "",
    },
  };
}

// ─── Main component ───────────────────────────────────────────

export function Switcher() {
  const isStandalone = useMemo(() => {
    return typeof window !== "undefined" && new URL(window.location.href).searchParams.get("page") === "switcher";
  }, []);

  // `open` is the only piece of state that drives rendering —
  // everything else is reset on close to avoid stale state.
  const [open, setOpen] = useState(isStandalone);
  const [data, setData] = useState<DataState | null>(null);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [activePane, setActivePane] = useState<"provider" | "model">("provider");
  const [providerIdx, setProviderIdx] = useState(0);
  const [modelIdx, setModelIdx] = useState(0);
  const [recents, setRecents] = useState<RecentEntry[]>([]);
  const [favorites, setFavorites] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const { show } = useToast();

  useEffect(() => {
    if (isStandalone) {
      document.body.style.background = "transparent";
    }
  }, [isStandalone]);

  // Subscribe to the Tauri shortcut event. The listener survives
  // every render; it just flips `open`.
  //
  // Two paths:
  //   1. Inside the Tauri shell — listen for the `show-switcher`
  //      event that `run()` emits when the user presses the chord.
  //   2. Outside the shell (e.g. `vite dev` or the HTTP-served UI)
  //      — fall back to a window-level keydown so the overlay can
  //      still be exercised during UI development.
  //
  // Why we use `isTauri()` instead of `try/catch` around `listen`:
  // `listen()` returns a Promise that resolves only after the IPC
  // round-trip succeeds. In a plain browser the failure happens
  // asynchronously (the `invoke` shim throws on
  // `window.__TAURI_INTERNALS__`), so a sync `try/catch` would
  // never see the error and the browser-fallback handler would
  // never be installed. We branch synchronously on
  // `isTauri()` instead.
  useEffect(() => {
    if (!isTauri()) {
      const onKey = (e: KeyboardEvent) => {
        const mod = e.metaKey || e.ctrlKey;
        if (!mod || !e.shiftKey || e.altKey) return;
        if (e.key.toLowerCase() === "p") {
          e.preventDefault();
          setOpen(true);
        }
      };
      window.addEventListener("keydown", onKey);
      return () => window.removeEventListener("keydown", onKey);
    }
    let unlistenP: Promise<() => void> | null = null;
    let cancelled = false;
    (async () => {
      try {
        unlistenP = listen<void>("show-switcher", () => {
          setOpen(true);
          setQuery("");
          setActivePane("provider");
          setProviderIdx(0);
          setModelIdx(0);
          setErr(null);
          setBusy(false);
          requestAnimationFrame(() => {
            inputRef.current?.focus();
          });
        });
        if (cancelled && unlistenP) {
          unlistenP.then((fn) => fn()).catch(() => undefined);
          unlistenP = null;
        }
      } catch {
        /* listen() rejected synchronously — nothing to clean up. */
      }
    })();
    return () => {
      cancelled = true;
      if (unlistenP) {
        unlistenP.then((fn) => fn()).catch(() => undefined);
      }
    };
  }, []);

  // Reload localStorage-backed recents / favorites on every open
  // so multi-tab edits show up.
  useEffect(() => {
    if (!open) return;
    setRecents(loadRecents());
    setFavorites(loadFavorites());
  }, [open]);

  // When the overlay opens: fetch the data if we don't have it,
  // focus the input, and reset indices so the user lands at the
  // current default.
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setActivePane("provider");
    setErr(null);
    setBusy(false);
    let cancelled = false;
    (async () => {
      if (!data) {
        setLoading(true);
        try {
          const next = await fetchData();
          if (!cancelled) setData(next);
        } catch (e) {
          if (!cancelled) setErr(String(e));
        } finally {
          if (!cancelled) setLoading(false);
        }
      }
      if (cancelled) return;
      // Focus the search input on the next frame so the
      // `autofocus`-equivalent actually wins against the
      // `setOpen(true)` render cycle.
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    })();
    return () => {
      cancelled = true;
    };
    // We deliberately key off `open` only — re-running this on
    // every `data` update would steal focus from the input.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Compute visible providers + models whenever the query / data
  // changes. Memoised because the fuzzy scan is O(n*m).
  const visibleProviders = useMemo(() => {
    const list = data?.providers ?? [];
    const filtered = list.filter(
      (p) =>
        fuzzy(providerDisplayName(p), query) ||
        fuzzy(p.id, query) ||
        fuzzy(p.api_format, query),
    );
    // Sort: favorites first (providers with at least one favorited
    // model), then current default, then alphabetical.
    const favSet = new Set(favorites);
    const currentKey = data?.defaults
      ? pairKey(data.defaults.provider, data.defaults.model)
      : "";
    return filtered.slice().sort((a, b) => {
      const aFav = (data?.models ?? []).some((m) => m.provider === a.id && favSet.has(pairKey(a.id, m.id)));
      const bFav = (data?.models ?? []).some((m) => m.provider === b.id && favSet.has(pairKey(b.id, m.id)));
      if (aFav !== bFav) return aFav ? -1 : 1;
      const aCurrent = currentKey && a.id === data!.defaults!.provider ? 1 : 0;
      const bCurrent = currentKey && b.id === data!.defaults!.provider ? 1 : 0;
      if (aCurrent !== bCurrent) return bCurrent - aCurrent;
      return providerDisplayName(a).localeCompare(providerDisplayName(b));
    });
  }, [data, query, favorites]);

  const visibleModels = useMemo(() => {
    if (!data) return [];
    const provider = visibleProviders[providerIdx];
    const list = provider
      ? data.models.filter((m) => m.provider === provider.id)
      : data.models;
    const filtered = list.filter(
      (m) =>
        fuzzy(m.id, query) ||
        fuzzy(m.provider, query),
    );
    const currentKey = data.defaults
      ? pairKey(data.defaults.provider, data.defaults.model)
      : "";
    return filtered.slice().sort((a, b) => {
      const aFav = favorites.includes(pairKey(a.provider, a.id)) ? 1 : 0;
      const bFav = favorites.includes(pairKey(b.provider, b.id)) ? 1 : 0;
      if (aFav !== bFav) return bFav - aFav;
      const aCurrent = currentKey === pairKey(a.provider, a.id) ? 1 : 0;
      const bCurrent = currentKey === pairKey(b.provider, b.id) ? 1 : 0;
      if (aCurrent !== bCurrent) return bCurrent - aCurrent;
      return a.id.localeCompare(b.id);
    });
  }, [data, visibleProviders, providerIdx, query, favorites]);

  // Clamp the cursor whenever the visible list shrinks (e.g. after
  // typing in the filter).
  useEffect(() => {
    if (providerIdx >= visibleProviders.length) {
      setProviderIdx(Math.max(0, visibleProviders.length - 1));
    }
  }, [visibleProviders.length, providerIdx]);
  useEffect(() => {
    if (modelIdx >= visibleModels.length) {
      setModelIdx(Math.max(0, visibleModels.length - 1));
    }
  }, [visibleModels.length, modelIdx]);

  const close = useCallback(async () => {
    if (isStandalone) {
      if (isTauri()) {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const win = getCurrentWindow();
        await win.hide();
      }
    } else {
      setOpen(false);
    }
  }, [isStandalone]);

  const apply = useCallback(
    async (provider: ProviderInfo | null, model: ModelInfo | null) => {
      if (!provider || !model) return;
      setBusy(true);
      try {
        await api.setDefaultProviderModel(provider.id, model.id);
        const entry: RecentEntry = {
          provider: provider.id,
          model: model.id,
          ts: Date.now(),
        };
        const next = [
          entry,
          ...recents.filter(
            (r) => !(r.provider === provider.id && r.model === model.id),
          ),
        ].slice(0, MAX_RECENTS);
        setRecents(next);
        saveRecents(next);
        show(`Default → ${providerDisplayName(provider)} · ${model.id}`, "ok");
        // Update cached data so the "current" badge moves without a
        // round-trip and so reopening the overlay shows the new state.
        setData((prev) =>
          prev
            ? {
                ...prev,
                defaults: { provider: provider.id, model: model.id },
              }
            : prev,
        );
        await close();
        setBusy(false);
      } catch (e) {
        show(`Switcher: ${String(e)}`, "err");
        setBusy(false);
      }
    },
    [recents, show, close],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (!open) return;
      // Esc always closes.
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
        return;
      }
      // Enter applies the current selection.
      if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        const provider = visibleProviders[providerIdx] ?? null;
        const model = visibleModels[modelIdx] ?? null;
        if (provider && model) {
          apply(provider, model);
        }
        return;
      }
      // Tab toggles the focused pane.
      if (e.key === "Tab") {
        e.preventDefault();
        e.stopPropagation();
        setActivePane((p) => (p === "provider" ? "model" : "provider"));
        return;
      }
      // Arrow nav within the active pane.
      if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        if (activePane === "provider") {
          setProviderIdx((i) =>
            Math.min(i + 1, Math.max(0, visibleProviders.length - 1)),
          );
        } else {
          setModelIdx((i) =>
            Math.min(i + 1, Math.max(0, visibleModels.length - 1)),
          );
        }
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        if (activePane === "provider") {
          setProviderIdx((i) => Math.max(0, i - 1));
        } else {
          setModelIdx((i) => Math.max(0, i - 1));
        }
        return;
      }
      // Right / Left mirrors Tab and goes back.
      if (e.key === "ArrowRight") {
        e.preventDefault();
        e.stopPropagation();
        setActivePane("model");
        return;
      }
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        e.stopPropagation();
        setActivePane("provider");
        return;
      }
    },
    [open, close, apply, activePane, providerIdx, modelIdx, visibleProviders, visibleModels],
  );

  // While the overlay is open, capture every key globally so the
  // page underneath can't swallow them. We listen on the window so
  // a stray Tab on the search input doesn't lose focus.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      onKeyDown({
        key: e.key,
        preventDefault: () => e.preventDefault(),
        stopPropagation: () => e.stopPropagation(),
      } as unknown as React.KeyboardEvent<HTMLDivElement>);
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true } as any);
  }, [open, onKeyDown]);

  const toggleFavorite = useCallback(
    (providerId: string, modelId: string) => {
      const key = pairKey(providerId, modelId);
      setFavorites((prev) => {
        const next = prev.includes(key)
          ? prev.filter((k) => k !== key)
          : [...prev, key];
        saveFavorites(next);
        return next;
      });
    },
    [],
  );

  if (!open) return null;

  const currentKey =
    data?.defaults && data.defaults.provider && data.defaults.model
      ? pairKey(data.defaults.provider, data.defaults.model)
      : null;
  const totalProviders = data?.providers.length ?? 0;
  const totalModels = data?.models.length ?? 0;

  return (
    <div
      className={"switcher-root" + (isStandalone ? " standalone" : "")}
      role="dialog"
      aria-modal="true"
      aria-label="Provider and model switcher"
      onMouseDown={(e) => {
        // Click on the dim backdrop closes; click on the panel
        // itself (or its descendants) does not. We use
        // mousedown so a user who starts a selection drag outside
        // the panel and releases inside doesn't accidentally
        // dismiss the overlay.
        if (e.target === e.currentTarget) close();
      }}
      onKeyDown={onKeyDown}
    >
      <div className={"switcher-panel" + (isStandalone ? " standalone" : "")} role="document">
        <div className="switcher-head">
          <div className="switcher-search">
            <input
              ref={inputRef}
              className="switcher-input"
              type="text"
              placeholder="Search providers or models…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              // Keep native key handling for the actual text
              // editing, but the window-level capture above
              // ensures our nav keys still fire even when the
              // input isn't focused.
              spellCheck={false}
              autoComplete="off"
              autoCorrect="off"
              aria-label="Switcher search"
            />
          </div>
          <div className="switcher-hint-top">
            <span className="badge ok">Esc dismiss</span>
          </div>
        </div>

        {loading && !data ? (
          <div className="switcher-body switcher-empty">
            Loading providers…
          </div>
        ) : err ? (
          <div className="switcher-body switcher-empty">
            <div className="empty-title">Failed to load</div>
            <div className="empty-sub">{err}</div>
          </div>
        ) : !data || totalProviders === 0 ? (
          <div className="switcher-body switcher-empty">
            <div className="empty-title">No providers configured</div>
            <div className="empty-sub">
              Add one in the Providers page to start routing requests.
            </div>
          </div>
        ) : (
          <>
            <div className="switcher-panes">
              <div
                className={
                  "switcher-pane switcher-pane-providers" +
                  (activePane === "provider" ? " active" : "")
                }
                onMouseEnter={() => setActivePane("provider")}
              >
                <div className="switcher-pane-head">
                  <span>Providers</span>
                  <span className="switcher-count">
                    {visibleProviders.length}
                  </span>
                </div>
                <div className="switcher-list" role="listbox" aria-label="Providers">
                  {visibleProviders.length === 0 && (
                    <div className="switcher-list-empty">
                      No providers match “{query}”.
                    </div>
                  )}
                  {visibleProviders.map((p, idx) => {
                    const isCurrent =
                      data.defaults?.provider === p.id &&
                      visibleModels.some(
                        (m) => m.id === data.defaults?.model,
                      );
                    return (
                      <div
                        key={p.id}
                        role="option"
                        aria-selected={idx === providerIdx}
                        className={
                          "switcher-row" +
                          (idx === providerIdx ? " selected" : "") +
                          (isCurrent ? " current" : "")
                        }
                        onMouseEnter={() => {
                          setActivePane("provider");
                          setProviderIdx(idx);
                        }}
                        onClick={() => {
                          setProviderIdx(idx);
                          // Clicking a provider drops the cursor
                          // into the model pane so a follow-up
                          // Enter applies both halves of the pair.
                          setActivePane("model");
                        }}
                      >
                        <span
                          className="switcher-dot"
                          style={{ background: providerColor(p) }}
                          aria-hidden
                        />
                        <span className="switcher-row-label">
                          {providerDisplayName(p)}
                        </span>
                        {isCurrent && (
                          <span className="badge info switcher-row-badge">
                            current
                          </span>
                        )}
                        {!p.enabled && (
                          <span className="badge warn switcher-row-badge">
                            disabled
                          </span>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>

              <div
                className={
                  "switcher-pane switcher-pane-models" +
                  (activePane === "model" ? " active" : "")
                }
                onMouseEnter={() => setActivePane("model")}
              >
                <div className="switcher-pane-head">
                  <span>Models</span>
                  <span className="switcher-count">
                    {visibleModels.length}
                  </span>
                </div>
                <div className="switcher-list" role="listbox" aria-label="Models">
                  {visibleModels.length === 0 && (
                    <div className="switcher-list-empty">
                      {visibleProviders[providerIdx]
                        ? `No models for ${providerDisplayName(
                            visibleProviders[providerIdx],
                          )}.`
                        : "No models match the filter."}
                    </div>
                  )}
                  {visibleModels.map((m, idx) => {
                    const isCurrent = currentKey === pairKey(m.provider, m.id);
                    const isFav = favorites.includes(pairKey(m.provider, m.id));
                    return (
                      <div
                        key={`${m.provider}:${m.id}`}
                        role="option"
                        aria-selected={idx === modelIdx}
                        className={
                          "switcher-row" +
                          (idx === modelIdx ? " selected" : "") +
                          (isCurrent ? " current" : "")
                        }
                        onMouseEnter={() => {
                          setActivePane("model");
                          setModelIdx(idx);
                        }}
                        onClick={() => {
                          setActivePane("model");
                          setModelIdx(idx);
                        }}
                      >
                        <button
                          type="button"
                          className={
                            "switcher-star" + (isFav ? " on" : "")
                          }
                          aria-label={
                            isFav ? "Remove from favorites" : "Add to favorites"
                          }
                          title={isFav ? "Unfavorite" : "Favorite"}
                          onClick={(e) => {
                            e.stopPropagation();
                            toggleFavorite(m.provider, m.id);
                          }}
                        >
                          {isFav ? "★" : "☆"}
                        </button>
                        <span className="switcher-row-label mono">
                          {m.id}
                        </span>
                        {isCurrent && (
                          <span className="badge info switcher-row-badge">
                            current
                          </span>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>
            </div>

            {recents.length > 0 && (
              <div className="switcher-recents">
                <span className="switcher-recents-label">Recent</span>
                {recents.map((r) => {
                  const provider = data.providers.find(
                    (p) => p.id === r.provider,
                  );
                  const stillExists = data.models.some(
                    (m) => m.provider === r.provider && m.id === r.model,
                  );
                  if (!stillExists) return null;
                  return (
                    <button
                      type="button"
                      key={`${r.provider}:${r.model}`}
                      className="switcher-recent-pill"
                      onClick={() => {
                        if (provider) {
                          setData((prev) =>
                            prev
                              ? {
                                  ...prev,
                                  defaults: {
                                    provider: r.provider,
                                    model: r.model,
                                  },
                                }
                              : prev,
                          );
                          apply(provider, { id: r.model, provider: r.provider } as ModelInfo);
                        }
                      }}
                      title={`${r.provider} · ${r.model}`}
                    >
                      <span
                        className="switcher-dot small"
                        style={{
                          background: provider ? providerColor(provider) : FALLBACK_BADGE_COLOR,
                        }}
                        aria-hidden
                      />
                      {provider ? providerDisplayName(provider) : r.provider}
                      <span className="switcher-recent-sep">·</span>
                      <span className="mono">{r.model}</span>
                    </button>
                  );
                })}
              </div>
            )}

            <div className="switcher-footer">
              <span className="switcher-kbd-hint">
                <kbd>↑</kbd>
                <kbd>↓</kbd> navigate
              </span>
              <span className="switcher-kbd-hint">
                <kbd>Tab</kbd> switch pane
              </span>
              <span className="switcher-kbd-hint">
                <kbd>⏎</kbd> apply
              </span>
              <span className="switcher-kbd-hint">
                <kbd>Esc</kbd> dismiss
              </span>
              <span className="switcher-grow" />
              <span className="switcher-meta">
                {totalProviders} providers · {totalModels} models
              </span>
            </div>
          </>
        )}
        {busy && <div className="switcher-busy" aria-hidden />}
      </div>
    </div>
  );
}