import { useEffect, useState } from "react";
import { api } from "../api";
import { IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

interface HealthSnap {
  provider: string;
  samples: number;
  success_rate: number;
  avg_latency_ms: number;
  score: number;
}

export function Health() {
  const [providers, setProviders] = useState<HealthSnap[]>([]);
  const [bind, setBind] = useState<string>("127.0.0.1:4073");
  const [err, setErr] = useState<string | null>(null);

  const reload = async () => {
    try {
      const r = await api.health();
      setProviders(r.providers ?? []);
    } catch (e) {
      setErr(String(e));
    }
  };

  useEffect(() => {
    reload();
    const id = setInterval(reload, 5000);
    // Pull the live bind address from /ui/status so the health
    // endpoint URL always matches the actual server port (the user
    // can change it via Settings or AUTOROUTER_BIND).
    api.status()
      .then((s) => setBind(s.bind))
      .catch(() => { /* fall back to default */ });
    return () => clearInterval(id);
  }, []);

  const scoreColor = (s: number) =>
    s > 0.8 ? "var(--success)" : s > 0.5 ? "var(--warning)" : "var(--danger)";

  return (
    <>
      <div className="page-header">
        <h1>Health</h1>
        <div className="sub">Per-provider uptime, latency, and error rate from the rolling health tracker.</div>
        <div className="spacer" />
        <button className="btn ghost" onClick={reload} aria-label="Refresh health data">
          <IconRefresh /> Refresh
        </button>
      </div>
      {err && <div className="badge err">{err}</div>}
      <div className="grid cols-3">
        {providers.map((p) => (
          <div key={p.provider} className="card">
            <h3>{p.provider}</h3>
            <div className="value">
              <span style={{ color: scoreColor(p.score) }}>{(p.score * 100).toFixed(0)}%</span>
            </div>
            <div className="delta">score · {p.samples} samples</div>
            <div className="kv" style={{ marginTop: 12 }}>
              <div className="k">Success rate</div>
              <div className="v">{(p.success_rate * 100).toFixed(1)}%</div>
              <div className="k">Avg latency</div>
              <div className="v">{p.avg_latency_ms.toFixed(0)} ms</div>
            </div>
            <div style={{ marginTop: 12, height: 8, background: "var(--bg-elev-2)", borderRadius: 4, overflow: "hidden" }}>
              <div
                role="progressbar"
                aria-valuenow={Math.max(0, Math.min(100, p.score * 100))}
                aria-valuemin={0}
                aria-valuemax={100}
                style={{
                  width: `${Math.max(0, Math.min(100, p.score * 100))}%`,
                  height: "100%",
                  background: scoreColor(p.score),
                  transition: "width 0.3s",
                }}
              />
            </div>
          </div>
        ))}
        {providers.length === 0 && !err && (
          <div className="card" style={{ gridColumn: "1 / -1" }}>
            <div className="empty">No traffic yet. Health snapshots will appear as requests are served.</div>
          </div>
        )}
      </div>
      <div className="section-card" style={{ marginTop: 16 }}>
        <h2>How scores are computed</h2>
        <div className="sub">
          Score = success_rate × 0.7 + latency_factor × 0.3, where latency_factor decays
          from 1.0 at 100 ms to 0.0 at 5 s. Scores &lt; 0.3 cause the smart router to
          prefer another provider when one is configured.
        </div>
      </div>
      <div className="section-card" style={{ marginTop: 16 }}>
        <h2>Health endpoint</h2>
        <div className="sub">
          Hit this URL to scrape the live snapshot in JSON. Useful for uptime
          monitors, scripts, and health probes that want the same per-provider
          numbers shown above.
        </div>
        <div className="row" style={{ gap: 8, alignItems: "center", marginTop: 8 }}>
          <code
            className="mono"
            style={{
              padding: "8px 12px",
              background: "var(--bg-elev-2)",
              borderRadius: 6,
              flex: 1,
              wordBreak: "break-all",
            }}
          >
            {`http://${bind}/healthz`}
          </code>
          <CopyButton
            text={`http://${bind}/healthz`}
            title="Copy health endpoint URL"
            successMsg="Health endpoint copied"
            size="sm"
            variant="inline"
          />
        </div>
      </div>
    </>
  );
}
