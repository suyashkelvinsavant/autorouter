import { invoke } from "@tauri-apps/api/core";
import type {
  AppConfig,
  LogsResponse,
  ProvidersResponse,
  SessionsResponse,
  StatusResponse,
} from "./types";

// The Tauri shell calls into Rust commands directly. Outside the
// shell (e.g. during `vite dev`) we fall back to plain HTTP fetch
// against the gateway.
let apiBase =
  (import.meta as any).env?.VITE_AUTOROUTER_API ||
  "http://127.0.0.1:4073";

function isTauri(): boolean {
  return typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
}

export function setApiBase(url: string) {
  apiBase = url.replace(/\/+$/, "");
}

export function getApiBase() {
  return apiBase;
}

async function http<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${apiBase}${path}`, {
    headers: { "content-type": "application/json", ...(init?.headers || {}) },
    ...init,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${text}`);
  }
  return res.json();
}

async function httpText(path: string, init?: RequestInit): Promise<string> {
  const res = await fetch(`${apiBase}${path}`, init);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${text}`);
  }
  return res.text();
}

/**
 * Like `httpText` but also returns the response headers so the
 * caller can detect server-side annotations such as the
 * `x-autorouter-auth-token-redacted` flag set by `/ui/export`.
 */
async function httpTextWithHeaders(
  path: string,
  init?: RequestInit,
): Promise<{ text: string; headers: Headers }> {
  const res = await fetch(`${apiBase}${path}`, init);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${text}`);
  }
  return { text: await res.text(), headers: res.headers };
}

// Command names exposed by the Tauri shell. Keep these in sync
// with `crates/autorouter-desktop/src/lib.rs`.
const CMDS = {
  status: "get_status",
  providers: "cmd_providers",
  sessions: "cmd_sessions",
  settingsGet: "cmd_settings_get",
  settingsPatch: "cmd_settings_patch",
  setDefaultProviderModel: "set_default_provider_model",
  logs: "cmd_logs",
  restart: "cmd_restart",
  serverInfo: "cmd_server_info",
  routing: "cmd_routing",
  routingPatch: "cmd_routing_patch",
  health: "cmd_health",
  events: "cmd_events",
  analytics: "cmd_analytics",
  debug: "cmd_debug",
  importConfig: "cmd_import_config",
  exportConfig: "cmd_export_config",
  checkUpdate: "cmd_check_update",
  toolProfiles: "cmd_tool_profiles",
  toolProfileSave: "cmd_tool_profile_save",
  toolTest: "cmd_tool_test",
  providerTest: "cmd_provider_test",
  secretGet: "cmd_secret_get",
  secretPut: "cmd_secret_put",
} as const;

async function tauri<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return (await invoke(cmd, args)) as T;
}

