import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { LogEntry } from "../types";
import {
  IconPlay,
  IconPause,
  IconTrash,
} from "../components/Icons";
import { CopyButton } from "../components/CopyButton";

const LEVELS = ["all", "debug", "info", "warn", "error"];

function levelClass(l: string) {
  switch (l) {
    case "error": return "err";
    case "warn":  return "warn";
    case "info":  return "info";
    case "debug": return "debug";
    default:      return "";
  }
}

export function Logs() {
  const [lines, setLines] = useState<LogEntry[]>([]);
  const [level, setLevel] = useState("all");
  const [since, setSince] = useState<number>(0);
  const [err, setErr] = useState<string | null>(null);
  const [paused, setPaused] = useState(false);
  const [filter, setFilter] = useState("");
  const ref = useRef<HTMLDivElement>(null);

  const fetchOnce = async () => {
    if (paused) return;
    try {
      const r = await api.logs(since || undefined, 1000, level === "all" ? undefined : level);
      if (r.lines.length === 0) return;
      setLines((prev) => {
        const n = [...prev, ...r.lines];
        if (n.length > 5000) n.splice(0, n.length - 5000);
        return n;
      });
      if (r.next_since) setSince(r.next_since);
    } catch (e) {
      setErr(String(e));
    }
  };

  useEffect(() => { setLines([]); setSince(0); }, [level]);
  useEffect(() => {
    const id = setInterval(fetchOnce, 1000);
    return () => clearInterval(id);
  }, [paused, level, since]);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Only auto-scroll if the user is already near the bottom (~50px);
    // otherwise they've scrolled up to read and we must not yank them back.
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 50;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [lines.length]);

  const counts = useMemo(() => {
    const c: Record<string, number> = { debug: 0, info: 0, warn: 0, error: 0 };
    for (const l of lines) {
      const k = l.level.toLowerCase();
      if (k in c) c[k]++;
    }
    return c;
  }, [lines]);

  const filtered = useMemo(() => {
    if (!filter) return lines;
    const q = filter.toLowerCase();
    return lines.filter(
      (l) =>
        l.message.toLowerCase().includes(q) ||
        l.target.toLowerCase().includes(q),
    );
  }, [lines, filter]);

  return (
    <>
      <div className="page-header">
        <h1>Logs</h1>
        <div className="sub">Live tail of in-process log lines.</div>
      </div>
      <div className="log-toolbar">
        <select
          className="select"
          style={{ width: 120 }}
          value={level}
          onChange={(e) => setLevel(e.target.value)}
          aria-label="Minimum log level"
        >
          {LEVELS.map((l) => (
            <option key={l} value={l}>{l}</option>
          ))}
        </select>
        <input
          className="input"
          style={{ maxWidth: 280 }}
          placeholder="Filter by message or target…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="Filter logs"
        />
        <div className="level-counts" aria-live="polite">
          <span className="pill" title="Debug">{counts.debug}</span>
          <span className="pill" title="Info">{counts.info}</span>
          <span className="pill" title="Warn">{counts.warn}</span>
          <span className="pill" title="Error">{counts.error}</span>
        </div>
        <div className="spacer" />
        <button
          className="btn"
          onClick={() => setPaused(!paused)}
          title={paused ? "Resume tail" : "Pause tail"}
        >
          {paused ? <><IconPlay /> Resume</> : <><IconPause /> Pause</>}
        </button>
        <button
          className="btn"
          onClick={() => setLines([])}
          title="Clear log buffer"
        >
          <IconTrash /> Clear
        </button>
      </div>
      {err ? (
        <div className="empty">
          <div className="empty-title">Could not load logs</div>
          <div className="empty-sub">{err}</div>
        </div>
      ) : (
        <div className="log" ref={ref}>
          {filtered.length === 0 ? (
            <div style={{ color: "var(--fg-faint)" }}>
              {lines.length === 0
                ? "No log lines yet. Trigger a request to see activity."
                : "No lines match the current filter."}
            </div>
          ) : (
            filtered.map((l, i) => (
              <div className="line" key={i}>
                <span className="ts">{l.ts.substring(11, 23)}</span>
                <span className={"lvl " + levelClass(l.level)}>{l.level}</span>
                <span className="target">{l.target}</span>
                <span>{l.message}</span>
                <CopyButton
                  text={`${l.ts} ${l.level} ${l.target} ${l.message}`}
                  size="sm"
                  title="Copy line"
                  successMsg="Line copied"
                  className="log-copy-mount"
                />
              </div>
            ))
          )}
        </div>
      )}
    </>
  );
}
