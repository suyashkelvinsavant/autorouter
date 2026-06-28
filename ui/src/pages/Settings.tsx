import { useEffect, useState } from "react";
import { api } from "../api";
import type { AppConfig } from "../types";
import { reveal_data_dir, isTauri } from "../tauri";
import { CopyButton } from "../components/CopyButton";
import { useToast } from "../components/Toast";
import {
  IconRefresh,
  IconPower,
  IconFolder,
} from "../components/Icons";

export function Settings() {
  const { show: showToast } = useToast();
  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const reload = async () => {
    try {
      setCfg(await api.settings());
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => {
    reload();
  }, []);

  const save = async (patch: any) => {
    setBusy(true);
    setErr(null);
    setSaved(null);
    try {
      const next = await api.patchSettings(patch);
      setCfg(next);
      setSaved("Saved");
      setTimeout(() => setSaved(null), 1500);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const restart = async () => {
    setBusy(true);
    try {
      await api.restart();
      setSaved("Restart requested");
      setTimeout(() => setSaved(null), 1500);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!cfg)
    return (
      <div className="empty">
        <div className="empty-title">Loading settings…</div>
      </div>
    );

  return (
    <>
      <div className="page-header">
        <h1>Settings</h1>
        <div className="sub">Live configuration. Changes apply immediately.</div>
        <div className="spacer" />
        {saved && <span className="badge ok">{saved}</span>}
        {err && <span className="badge err">{err}</span>}
      </div>
      <div className="row" style={{ marginBottom: 16, flexWrap: "wrap" }}>
        <button className="btn" onClick={reload} disabled={busy} aria-label="Reload settings">
          <IconRefresh /> Reload
        </button>
        <button className="btn danger" onClick={restart} disabled={busy} aria-label="Restart server">
          <IconPower /> Restart server
        </button>
        <button className="btn" onClick={async () => { if (!isTauri()) return; if (!(await reveal_data_dir())) showToast("Could not open data directory", "err"); }} disabled={busy} aria-label="Reveal data directory">
          <IconFolder /> Reveal data dir
        </button>
      </div>

      <div className="section-card">
        <h2>Server</h2>
        <div className="sub">Bind address, CORS, and request limits.</div>
        <div className="grid cols-2">
          <div className="field">
            <label htmlFor="settings-bind">Bind address</label>
            <div className="row" style={{ gap: 6 }}>
              <input
                id="settings-bind"
                className="input mono"
                value={cfg.server.bind}
                onChange={(e) =>
                  setCfg({ ...cfg, server: { ...cfg.server, bind: e.target.value } })
                }
              />
              <CopyButton
                text={`http://${cfg.server.bind}`}
                title="Copy gateway URL"
                successMsg="Gateway URL copied"
                size="sm"
                variant="inline"
              />
            </div>
            <div className="hint">Loopback only by default for security.</div>
          </div>
          <div className="field">
            <label htmlFor="settings-auth-token">Auth token</label>
            <input
              id="settings-auth-token"
              className="input mono"
              type="password"
              value={cfg.server.auth_token ?? ""}
              placeholder={
                cfg.has_auth_token
                  ? "(set — type to replace)"
                  : "(not set)"
              }
              onChange={(e) =>
                setCfg({
                  ...cfg,
                  server: { ...cfg.server, auth_token: e.target.value || null },
                })
              }
            />
            <div className="hint">
              Used only when "Require auth token" is on. The current
              value is never returned by the API, so paste a new token
              to replace it.
            </div>
          </div>
          <div className="field">
            <label htmlFor="settings-max-body">Max body size (bytes)</label>
            <input
              id="settings-max-body"
              className="input mono"
              type="number"
              value={cfg.server.max_body_bytes}
              onChange={(e) =>
                setCfg({
                  ...cfg,
                  server: { ...cfg.server, max_body_bytes: Number(e.target.value) || 0 },
                })
              }
            />
          </div>
          <div className="field">
            <label htmlFor="settings-timeout">Request timeout (seconds)</label>
            <input
              id="settings-timeout"
              className="input mono"
              type="number"
              value={cfg.server.request_timeout_seconds}
              onChange={(e) =>
                setCfg({
                  ...cfg,
                  server: { ...cfg.server, request_timeout_seconds: Number(e.target.value) || 0 },
                })
              }
            />
          </div>
        </div>
        <div className="row" style={{ gap: 16, marginTop: 8, flexWrap: "wrap" }}>
          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              aria-label="Enable CORS"
              checked={cfg.server.enable_cors}
              onChange={(e) =>
                setCfg({ ...cfg, server: { ...cfg.server, enable_cors: e.target.checked } })
              }
            />
            Enable CORS
          </label>
          <label className="row" style={{ gap: 6 }}>
            <input
              type="checkbox"
              aria-label="Require auth token"
              checked={cfg.server.require_auth}
              onChange={(e) =>
                setCfg({ ...cfg, server: { ...cfg.server, require_auth: e.target.checked } })
              }
            />
            Require auth token
          </label>
        </div>
        <div className="actions">
          <button
            className="btn primary"
            onClick={() => save({ server: cfg.server })}
            disabled={busy}
            aria-label="Save server settings"
          >
            Save server
          </button>
        </div>
      </div>

      <div className="section-card">
        <h2>Defaults</h2>
        <div className="sub">What to use when a request does not specify a model or provider.</div>
        <div className="grid cols-2">
          <div className="field">
            <label htmlFor="settings-default-model">Default model</label>
            <input
              id="settings-default-model"
              className="input mono"
              value={cfg.defaults.default_model}
              onChange={(e) =>
                setCfg({ ...cfg, defaults: { ...cfg.defaults, default_model: e.target.value } })
              }
            />
          </div>
          <div className="field">
            <label htmlFor="settings-default-provider">Default provider</label>
            <input
              id="settings-default-provider"
              className="input mono"
              value={cfg.defaults.default_provider}
              onChange={(e) =>
                setCfg({ ...cfg, defaults: { ...cfg.defaults, default_provider: e.target.value } })
              }
            />
          </div>
          <div className="field">
            <label htmlFor="settings-max-tokens">Max total tokens</label>
            <input
              id="settings-max-tokens"
              className="input mono"
              type="number"
              value={cfg.defaults.max_total_tokens}
              onChange={(e) =>
                setCfg({
                  ...cfg,
                  defaults: { ...cfg.defaults, max_total_tokens: Number(e.target.value) || 0 },
                })
              }
            />
          </div>
          <div className="field" style={{ alignSelf: "end" }}>
            <label className="row" style={{ gap: 6 }}>
              <input
                type="checkbox"
                aria-label="Stream by default"
                checked={cfg.defaults.stream_by_default}
                onChange={(e) =>
                  setCfg({
                    ...cfg,
                    defaults: { ...cfg.defaults, stream_by_default: e.target.checked },
                  })
                }
              />
              Stream by default
            </label>
          </div>
        </div>
        <div className="actions">
          <button
            className="btn primary"
            onClick={() => save({ defaults: cfg.defaults })}
            disabled={busy}
            aria-label="Save default settings"
          >
            Save defaults
          </button>
        </div>
      </div>

      <div className="section-card">
        <h2>Logging</h2>
        <div className="sub">In-process log level and format.</div>
        <div className="grid cols-2">
          <div className="field">
            <label htmlFor="settings-log-level">Level</label>
            <select
              id="settings-log-level"
              className="select"
              value={cfg.logging.level}
              onChange={(e) =>
                setCfg({ ...cfg, logging: { ...cfg.logging, level: e.target.value } })
              }
            >
              {["trace", "debug", "info", "warn", "error"].map((l) => (
                <option key={l} value={l}>{l}</option>
              ))}
            </select>
          </div>
          <div className="field" style={{ alignSelf: "end" }}>
            <label className="row" style={{ gap: 6 }}>
              <input
                type="checkbox"
                aria-label="JSON format"
                checked={cfg.logging.json}
                onChange={(e) =>
                  setCfg({ ...cfg, logging: { ...cfg.logging, json: e.target.checked } })
                }
              />
              JSON format
            </label>
          </div>
        </div>
        <div className="actions">
          <button
            className="btn primary"
            onClick={() => save({ logging: cfg.logging })}
            disabled={busy}
            aria-label="Save logging settings"
          >
            Save logging
          </button>
        </div>
      </div>

      <div className="section-card">
        <h2>Storage</h2>
        <div className="sub">Where AutoRouter keeps its config, secrets, and database.</div>
        <div className="kv">
          <div className="k">Data dir</div>
          <div className="v mono row" style={{ gap: 6, alignItems: "center" }}>
            <span style={{ wordBreak: "break-all" }}>
              {cfg.storage.data_dir || "(default)"}
            </span>
            {cfg.storage.data_dir && (
              <CopyButton
                text={cfg.storage.data_dir}
                title="Copy data dir path"
                successMsg="Data dir path copied"
                size="sm"
                variant="inline"
              />
            )}
          </div>
          <div className="k">Database</div>
          <div className="v mono row" style={{ gap: 6, alignItems: "center" }}>
            <span style={{ wordBreak: "break-all" }}>
              {cfg.storage.database_file}
            </span>
            <CopyButton
              text={cfg.storage.database_file}
              title="Copy database path"
              successMsg="Database path copied"
              size="sm"
              variant="inline"
            />
          </div>
          <div className="k">Backup on shutdown</div>
          <div className="v">{cfg.storage.backup_on_shutdown ? "yes" : "no"}</div>
          <div className="k">Backups kept</div>
          <div className="v">{cfg.storage.backup_keep}</div>
        </div>
      </div>
    </>
  );
}
