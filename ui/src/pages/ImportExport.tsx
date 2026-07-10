import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { IconDownload, IconUpload, IconRefresh, IconCopy } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

export function ImportExport() {
  const [text, setText] = useState<string>("");
  const [err, setErr] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const reload = async () => {
    setBusy(true);
    setErr(null);
    setOk(null);
    try {
      const r = await api.exportConfigRaw();
      setText(r.text);
      setOk(
        r.redacted
          ? "Loaded from server. The auth token was redacted; re-set it after import."
          : "Loaded from server"
      );
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    reload();
  }, []);

  const download = () => {
    const blob = new Blob([text], { type: "application/toml" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "config.toml";
    a.click();
    URL.revokeObjectURL(url);
  };

  const upload = (e: React.ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    if (!f) return;
    const reader = new FileReader();
    reader.onload = () => setText(String(reader.result ?? ""));
    reader.readAsText(f);
  };

  const doImport = async () => {
    setBusy(true);
    setErr(null);
    setOk(null);
    try {
      await api.importConfig(text);
      setOk("Imported. Reloading…");
      setTimeout(() => location.reload(), 800);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="page-header">
        <h1>Import / Export</h1>
        <div className="sub">Back up or restore the full configuration. Secret values are not exported.</div>
        <div className="spacer" />
        {ok && <span className="badge ok">{ok}</span>}
        {err && <span className="badge err">{err}</span>}
        <button className="btn ghost" onClick={reload} disabled={busy}>
          <IconRefresh /> Reload
        </button>
        <button className="btn" onClick={download} disabled={busy || !text}>
          <IconDownload /> Download
        </button>
        <CopyButton
          text={text}
          title="Copy config to clipboard"
          successMsg="Config copied"
          size="md"
          variant="block"
          className="btn"
        >
          <IconCopy /> Copy
        </CopyButton>
        <button className="btn" onClick={() => fileInput.current?.click()} disabled={busy}>
          <IconUpload /> Upload file
        </button>
        <input
          ref={fileInput}
          type="file"
          accept=".toml,.txt,text/plain"
          style={{ display: "none" }}
          onChange={upload}
        />
        <button className="btn primary" onClick={doImport} disabled={busy || !text}>
          Import & apply
        </button>
      </div>
      <div className="section-card">
        <h2>config.toml</h2>
        <div className="sub">Edit and import, or upload a new file.</div>
        <textarea
          className="input mono"
          style={{ minHeight: 480, fontFamily: "var(--mono)", fontSize: 12 }}
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
        />
      </div>
    </>
  );
}
