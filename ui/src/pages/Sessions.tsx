import { useEffect, useState } from "react";
import { api } from "../api";
import type { SessionInfo } from "../types";
import { IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

function formatTime(s: string) {
  try {
    const d = new Date(s);
    return d.toLocaleString();
  } catch {
    return s;
  }
}

function timeAgo(s: string): string {
  try {
    const t = new Date(s).getTime();
    const diff = Date.now() - t;
    if (diff < 60_000) return `${Math.max(1, Math.floor(diff / 1000))}s ago`;
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return `${Math.floor(diff / 86_400_000)}d ago`;
  } catch {
    return s;
  }
}

function shortId(id: string) {
  return id.length > 12 ? id.slice(0, 8) + "…" + id.slice(-4) : id;
}

export function Sessions() {
  const [sessions, setSessions] = useState<SessionInfo[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const reload = async () => {
    try {
      const r = await api.sessions();
      setSessions(r.sessions);
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => {
    reload();
    const id = setInterval(reload, 3000);
    return () => clearInterval(id);
  }, []);

  return (
    <>
      <div className="page-header">
        <h1>Sessions</h1>
        <div className="sub">
          Active AI-tool connections. AutoRouter keeps a session per tool
          identified by the <code>X-AutoRouter-Session</code> header.
        </div>
        <div className="spacer" />
        <button className="btn" onClick={reload}>
          <IconRefresh /> Refresh
        </button>
      </div>

      {err ? (
        <div className="empty">
          <div className="empty-title">Could not load sessions</div>
          <div className="empty-sub">{err}</div>
        </div>
      ) : !sessions || sessions.length === 0 ? (
        <div className="empty">
          <div className="empty-title">No sessions yet</div>
          <div className="empty-sub">
            Connect an AI tool (Claude Code, Codex, Aider, Continue…)
            pointing at the local endpoint to see it appear here. The
            gateway watches the <code>X-AutoRouter-Session</code> header.
          </div>
        </div>
      ) : (
        <div className="card" style={{ padding: 0 }}>
          <table className="table">
            <thead>
              <tr>
                <th>ID</th>
                <th>Label</th>
                <th>Source</th>
                <th>Requests</th>
                <th>Last request</th>
                <th>Created</th>
              </tr>
            </thead>
            <tbody>
              {sessions.map((s) => (
                <tr key={s.id}>
                  <td className="mono" title={s.id}>
                    <span>{shortId(s.id)}</span>
                    <CopyButton
                      text={s.id}
                      size="sm"
                      title="Copy session id"
                      successMsg="Session id copied"
                      className="copy-inline-mount"
                    />
                  </td>
                  <td>{s.label || "—"}</td>
                  <td>
                    <span className="badge info">{s.source_provider}</span>
                  </td>
                  <td>{s.request_count}</td>
                  <td className="mono" title={s.last_request_id || ""}>
                    {s.last_request_id
                      ? timeAgo(s.last_request_at ?? s.created_at)
                      : "—"}
                  </td>
                  <td className="mono" title={s.created_at}>
                    {formatTime(s.created_at)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
