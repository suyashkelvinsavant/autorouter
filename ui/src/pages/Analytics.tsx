import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { IconRefresh } from "../components/Icons";

// The Analytics endpoint returns rolling counters plus latency
// statistics. The shape mirrors the JSON `/ui/analytics` returns.
// New fields are added defensively (defaulted to 0 / []) so an
// older gateway (pre-analytics-fix) does not break the page.
interface Analytics {
  total_requests: number;
  total_failures: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cache_read_tokens: number;
  total_cache_write_tokens: number;
  total_reasoning_tokens?: number;
  total_rate_limit_hits: number;
  avg_latency_ms?: number;
  p50_latency_ms?: number;
  p95_latency_ms?: number;
  latency_recorded?: boolean;
  by_provider: Bucket[];
  by_model: Bucket[];
  events_examined?: number;
}

interface Bucket {
  provider?: string;
  model?: string;
  requests: number;
  failures: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  reasoning_tokens?: number;
}

interface EventRow {
  provider: string;
  model: string;
  status: number;
  latency_ms: number;
  input_tokens?: number;
  output_tokens?: number;
  created_at: number;
}

function fmtNum(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "k";
  return String(n);
}

function fmtMs(n: number | undefined): string {
  if (!n || n <= 0) return "—";
  if (n >= 1000) return (n / 1000).toFixed(2) + "s";
  return n + "ms";
}

// Build 12 5-minute buckets for the last hour. The Sparkline path
// is purely client-side; no chart library required. Returns an
// array of { ts, count } where ts is the start of the bucket
// (ms since epoch) and count is the number of events in that
// bucket.
function bucketEventsByTime(events: EventRow[], now = Date.now()): { ts: number; count: number }[] {
  const BUCKET_MS = 5 * 60 * 1000;
  const HORIZON_MS = 60 * 60 * 1000;
  const horizonStart = now - HORIZON_MS;
  const buckets: { ts: number; count: number }[] = [];
  for (let i = 0; i < 12; i++) {
    buckets.push({ ts: horizonStart + i * BUCKET_MS, count: 0 });
  }
  for (const e of events) {
    const ts = (e.created_at ?? 0) * 1000;
    if (ts < horizonStart || ts > now) continue;
    const idx = Math.min(
      11,
      Math.max(0, Math.floor((ts - horizonStart) / BUCKET_MS)),
    );
    buckets[idx].count += 1;
  }
  return buckets;
}

