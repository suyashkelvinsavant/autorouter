import { useEffect, useState } from "react";
import { api } from "../api";
import { IconRefresh, IconPlus, IconTrash, IconPlay } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

interface ToolProfile {
  name: string;
  description: string;
  schema: any;
}

const STARTER: ToolProfile = {
  name: "weather",
  description: "Look up the current weather for a city.",
  schema: {
    type: "object",
    properties: {
      city: { type: "string", description: "City name" },
      unit: { type: "string", enum: ["celsius", "fahrenheit"], default: "celsius" },
    },
    required: ["city"],
  },
};

export function ToolProfiles() {
  const [profiles, setProfiles] = useState<ToolProfile[] | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [input, setInput] = useState('{\n  "city": "Tokyo"\n}');
  const [result, setResult] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const reload = async () => {
    try {
      const r = await api.toolProfiles();
      setProfiles(r.profiles ?? []);
    } catch (e) {
      setErr(String(e));
    }
  };
  useEffect(() => {
    reload();
  }, []);

  const cur = selected != null && profiles ? profiles[selected] : null;

  const add = () => {
    if (!profiles) setProfiles([STARTER]);
    else setProfiles([...profiles, { ...STARTER, name: `tool_${profiles.length + 1}` }]);
    setSelected(profiles ? profiles.length : 0);
  };

  const remove = (i: number) => {
    if (!profiles) return;
    const next = profiles.slice();
    next.splice(i, 1);
    setProfiles(next);
    if (selected === i) setSelected(null);
  };

  const save = async () => {
    if (!profiles) return;
    setBusy(true);
    try {
      for (const p of profiles) {
        await api.saveToolProfile(p);
      }
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    if (!cur) return;
    setBusy(true);
    setResult(null);
    try {
      const parsed = JSON.parse(input);
      const r = await api.testTool(cur.name, parsed);
      setResult(JSON.stringify(r, null, 2));
    } catch (e) {
      setResult("Error: " + String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="page-header">
        <h1>Tool profiles</h1>
        <div className="sub">Define tools the model may invoke. JSON-Schema-aware test sandbox.</div>
        <div className="spacer" />
        <button className="btn ghost" onClick={reload} disabled={busy}>
          <IconRefresh /> Reload
        </button>
        <button className="btn" onClick={add} disabled={busy}>
          <IconPlus /> New profile
        </button>
        <button className="btn primary" onClick={save} disabled={busy}>
          Save
        </button>
      </div>
      {err && <div className="badge err">{err}</div>}
      <div className="row" style={{ gap: 16, alignItems: "stretch" }}>
        <div className="section-card" style={{ width: 260, flexShrink: 0 }}>
          <h2>Profiles</h2>
          <div className="list">
            {(profiles ?? []).map((p, i) => (
              <div
                key={i}
                className={"list-item" + (selected === i ? " active" : "")}
                onClick={() => setSelected(i)}
              >
                <div className="list-title mono">{p.name || "(unnamed)"}</div>
                <div className="list-sub">{p.description || "—"}</div>
                <button
                  className="btn ghost danger"
                  style={{ marginTop: 4 }}
                  onClick={(e) => { e.stopPropagation(); remove(i); }}
                  aria-label="Delete"
                ><IconTrash /></button>
              </div>
            ))}
            {(profiles ?? []).length === 0 && (
              <div className="empty">No profiles yet.</div>
            )}
          </div>
        </div>
        <div className="section-card" style={{ flex: 1 }}>
          {!cur ? (
            <div className="empty">Select a profile on the left, or create a new one.</div>
          ) : (
            <>
              <div className="grid cols-2">
                <div className="field">
                  <label>Name</label>
                  <input
                    className="input mono"
                    value={cur.name}
                    onChange={(e) => {
                      if (!profiles) return;
                      const next = profiles.slice();
                      next[selected!] = { ...cur, name: e.target.value };
                      setProfiles(next);
                    }}
                  />
                </div>
                <div className="field">
                  <label>Description</label>
                  <input
                    className="input"
                    value={cur.description}
                    onChange={(e) => {
                      if (!profiles) return;
                      const next = profiles.slice();
                      next[selected!] = { ...cur, description: e.target.value };
                      setProfiles(next);
                    }}
                  />
                </div>
              </div>
              <div className="field" style={{ marginTop: 12 }}>
                <label>Schema (JSON Schema)</label>
                <textarea
                  className="input mono"
                  style={{ minHeight: 180, fontFamily: "var(--mono)" }}
                  value={(() => { try { return JSON.stringify(cur.schema, null, 2); } catch { return "{}"; } })()}
                  onChange={(e) => {
                    if (!profiles) return;
                    try {
                      const next = profiles.slice();
                      next[selected!] = { ...cur, schema: JSON.parse(e.target.value) };
                      setProfiles(next);
                    } catch {
                      /* ignore while typing */
                    }
                  }}
                />
              </div>
              <div className="section-head" style={{ marginTop: 16 }}>
                <h2>Test sandbox</h2>
              </div>
              <div className="grid cols-2">
                <div className="field">
                  <label>Input (JSON)</label>
                  <textarea
                    className="input mono"
                    style={{ minHeight: 100, fontFamily: "var(--mono)" }}
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                  />
                </div>
                <div className="field">
                  <label>Result</label>
                  <div className="row" style={{ gap: 6, alignItems: "flex-start" }}>
                    <pre
                      style={{
                        background: "var(--bg-elev-2)",
                        padding: 12,
                        borderRadius: 6,
                        minHeight: 100,
                        fontFamily: "var(--mono)",
                        fontSize: 12,
                        whiteSpace: "pre-wrap",
                        flex: 1,
                        margin: 0,
                      }}
                    >
{result ?? "(no run yet)"}
                    </pre>
                    {result && (
                      <CopyButton
                        text={result}
                        title="Copy result"
                        successMsg="Result copied"
                        size="sm"
                        variant="inline"
                      />
                    )}
                  </div>
                </div>
              </div>
              <div className="actions" style={{ marginTop: 12 }}>
                <button className="btn primary" onClick={run} disabled={busy}>
                  <IconPlay /> Run
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </>
  );
}
