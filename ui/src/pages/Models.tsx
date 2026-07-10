import { useEffect, useState } from "react";
import { api } from "../api";
import type { ModelInfo } from "../types";
import { IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

interface Capability {
  id: string;
  provider: string;
  context_window: number;
  max_output_tokens: number;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_audio: boolean;
  supports_streaming: boolean;
}

function fmtNum(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "k";
  return String(n);
}

export function Models() {
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [provider, setProvider] = useState<string>("all");

  const reload = async () => {
    try {
      const r = await api.providers();
      setModels(r.models);
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => {
    reload();
  }, []);

  const all = models ?? [];
  const providers = Array.from(new Set(all.map((m) => m.provider))).sort();
  const filtered = all.filter((m) => {
    if (provider !== "all" && m.provider !== provider) return false;
    if (filter && !m.id.toLowerCase().includes(filter.toLowerCase())) return false;
    return true;
  });

  return (
    <>
      <div className="page-header">
        <h1>Models</h1>
        <div className="sub">Capability registry for every model the router knows about.</div>
        <div className="spacer" />
        <button className="btn ghost" onClick={reload}>
          <IconRefresh /> Refresh
        </button>
      </div>
      {err && <div className="badge err">{err}</div>}
      <div className="row" style={{ marginBottom: 16, gap: 12, flexWrap: "wrap" }}>
        <div className="field" style={{ flex: 1, minWidth: 220 }}>
          <input
            className="input"
            placeholder="Filter models by id…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>
        <div className="field">
          <select className="select" value={provider} onChange={(e) => setProvider(e.target.value)}>
            <option value="all">All providers</option>
            {providers.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
        </div>
      </div>
      <div className="section-card">
        <table className="table">
          <thead>
            <tr>
              <th>Model</th>
              <th>Provider</th>
              <th>Context</th>
              <th>Max out</th>
              <th>Tools</th>
              <th>Vision</th>
              <th>Audio</th>
              <th>Stream</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((m) => (
              <tr key={`${m.provider}:${m.id}`}>
                <td className="mono">
                  <span className="row" style={{ gap: 6, alignItems: "center" }}>
                    <span>{m.id}</span>
                    <CopyButton
                      text={m.id}
                      title="Copy model id"
                      successMsg="Model id copied"
                      size="sm"
                      variant="inline"
                    />
                  </span>
                </td>
                <td>{m.provider}</td>
                <td>{fmtNum(m.context_window)}</td>
                <td>{fmtNum(m.max_output_tokens)}</td>
                <td>{m.supports_tools ? "yes" : "no"}</td>
                <td>{m.supports_vision ? "yes" : "no"}</td>
                <td>{m.supports_audio ? "yes" : "no"}</td>
                <td>{m.supports_streaming ? "yes" : "no"}</td>
              </tr>
            ))}
            {filtered.length === 0 && (
              <tr>
                <td colSpan={8} className="empty">No models match.</td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