function Sparkline(props: { data: { ts: number; count: number }[] }) {
  const { data } = props;
  const W = 320;
  const H = 60;
  const padX = 4;
  const padY = 6;
  const innerW = W - padX * 2;
  const innerH = H - padY * 2;
  const max = Math.max(1, ...data.map((d) => d.count));
  const stepX = data.length > 1 ? innerW / (data.length - 1) : 0;
  const points = data.map((d, i) => {
    const x = padX + i * stepX;
    const y = padY + innerH - (d.count / max) * innerH;
    return [x, y] as const;
  });
  const path = points
    .map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`)
    .join(" ");
  const total = data.reduce((acc, d) => acc + d.count, 0);
  if (total === 0) {
    return (
      <div className="sparkline-empty">
        <span>No requests in the last hour</span>
      </div>
    );
  }
  return (
    <svg
      className="sparkline"
      viewBox={`0 0 ${W} ${H}`}
      width="100%"
      height={H}
      role="img"
      aria-label={`Request volume last hour: ${total} requests`}
    >
      <path d={path} fill="none" stroke="currentColor" strokeWidth={1.5} />
      {points.map(([x, y], i) =>
        data[i].count > 0 ? (
          <circle key={i} cx={x} cy={y} r={1.8} fill="currentColor" />
        ) : null,
      )}
    </svg>
  );
}

function StatusBadge(props: { kind: "ok" | "err" | "warn"; children: React.ReactNode }) {
  return <span className={`badge ${props.kind}`}>{props.children}</span>;
}

export function Analytics() {
  const [data, setData] = useState<Analytics | null>(null);
  const [events, setEvents] = useState<EventRow[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // The user can force a hard reload by clicking the Refresh
  // button; the periodic 5s poll is incremental.
  const reload = async (hard = false) => {
    try {
      if (hard) setLoading(true);
      const [a, e] = await Promise.all([
        api.analytics(),
        api.events(undefined, 500).catch(() => ({ events: [] as EventRow[] })),
      ]);
      setData(a as Analytics);
      setEvents(((e as { events: EventRow[] }).events) ?? []);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => {
    reload(false);
    const id = setInterval(() => reload(false), 5000);
    return () => clearInterval(id);
  }, []);

  const sparkData = useMemo(() => bucketEventsByTime(events), [events]);
  const anyTokens =
    (data?.total_input_tokens ?? 0) + (data?.total_output_tokens ?? 0) > 0;
  const anyLatency = data?.latency_recorded ?? false;
  const anyRequests = (data?.total_requests ?? 0) > 0;

  // Empty state: no storage backend, or storage is enabled but
  // has no rows yet. We surface a clear "send a request to see
  // data" hint instead of the "0 requests" deceptive view the
  // old page rendered.
  if (!loading && !data && !err) {
    return (
      <>
        <div className="page-header">
          <h1>Analytics</h1>
          <div className="sub">Token usage, error rate, and per-provider rollups.</div>
        </div>
        <div className="empty-state">
          <h2>No storage backend attached</h2>
          <p>
            The gateway isn't writing request events yet. Once a request flows through the
            gateway the counters here will start populating.
          </p>
          <button className="btn" onClick={() => reload(true)}>
            <IconRefresh /> Retry
          </button>
        </div>
      </>
    );
  }

  if (loading && !data) {
    return (
      <>
        <div className="page-header">
          <h1>Analytics</h1>
          <div className="sub">Token usage, error rate, and per-provider rollups.</div>
        </div>
        <div className="empty">Loading…</div>
      </>
    );
  }

  return (
    <>
      <div className="page-header">
        <h1>Analytics</h1>
        <div className="sub">Token usage, error rate, and per-provider rollups since startup.</div>
        <div className="spacer" />
        <button
          className="btn ghost"
          onClick={() => reload(true)}
          disabled={loading}
          title="Reload"
        >
          <IconRefresh /> {loading ? "Reloading…" : "Refresh"}
        </button>
      </div>

      {err && (
        <div className="empty-state">
          <h2>Couldn't load analytics</h2>
          <p>{err}</p>
          <button className="btn" onClick={() => reload(true)}>
            <IconRefresh /> Retry
          </button>
        </div>
      )}

      {data && (
        <>
          <div className="section-head">
            <h2>Request volume — last hour</h2>
            <div className="sub">
              {sparkData.reduce((acc, d) => acc + d.count, 0)} requests across 5-minute buckets.
            </div>
          </div>
          <div className="section-card sparkline-card">
            <Sparkline data={sparkData} />
          </div>

          <div className="section-head">
            <h2>Latency</h2>
          </div>
          <div className="grid cols-3">
            <div className="card">
              <h3>p50 latency</h3>
              <div className="value">{fmtMs(data.p50_latency_ms)}</div>
              <div className="delta">median request</div>
            </div>
            <div className="card">
              <h3>p95 latency</h3>
              <div className="value">{fmtMs(data.p95_latency_ms)}</div>
              <div className="delta">95th percentile</div>
            </div>
            <div className="card">
              <h3>Average</h3>
              <div className="value">{fmtMs(data.avg_latency_ms)}</div>
              <div className="delta">mean across {fmtNum(data.total_requests)} requests</div>
            </div>
          </div>
          {!anyLatency && anyRequests && (
            <div className="empty-state inline">
              <p>
                Latency not recorded for any of the {fmtNum(data.total_requests)} events.
                This typically means the gateway is using a mock upstream that doesn't
                simulate timing.
              </p>
            </div>
          )}
          {!anyRequests && (
            <div className="empty-state inline">
              <p>
                No usage recorded yet. Send a request through the gateway (the Dashboard's
                "PONG" tile is a quick way) and these counters will start populating.
              </p>
            </div>
          )}

          <div className="section-head">
            <h2>Counters</h2>
          </div>
          <div className="grid cols-4">
            <div className="card">
              <h3>Requests</h3>
              <div className="value">{fmtNum(data.total_requests)}</div>
              <div className="delta">total since startup</div>
            </div>
            <div className="card">
              <h3>Failures</h3>
              <div className="value" style={{ color: data.total_failures > 0 ? "var(--danger)" : undefined }}>
                {fmtNum(data.total_failures)}
              </div>
              <div className="delta">
                {data.total_requests > 0
                  ? ((data.total_failures / data.total_requests) * 100).toFixed(1) + "% error rate"
                  : "0%"}
              </div>
            </div>
            <div className="card">
              <h3>Input tokens</h3>
              <div className="value">{fmtNum(data.total_input_tokens)}</div>
              <div className="delta">prompt tokens</div>
            </div>
            <div className="card">
              <h3>Output tokens</h3>
              <div className="value">{fmtNum(data.total_output_tokens)}</div>
              <div className="delta">completion tokens</div>
            </div>
            <div className="card">
              <h3>Cache read</h3>
              <div className="value">{fmtNum(data.total_cache_read_tokens)}</div>
              <div className="delta">tokens served from cache</div>
            </div>
            <div className="card">
              <h3>Cache write</h3>
              <div className="value">{fmtNum(data.total_cache_write_tokens)}</div>
              <div className="delta">tokens written to cache</div>
            </div>
            <div className="card">
              <h3>Rate-limit hits</h3>
              <div className="value" style={{ color: data.total_rate_limit_hits > 0 ? "var(--warning)" : undefined }}>
                {fmtNum(data.total_rate_limit_hits)}
              </div>
              <div className="delta">429 responses observed</div>
            </div>
            <div className="card">
              <h3>Total tokens</h3>
              <div className="value">
                {fmtNum(
                  data.total_input_tokens +
                    data.total_output_tokens +
                    data.total_cache_read_tokens +
                    data.total_cache_write_tokens,
                )}
              </div>
              <div className="delta">in + out + cache</div>
            </div>
          </div>

          {!anyTokens && anyRequests && (
            <div className="empty-state inline">
              <p>
                No token counts in the {fmtNum(data.total_requests)} events yet. Token data
                only accumulates when an upstream returns a real <code>usage</code> block —
                the bundled mock upstream now emits synthetic counts (input proportional to
                the prompt, output 50–200). Switch <code>X-AutoRouter-Target</code> to{" "}
                <code>mock</code> to exercise it, or wire a real provider to see real usage.
              </p>
            </div>
          )}

          <div className="section-head">
            <h2>By provider</h2>
          </div>
          <div className="section-card">
            <table className="table">
              <thead>
                <tr>
                  <th>Provider</th>
                  <th>Requests</th>
                  <th>Failures</th>
                  <th>Input</th>
                  <th>Output</th>
                </tr>
              </thead>
              <tbody>
                {(data.by_provider ?? []).map((p) => (
                  <tr key={p.provider}>
                    <td>{p.provider}</td>
                    <td>{p.requests}</td>
                    <td>{p.failures}</td>
                    <td className="mono">{fmtNum(p.input_tokens)}</td>
                    <td className="mono">{fmtNum(p.output_tokens)}</td>
                  </tr>
                ))}
                {(data.by_provider ?? []).length === 0 && (
                  <tr>
                    <td colSpan={5} className="empty">No data yet.</td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <div className="section-head">
            <h2>By model</h2>
          </div>
          <div className="section-card">
            <table className="table">
              <thead>
                <tr>
                  <th>Model</th>
                  <th>Requests</th>
                  <th>Failures</th>
                  <th>Input</th>
                  <th>Output</th>
                </tr>
              </thead>
              <tbody>
                {(data.by_model ?? []).map((m) => (
                  <tr key={m.model}>
                    <td className="mono">{m.model}</td>
                    <td>{m.requests}</td>
                    <td>{m.failures}</td>
                    <td className="mono">{fmtNum(m.input_tokens)}</td>
                    <td className="mono">{fmtNum(m.output_tokens)}</td>
                  </tr>
                ))}
                {(data.by_model ?? []).length === 0 && (
                  <tr>
                    <td colSpan={5} className="empty">No data yet.</td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <div className="section-head">
            <h2>Status</h2>
          </div>
          <div className="section-card">
            <div className="kv">
              <div className="kv-row">
                <div className="k">Storage</div>
                <div className="v">
                  <StatusBadge kind="ok">enabled</StatusBadge>
                </div>
              </div>
              <div className="kv-row">
                <div className="k">Events examined</div>
                <div className="v">{fmtNum(data.events_examined ?? 0)}</div>
              </div>
              <div className="kv-row">
                <div className="k">Token data</div>
                <div className="v">
                  {anyTokens ? (
                    <StatusBadge kind="ok">present</StatusBadge>
                  ) : (
                    <StatusBadge kind="warn">none recorded</StatusBadge>
                  )}
                </div>
              </div>
              <div className="kv-row">
                <div className="k">Latency data</div>
                <div className="v">
                  {anyLatency ? (
                    <StatusBadge kind="ok">recorded</StatusBadge>
                  ) : (
                    <StatusBadge kind="warn">not recorded</StatusBadge>
                  )}
                </div>
              </div>
            </div>
          </div>
        </>
      )}
    </>
  );
}