import { useEffect, useState } from "react";
import { api } from "../api";
import { IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

interface DebugInfo {
  version: string;
  bind: string;
  started_at: string;
  uptime_seconds: number;
  pid: number;
  arch: string;
  os: string;
  config: any;
  env: { key: string; value: string }[];
  build: {
    rust_version: string;
    profile: string;
    target: string;
    features: string[];
  };
}

function redact(s: string): string {
  if (s.length <= 8) return "***";
  return s.slice(0, 4) + "…" + s.slice(-4);
}

export function Debug() {
  const [info, setInfo] = useState<DebugInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const reload = async () => {
    try {
      setInfo(await api.debug());
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => {
    reload();
  }, []);

  if (!info && !err) {
    return (
      <>
        <div className="page-header">
          <h1>Debug</h1>
          <div className="sub">Read-only snapshot of the running gateway for support tickets.</div>
        </div>
        <div className="empty">Loading…</div>
      </>
    );
  }

  return (
    <>
      <div className="page-header">
        <h1>Debug</h1>
        <div className="sub">Read-only snapshot for support tickets. Sensitive values are redacted.</div>
        <div className="spacer" />
        <button className="btn ghost" onClick={reload}>
          <IconRefresh /> Refresh
        </button>
      </div>
      {err && <div className="badge err">{err}</div>}
      {info && (
        <>
          <div className="section-card">
            <h2>Process</h2>
            <div className="kv">
              <div className="k">Version</div>
              <div className="v mono">{info.version}</div>
              <div className="k">PID</div>
              <div className="v mono">{info.pid}</div>
              <div className="k">Bind</div>
              <div className="v mono">{info.bind}</div>
              <div className="k">Uptime</div>
              <div className="v mono">{info.uptime_seconds}s</div>
              <div className="k">OS / arch</div>
              <div className="v mono">{info.os} / {info.arch}</div>
              <div className="k">Started</div>
              <div className="v mono">{info.started_at}</div>
            </div>
          </div>
          <div className="section-card">
            <h2>Build</h2>
            <div className="kv">
              <div className="k">Rust</div>
              <div className="v mono">{info.build.rust_version}</div>
              <div className="k">Profile</div>
              <div className="v mono">{info.build.profile}</div>
              <div className="k">Target</div>
              <div className="v mono">{info.build.target}</div>
              <div className="k">Features</div>
              <div className="v mono">{(info.build.features ?? []).join(", ") || "—"}</div>
            </div>
          </div>
          <div className="section-card">
            <h2>Config (redacted)</h2>
            <div className="row" style={{ marginBottom: 8 }}>
              <CopyButton
                text={JSON.stringify(info.config, null, 2)}
                variant="block"
                label="Copy JSON"
                successMsg="Config JSON copied"
              />
            </div>
            <pre
              style={{
                background: "var(--bg-elev-2)",
                padding: 12,
                borderRadius: 6,
                overflow: "auto",
                fontFamily: "var(--mono)",
                fontSize: 12,
                maxHeight: 320,
              }}
            >
{JSON.stringify(redactConfig(info.config), null, 2)}
            </pre>
          </div>
          <div className="section-card">
            <h2>Environment variables (filtered)</h2>
            <div className="sub">Only variables starting with <code>AUTOROUTER_</code> are surfaced.</div>
            <table className="table">
              <thead>
                <tr>
                  <th>Key</th>
                  <th>Value</th>
                </tr>
              </thead>
              <tbody>
                {(info.env ?? []).map((e) => (
                  <tr key={e.key}>
                    <td className="mono">{e.key}</td>
                    <td className="mono">{e.value}</td>
                  </tr>
                ))}
                {(info.env ?? []).length === 0 && (
                  <tr>
                    <td colSpan={2} className="empty">No AUTOROUTER_ env vars set.</td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </>
      )}
    </>
  );
}

function redactConfig(cfg: any): any {
  if (!cfg) return cfg;
  if (typeof cfg !== "object") return cfg;
  if (Array.isArray(cfg)) return cfg.map(redactConfig);
  const out: any = {};
  for (const [k, v] of Object.entries(cfg)) {
    if (
      /token|secret|key|password|api_key/i.test(k) &&
      typeof v === "string" &&
      v.length > 0
    ) {
      out[k] = redact(v);
    } else if (typeof v === "object" && v !== null) {
      out[k] = redactConfig(v);
    } else {
      out[k] = v;
    }
  }
  return out;
}
