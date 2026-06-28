import { useEffect, useState } from "react";
import { api } from "../api";
import { IconRefresh, IconDownload } from "../components/Icons";

interface UpdateInfo {
  current_version: string;
  latest_version: string | null;
  update_available: boolean;
  release_notes: string;
  release_url: string | null;
  published_at: string | null;
  can_self_update: boolean;
}

export function Update() {
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const reload = async () => {
    setBusy(true);
    setErr(null);
    try {
      setInfo(await api.checkUpdate());
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    reload();
  }, []);

  return (
    <>
      <div className="page-header">
        <h1>Update</h1>
        <div className="sub">Check for new versions, view release notes, and install updates.</div>
        <div className="spacer" />
        <button className="btn ghost" onClick={reload} disabled={busy}>
          <IconRefresh /> Check now
        </button>
      </div>
      {err && <div className="badge err">{err}</div>}
      {info && (
        <>
          <div className="section-card">
            <h2>Current version</h2>
            <div className="value mono" style={{ fontSize: 24 }}>{info.current_version}</div>
          </div>
          <div className="section-card">
            <h2>Latest release</h2>
            {info.update_available ? (
              <>
                <div className="badge ok lg" style={{ marginBottom: 12 }}>Update available</div>
                <div className="kv">
                  <div className="k">Latest</div>
                  <div className="v mono">{info.latest_version}</div>
                  <div className="k">Published</div>
                  <div className="v mono">{info.published_at ?? "—"}</div>
                  <div className="k">Release URL</div>
                  <div className="v">
                    {info.release_url ? (
                      <a href={info.release_url} target="_blank" rel="noreferrer">
                        {info.release_url}
                      </a>
                    ) : "—"}
                  </div>
                </div>
                <div className="actions" style={{ marginTop: 12 }}>
                  <a
                    className="btn primary"
                    href={info.release_url ?? "#"}
                    target="_blank"
                    rel="noreferrer"
                  >
                    <IconDownload /> Open release page
                  </a>
                </div>
              </>
            ) : (
              <div className="empty">You are running the latest version.</div>
            )}
          </div>
          <div className="section-card">
            <h2>Release notes</h2>
            <pre
              style={{
                background: "var(--bg-elev-2)",
                padding: 12,
                borderRadius: 6,
                fontFamily: "var(--mono)",
                fontSize: 12,
                whiteSpace: "pre-wrap",
                maxHeight: 400,
                overflow: "auto",
              }}
            >
{info.release_notes || "(no release notes available)"}
            </pre>
          </div>
          <div className="section-card">
            <h2>Self-update</h2>
            <div className="sub">
              In-app self-update uses the Tauri updater plugin and is enabled on release builds
              with a configured public key. The headless gateway checks on launch and prints
              a notice; install with the platform package manager.
            </div>
          </div>
        </>
      )}
    </>
  );
}