export const api = {
  status: async (): Promise<StatusResponse> => {
    if (isTauri()) return tauri(CMDS.status);
    return http<StatusResponse>("/ui/status");
  },
  providers: async (): Promise<ProvidersResponse> => {
    if (isTauri()) return tauri(CMDS.providers);
    return http<ProvidersResponse>("/ui/providers");
  },
  sessions: async (): Promise<SessionsResponse> => {
    if (isTauri()) return tauri(CMDS.sessions);
    return http<SessionsResponse>("/ui/sessions");
  },
  settings: async (): Promise<AppConfig> => {
    if (isTauri()) return tauri(CMDS.settingsGet);
    return http<AppConfig>("/ui/settings");
  },
  patchSettings: async (patch: unknown): Promise<AppConfig> => {
    if (isTauri()) return tauri(CMDS.settingsPatch, { patch });
    return http<AppConfig>("/ui/settings", {
      method: "PATCH",
      body: JSON.stringify(patch),
    });
  },
  /**
   * One-shot helper for the provider/model switcher overlay.
   * PATCHes `{defaults: {default_provider, default_model}}` so a
   * single keystroke can flip the active default. Outside the
   * Tauri shell (vite dev, headless mode), the same PATCH shape
   * is sent to the HTTP gateway so the overlay works identically
   * during UI development.
   *
   * Returns `{ok, defaults: {default_provider, default_model}}`
   * (Tauri command) or the full redacted `AppConfig` (HTTP path).
   */
  setDefaultProviderModel: async (
    provider: string,
    model: string,
  ): Promise<{ ok: boolean; defaults: { default_provider: string; default_model: string } }> => {
    const body = {
      defaults: { default_provider: provider, default_model: model },
    };
    if (isTauri()) {
      const r = (await tauri<{ ok: boolean; defaults: { default_provider: string; default_model: string } }>(
        CMDS.setDefaultProviderModel,
        { provider, model },
      )) ?? { ok: false, defaults: { default_provider: provider, default_model: model } };
      return r;
    }
    await http<AppConfig>("/ui/settings", {
      method: "PATCH",
      body: JSON.stringify(body),
    });
    return { ok: true, defaults: { default_provider: provider, default_model: model } };
  },
  server: async (): Promise<unknown> => {
    if (isTauri()) return tauri(CMDS.serverInfo);
    return http("/ui/server");
  },
  restart: async (): Promise<{ ok: boolean }> => {
    if (isTauri()) return tauri(CMDS.restart);
    return http<{ ok: boolean }>("/ui/restart", { method: "POST" });
  },
  logs: async (
    since?: number,
    limit = 500,
    level?: string,
  ): Promise<LogsResponse> => {
    if (isTauri()) {
      return tauri(CMDS.logs, { since: since ?? null, limit, level: level ?? null });
    }
    const params = new URLSearchParams();
    if (since != null) params.set("since", String(since));
    params.set("limit", String(limit));
    if (level) params.set("level", level);
    return http<LogsResponse>(`/ui/logs?${params.toString()}`);
  },
  routing: async (): Promise<{ rules: any[]; default_tags: string[] }> => {
    if (isTauri()) return tauri(CMDS.routing);
    return http("/ui/routing");
  },
  patchRouting: async (patch: { rules: any[]; default_tags?: string[] }): Promise<{ rules: any[]; default_tags: string[] }> => {
    if (isTauri()) return tauri(CMDS.routingPatch, { patch });
    return http("/ui/routing", { method: "PATCH", body: JSON.stringify(patch) });
  },
  health: async (): Promise<{ providers: any[] }> => {
    if (isTauri()) return tauri(CMDS.health);
    return http("/ui/health");
  },
  events: async (provider?: string | null, limit = 100): Promise<{ events: any[] }> => {
    if (isTauri()) return tauri(CMDS.events, { provider: provider ?? null, limit });
    const params = new URLSearchParams();
    if (provider) params.set("provider", provider);
    params.set("limit", String(limit));
    return http(`/ui/events?${params.toString()}`);
  },
  analytics: async (): Promise<any> => {
    if (isTauri()) return tauri(CMDS.analytics);
    return http("/ui/analytics");
  },
  debug: async (): Promise<any> => {
    if (isTauri()) return tauri(CMDS.debug);
    return http("/ui/debug");
  },
  importConfig: async (text: string): Promise<any> => {
    if (isTauri()) return tauri(CMDS.importConfig, { text });
    return http("/ui/import", {
      method: "POST",
      body: text,
      headers: { "content-type": "application/toml" },
    });
  },
  exportConfig: async (): Promise<string> => {
    if (isTauri()) return tauri(CMDS.exportConfig);
    return httpText("/ui/export");
  },
  /**
   * Like `exportConfig` but also returns the response headers so
   * the UI can surface a notice when the server has redacted
   * credentials (e.g. `auth_token`) from the export payload.
   * Tauri invocations do not currently expose headers, so the
   * `redacted` flag is `false` in that path.
   */
  exportConfigRaw: async (): Promise<{ text: string; redacted: boolean }> => {
    if (isTauri()) {
      const text = (await tauri<string>(CMDS.exportConfig)) ?? "";
      return { text, redacted: false };
    }
    const r = await httpTextWithHeaders("/ui/export");
    return {
      text: r.text,
      redacted: r.headers.get("x-autorouter-auth-token-redacted") === "true",
    };
  },
  checkUpdate: async (): Promise<any> => {
    if (isTauri()) return tauri(CMDS.checkUpdate);
    return http("/ui/update");
  },
  toolProfiles: async (): Promise<{ profiles: any[] }> => {
    if (isTauri()) return tauri(CMDS.toolProfiles);
    return http("/ui/tool_profiles");
  },
  saveToolProfile: async (profile: any): Promise<{ profiles: any[] }> => {
    if (isTauri()) return tauri(CMDS.toolProfileSave, { profile });
    return http("/ui/tool_profiles", { method: "POST", body: JSON.stringify(profile) });
  },
  testTool: async (name: string, input: any): Promise<any> => {
    if (isTauri()) return tauri(CMDS.toolTest, { name, input });
    return http("/ui/tool_test", { method: "POST", body: JSON.stringify({ name, input }) });
  },
  /**
   * Probe a configured provider with a minimal chat-completion
   * request. Returns `{ok, status?, latency_ms, error?}`; the
   * dashboard "Test connection" button uses this to surface
   * wiring issues (wrong base URL, missing secret, 401, …)
   * before the operator sends a real request.
   */
  providerTest: async (id: string, model?: string): Promise<any> => {
    if (isTauri()) return tauri(CMDS.providerTest, { id, model: model ?? null });
    return http("/ui/provider_test", {
      method: "POST",
      body: JSON.stringify({ id, model: model ?? null }),
    });
  },
  /** Retrieve a stored secret value by its id. Resolves env:NAME references too. */
  secretGet: async (id: string): Promise<{ value: string }> => {
    if (isTauri()) return tauri(CMDS.secretGet, { id });
    return http<{ value: string }>(`/ui/secrets/${encodeURIComponent(id)}`);
  },
  /** Store a secret value directly in the secret store (keychain / file). */
  secretPut: async (id: string, value: string): Promise<{ ok: boolean }> => {
    if (isTauri()) return tauri(CMDS.secretPut, { id, value });
    return http<{ ok: boolean }>(`/ui/secrets/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify({ value }),
    });
  },
};
