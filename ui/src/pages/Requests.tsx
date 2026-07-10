import React, { useEffect, useState } from "react";
import { api } from "../api";
import { IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

interface ProviderEvent {
  provider: string;
  model: string;
  kind: string;
  status: number;
  latency_ms: number;
  ts?: number;
  error?: string | null;
  request_id?: string;
  session_id?: string;
  source_provider?: string;
  created_at: string | number;
}

function timeAgo(s: string | number): string {
  try {
    let t: number;
    if (typeof s === "number") {
      // The server may emit either a Unix-seconds number or a
      // Unix-milliseconds number depending on the value's magnitude.
      // 10^10 ≈ 1970 + 228 years at ms resolution, ≈ 1970 + 285 days
      // at seconds; the latter is obviously wrong so we treat any
      // number below 10^12 as seconds and scale up.
      t = s < 1e12 ? s * 1000 : s;
    } else {
      t = new Date(s).getTime();
    }
    const diff = Date.now() - t;
    if (Number.isNaN(diff) || diff < 1000) return "now";
    if (diff < 60_000) return Math.floor(diff / 1000) + "s ago";
    if (diff < 3_600_000) return Math.floor(diff / 60_000) + "m ago";
    if (diff < 86_400_000) return Math.floor(diff / 3_600_000) + "h ago";
    return Math.floor(diff / 86_400_000) + "d ago";
  } catch {
    return String(s);
  }
}

export function Requests() {
  const [events, setEvents] = useState<ProviderEvent[] | null>(null);
  const [filter, setFilter] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);

  const reload = async () => {
    try {
      const r = await api.events(undefined, 200);
      setEvents(r.events ?? []);
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => {
    reload();
    const id = setInterval(reload, 3000);
    return () => clearInterval(id);
  }, []);

  const all = events ?? [];
  const filtered = all.filter((e) => {
    if (!filter) return true;
    const f = filter.toLowerCase();
    return (
      e.provider.toLowerCase().includes(f) ||
      e.model.toLowerCase().includes(f) ||
      (e.error ?? "").toLowerCase().includes(f) ||
      (e.request_id ?? "").toLowerCase().includes(f)
    );
  });

  return (
    <>
      <div className="page-header">
        <h1>Requests</h1>
        <div className="sub">Recent upstream calls with status, latency, and error info.</div>
        <div className="spacer" />
        <button className="btn ghost" onClick={reload}>
          <IconRefresh /> Refresh
        </button>
      </div>
      {err && <div className="badge err">{err}</div>}
      <div className="row" style={{ marginBottom: 12 }}>
        <input
          className="input"
          style={{ flex: 1, maxWidth: 360 }}
          placeholder="Filter by provider, model, error, or request id…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>
      <div className="section-card">
        <table className="table">
          <thead>
            <tr>
              <th>When</th>
              <th>Provider</th>
              <th>Model</th>
              <th>Status</th>
              <th>Latency</th>
              <th>Error</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((e, i) => {
              const rowKey = e.request_id ?? `${e.ts ?? ""}-${e.created_at ?? ""}-${e.latency_ms}-${e.model}`;
              const isOpen = expanded === rowKey;
              return (
                <React.Fragment key={rowKey}>
                  <tr
                    onClick={() => setExpanded(isOpen ? null : rowKey)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setExpanded(isOpen ? null : rowKey);
                      }
                    }}
                    role="button"
                    tabIndex={0}
                    aria-label="Toggle details"
                    style={{ cursor: "pointer" }}
                  >
                    <td className="mono">{timeAgo(e.created_at)}</td>
                    <td>{e.provider}</td>
                    <td className="mono">{e.model}</td>
                    <td>
                      <span className={"badge " + (e.status > 0 && e.status < 400 ? "ok" : "err")}>
                        {e.status}
                      </span>
                    </td>
                    <td className="mono">{e.latency_ms} ms</td>
                    <td>{e.error ?? ""}</td>
                    <td>{isOpen ? "▾" : "▸"}</td>
                  </tr>
                  {isOpen && (
                    <tr>
                      <td colSpan={7} style={{ background: "var(--bg-elev-2)", padding: 12 }}>
                        <div className="kv">
                          <div className="k">Request id</div>
                          <div className="v mono row" style={{ gap: 6, alignItems: "center" }}>
                            <span style={{ wordBreak: "break-all" }}>
                              {e.request_id ?? "—"}
                            </span>
                            {e.request_id && (
                              <CopyButton
                                text={e.request_id}
                                title="Copy request id"
                                successMsg="Request id copied"
                                size="sm"
                                variant="inline"
                              />
                            )}
                          </div>
                          <div className="k">Session id</div>
                          <div className="v mono row" style={{ gap: 6, alignItems: "center" }}>
                            <span style={{ wordBreak: "break-all" }}>
                              {e.session_id ?? "—"}
                            </span>
                            {e.session_id && (
                              <CopyButton
                                text={e.session_id}
                                title="Copy session id"
                                successMsg="Session id copied"
                                size="sm"
                                variant="inline"
                              />
                            )}
                          </div>
                          <div className="k">Source</div>
                          <div className="v">{e.source_provider ?? "—"}</div>
                          <div className="k">Kind</div>
                          <div className="v">{e.kind}</div>
                          <div className="k">Created at</div>
                          <div className="v mono">{e.created_at}</div>
                        </div>
                      </td>
                    </tr>
                  )}
                </React.Fragment>
              );
            })}
            {filtered.length === 0 && (
              <tr>
                <td colSpan={7} className="empty">
                  {all.length === 0 ? "No traffic yet." : "No events match the filter."}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
