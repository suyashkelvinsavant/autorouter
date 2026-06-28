import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { useToast } from "../components/Toast";
import {
  IconAlertTriangle,
  IconArrowRight,
  IconBeaker,
  IconCheck,
  IconChevronDown,
  IconChevronRight,
  IconChevronUp,
  IconCode,
  IconCopy,
  IconGripVertical,
  IconInfo,
  IconLayers,
  IconPlus,
  IconRefresh,
  IconRoute,
  IconTrash,
  IconWand,
  IconZap,
} from "../components/Icons";

/* ─── Wire-format types mirroring the Rust schema ─────────────────
 *
 * The Rust types live in `crates/autorouter-router/src/model.rs`.
 * We mirror the JSON shape on the wire here. Anything that does not
 * fit the schema (e.g. legacy `target_provider` / `target_model`
 * flat fields) is normalised when reading and round-tripped in the
 * canonical `target.provider` / `target.model` shape on save.
 */
type ProviderKind = "openai" | "anthropic" | "gemini" | "custom" | "openrouter";

interface RouteTarget {
  provider: ProviderKind;
  model: string;
  headers?: Record<string, string>;
}

interface LegacyNeeds {
  needs_tools?: boolean;
  needs_vision?: boolean;
  needs_audio?: boolean;
  approx_input_tokens_gt?: number | null;
}

interface MultimodalNeeds {
  image?: boolean;
  audio?: boolean;
  pdf?: boolean;
}

interface CapabilityNeeds {
  vision?: boolean;
  audio?: boolean;
  tools?: boolean;
  min_context?: number;
}

/** Canonical RoutingRule shape used everywhere in this UI. */
interface RoutingRule {
  /** Stable id used for React keys + drag tracking. Not persisted. */
  id: string;
  name: string;
  priority: number;
  /** In-list enabled flag. Serialised as `enabled`. */
  enabled: boolean;
  match_tags_any: string[];
  match_tags_all: string[];
  match_model_contains: string[];
  needs: CapabilityNeeds;
  when?: LegacyNeeds | null;
  prefer_free: boolean;
  match_latency_below_ms: number | null;
  match_cost_below_per_million: number | null;
  match_quota_below_pct: number | null;
  match_benchmark_above: number | null;
  max_context_tokens: number | null;
  when_multimodal: MultimodalNeeds;
  targets: RouteTarget[];
  target: RouteTarget;
  reason: string;
}

/** Read a rule from any of the on-wire shapes (legacy or canonical). */
function readRule(raw: any): RoutingRule {
  const legacyProvider = (raw?.target_provider ?? raw?.targetProvider) as
    | ProviderKind
    | undefined;
  const legacyModel = (raw?.target_model ?? raw?.targetModel) as string | undefined;

  let target: RouteTarget;
  if (raw?.target && typeof raw.target === "object") {
    target = {
      provider: (raw.target.provider ?? "openai") as ProviderKind,
      model: String(raw.target.model ?? ""),
      headers: raw.target.headers ?? {},
    };
  } else if (legacyProvider !== undefined || legacyModel !== undefined) {
    target = {
      provider: (legacyProvider ?? "openai") as ProviderKind,
      model: String(legacyModel ?? ""),
      headers: {},
    };
  } else {
    target = { provider: "openai", model: "", headers: {} };
  }

  const targets: RouteTarget[] = Array.isArray(raw?.targets)
    ? raw.targets.map((t: any) => ({
        provider: (t?.provider ?? "openai") as ProviderKind,
        model: String(t?.model ?? ""),
        headers: t?.headers ?? {},
      }))
    : [];

  const legacyWhen: LegacyNeeds = raw?.when ?? {};
  const needsFromWhen: CapabilityNeeds = {
    vision: !!legacyWhen.needs_vision,
    audio: !!legacyWhen.needs_audio,
    tools: !!legacyWhen.needs_tools,
    min_context: legacyWhen.approx_input_tokens_gt ?? 0,
  };
  const hasExplicitNeeds =
    raw?.needs &&
    (raw.needs.vision ||
      raw.needs.audio ||
      raw.needs.tools ||
      (raw.needs.min_context ?? 0) > 0);
  const needs: CapabilityNeeds = hasExplicitNeeds
    ? {
        vision: !!raw.needs.vision,
        audio: !!raw.needs.audio,
        tools: !!raw.needs.tools,
        min_context: Number(raw.needs.min_context ?? 0),
      }
    : needsFromWhen;

  const whenMultimodal: MultimodalNeeds = raw?.when_multimodal ?? {};
  const when =
    raw?.when && Object.keys(raw.when).length > 0
      ? {
          needs_tools: !!legacyWhen.needs_tools,
          needs_vision: !!legacyWhen.needs_vision,
          needs_audio: !!legacyWhen.needs_audio,
          approx_input_tokens_gt: legacyWhen.approx_input_tokens_gt ?? null,
        }
      : null;

  return {
    id: typeof raw?.id === "string" ? raw.id : makeId(),
    name: String(raw?.name ?? ""),
    priority: Number(raw?.priority ?? 50),
    enabled: raw?.enabled === false ? false : true,
    match_tags_any: Array.isArray(raw?.match_tags_any)
      ? raw.match_tags_any.map(String)
      : [],
    match_tags_all: Array.isArray(raw?.match_tags_all)
      ? raw.match_tags_all.map(String)
      : [],
    match_model_contains: Array.isArray(raw?.match_model_contains)
      ? raw.match_model_contains.map(String)
      : [],
    needs,
    when,
    prefer_free: !!raw?.prefer_free,
    match_latency_below_ms: numOrNull(raw?.match_latency_below_ms),
    match_cost_below_per_million: numOrNull(raw?.match_cost_below_per_million),
    match_quota_below_pct: numOrNull(raw?.match_quota_below_pct),
    match_benchmark_above: numOrNull(raw?.match_benchmark_above),
    max_context_tokens: numOrNull(raw?.max_context_tokens),
    when_multimodal: {
      image: !!whenMultimodal.image,
      audio: !!whenMultimodal.audio,
      pdf: !!whenMultimodal.pdf,
    },
    targets,
    target,
    reason: String(raw?.reason ?? ""),
  };
}

function numOrNull(v: any): number | null {
  if (v === undefined || v === null || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

let _idSeq = 0;
function makeId(): string {
  _idSeq += 1;
  return `r${Date.now().toString(36)}_${_idSeq}`;
}

/** Serialise a rule to the canonical wire format. */
function writeRule(rule: RoutingRule): any {
  const out: any = {
    name: rule.name,
    priority: rule.priority,
    enabled: rule.enabled !== false,
    match_tags_any: rule.match_tags_any.filter((t) => t && t.trim()),
    match_tags_all: rule.match_tags_all.filter((t) => t && t.trim()),
    match_model_contains: rule.match_model_contains.filter((s) => s && s.trim()),
    needs: {
      vision: !!rule.needs.vision,
      audio: !!rule.needs.audio,
      tools: !!rule.needs.tools,
      min_context: Number(rule.needs.min_context ?? 0),
    },
    prefer_free: !!rule.prefer_free,
    match_latency_below_ms: rule.match_latency_below_ms ?? null,
    match_cost_below_per_million: rule.match_cost_below_per_million ?? null,
    match_quota_below_pct: rule.match_quota_below_pct ?? null,
    match_benchmark_above: rule.match_benchmark_above ?? null,
    max_context_tokens: rule.max_context_tokens ?? null,
    when_multimodal: {
      image: !!rule.when_multimodal.image,
      audio: !!rule.when_multimodal.audio,
      pdf: !!rule.when_multimodal.pdf,
    },
    targets: rule.targets.map((t) => ({
      provider: t.provider,
      model: t.model,
      headers: t.headers ?? {},
    })),
    target: {
      provider: rule.target.provider,
      model: rule.target.model,
      headers: rule.target.headers ?? {},
    },
    reason: rule.reason ?? "",
  };
  if (rule.when) {
    const w = rule.when;
    const touched =
      w.needs_tools ||
      w.needs_vision ||
      w.needs_audio ||
      (w.approx_input_tokens_gt != null && w.approx_input_tokens_gt > 0);
    if (touched) {
      out.when = {
        needs_tools: !!w.needs_tools,
        needs_vision: !!w.needs_vision,
        needs_audio: !!w.needs_audio,
        approx_input_tokens_gt: w.approx_input_tokens_gt ?? null,
      };
    }
  }
  for (const k of Object.keys(out)) {
    if (out[k] === null) delete out[k];
  }
  return out;
}

/** Build a starter rule with sensible defaults. */
function newRule(name = "new-rule"): RoutingRule {
  return {
    id: makeId(),
    name,
    priority: 50,
    enabled: true,
    match_tags_any: [],
    match_tags_all: [],
    match_model_contains: [],
    needs: { vision: false, audio: false, tools: false, min_context: 0 },
    when: null,
    prefer_free: false,
    match_latency_below_ms: null,
    match_cost_below_per_million: null,
    match_quota_below_pct: null,
    match_benchmark_above: null,
    max_context_tokens: null,
    when_multimodal: { image: false, audio: false, pdf: false },
    targets: [],
    target: { provider: "openai", model: "", headers: {} },
    reason: "",
  };
}

/* ─── Templates ────────────────────────────────────────────────── */

interface RuleTemplate {
  id: string;
  name: string;
  blurb: string;
  rule: RoutingRule;
}

const TEMPLATES: RuleTemplate[] = [
  {
    id: "vision",
    name: "Vision → Gemini",
    blurb: "Send image-bearing requests to a multimodal provider.",
    rule: {
      ...newRule("vision-route"),
      priority: 10,
      needs: { vision: true, audio: false, tools: false, min_context: 0 },
      target: { provider: "gemini", model: "gemini-2.5-pro", headers: {} },
      reason: "Vision inputs routed to Gemini Pro for best multimodal quality.",
    },
  },
  {
    id: "tools",
    name: "Tool-use → Haiku",
    blurb: "Cheap, fast inference for tool-using agents.",
    rule: {
      ...newRule("tools-to-haiku"),
      priority: 10,
      needs: { vision: false, audio: false, tools: true, min_context: 0 },
      target: { provider: "anthropic", model: "claude-haiku-4-5", headers: {} },
      reason: "Tool-calling workloads go to Claude Haiku for cost and latency.",
    },
  },
  {
    id: "long-context",
    name: "Long context → Gemini Pro",
    blurb: "Requests above a token threshold go to a 1M-context model.",
    rule: {
      ...newRule("long-context"),
      priority: 15,
      max_context_tokens: 100000,
      target: { provider: "gemini", model: "gemini-2.5-pro", headers: {} },
      reason: "1M+ token context windows route to Gemini Pro.",
    },
  },
  {
    id: "free-tier",
    name: "Free tier fallback",
    blurb: "Prefer free providers when the caller opts in.",
    rule: {
      ...newRule("free-tier"),
      priority: 5,
      prefer_free: true,
      targets: [{ provider: "gemini", model: "gemini-2.5-flash", headers: {} }],
      target: { provider: "openrouter", model: "nex-agi/nex-n2-pro:free", headers: {} },
      reason: "Free-tier preference with Gemini Flash as a high-quality fallback.",
    },
  },
  {
    id: "tagged",
    name: "Tag-routed to model",
    blurb: "When a caller sets X-AutoRouter-Tag=premium, route accordingly.",
    rule: {
      ...newRule("tag-premium"),
      priority: 10,
      match_tags_any: ["premium"],
      target: { provider: "openai", model: "gpt-5", headers: {} },
      reason: "Premium-tagged requests skip defaults and route to GPT-5.",
    },
  },
  {
    id: "fallback",
    name: "Default with fallback chain",
    blurb: "Primary target with two health-aware fallbacks.",
    rule: {
      ...newRule("default-with-fallbacks"),
      priority: 100,
      target: { provider: "openai", model: "gpt-5", headers: {} },
      targets: [
        { provider: "anthropic", model: "claude-sonnet-4-5", headers: {} },
        { provider: "gemini", model: "gemini-2.5-pro", headers: {} },
      ],
      reason: "Default route with provider fallbacks when health drops.",
    },
  },
];

/* ─── Provider badge helpers ───────────────────────────────────── */

interface ProviderInfo {
  id: string;
  kind: "openai" | "anthropic" | "gemini" | "custom";
  display_name: string;
  api_format: string;
  enabled: boolean;
}

function providerLabel(p: ProviderInfo[], provider: ProviderKind): string {
  const entry = p.find((x) => (x.kind === "custom" ? "custom" : x.id) === provider);
  return entry?.display_name ?? provider;
}

function providerDotClass(provider: ProviderKind): string {
  switch (provider) {
    case "openai":
      return "provider-dot provider-dot-openai";
    case "anthropic":
      return "provider-dot provider-dot-anthropic";
    case "gemini":
      return "provider-dot provider-dot-gemini";
    case "openrouter":
      return "provider-dot provider-dot-openrouter";
    default:
      return "provider-dot provider-dot-custom";
  }
}

/* ─── Plain-English match preview ──────────────────────────────── */

/** Build a one-line English summary of when this rule fires. */
function matchPreview(rule: RoutingRule): string {
  const parts: string[] = [];
  if (rule.match_tags_any.length > 0) {
    parts.push(
      `tag is one of ${rule.match_tags_any.map((t) => `'${t}'`).join(", ")}`,
    );
  }
  if (rule.match_tags_all.length > 0) {
    parts.push(
      `tags include ${rule.match_tags_all.map((t) => `'${t}'`).join(" and ")}`,
    );
  }
  if (rule.match_model_contains.length > 0) {
    parts.push(
      `model name contains ${rule.match_model_contains.map((s) => `'${s}'`).join(" or ")}`,
    );
  }
  if (rule.needs.vision) parts.push("request includes an image");
  if (rule.needs.audio) parts.push("request includes audio");
  if (rule.needs.tools) parts.push("request uses tools");
  if (rule.needs.min_context && rule.needs.min_context > 0) {
    parts.push(`input ≥ ${rule.needs.min_context.toLocaleString()} tokens`);
  }
  if (rule.max_context_tokens && rule.max_context_tokens > 0) {
    parts.push(`input > ${rule.max_context_tokens.toLocaleString()} tokens`);
  }
  if (rule.when_multimodal.image) parts.push("has image attachment");
  if (rule.when_multimodal.audio) parts.push("has audio attachment");
  if (rule.when_multimodal.pdf) parts.push("has PDF attachment");
  if (rule.prefer_free) parts.push("prefer free tier");
  if (rule.match_latency_below_ms) {
    parts.push(`latency below ${rule.match_latency_below_ms} ms`);
  }
  if (rule.match_cost_below_per_million) {
    parts.push(`cost below $${rule.match_cost_below_per_million}/M tok`);
  }
  if (rule.match_quota_below_pct) {
    parts.push(`quota below ${rule.match_quota_below_pct}%`);
  }
  if (rule.match_benchmark_above) {
    parts.push(`benchmark ≥ ${rule.match_benchmark_above}`);
  }
  if (parts.length === 0) return "Always matches (catch-all).";
  return "Match when " + parts.join(" AND ") + ".";
}

/* ─── Field components ─────────────────────────────────────────── */

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="field">
      {label && <label>{label}</label>}
      {children}
      {hint && <div className="hint">{hint}</div>}
    </div>
  );
}

function ChipEditor({
  label,
  hint,
  values,
  onChange,
  placeholder,
}: {
  label: string;
  hint?: string;
  values: string[];
  onChange: (next: string[]) => void;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState("");
  return (
    <Field label={label} hint={hint}>
      <div className="chip-editor">
        {values.map((v, i) => (
          <span key={`${v}-${i}`} className="chip">
            {v}
            <button
              className="chip-x"
              aria-label={`Remove ${v}`}
              onClick={() => onChange(values.filter((_, j) => j !== i))}
            >
              ×
            </button>
          </span>
        ))}
        <input
          className="chip-input"
          placeholder={placeholder ?? "add value, press Enter"}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === ",") {
              e.preventDefault();
              const t = draft.trim();
              if (t && !values.includes(t)) onChange([...values, t]);
              setDraft("");
            } else if (e.key === "Backspace" && !draft && values.length) {
              onChange(values.slice(0, -1));
            }
          }}
        />
      </div>
    </Field>
  );
}

function NumberField({
  label,
  hint,
  value,
  onChange,
  suffix,
  min,
  max,
  step,
  placeholder,
}: {
  label: string;
  hint?: React.ReactNode;
  value: number | null;
  onChange: (v: number | null) => void;
  suffix?: string;
  min?: number;
  max?: number;
  step?: number;
  placeholder?: string;
}) {
  return (
    <Field
      label={label}
      hint={
        suffix ? (
          <>
            {hint ? `${hint} ` : ""}
            <span style={{ color: "var(--fg-faint)" }}>({suffix})</span>
          </>
        ) : (
          hint
        )
      }
    >
      <input
        className="input mono"
        type="number"
        value={value === null || value === undefined ? "" : value}
        placeholder={placeholder}
        min={min}
        max={max}
        step={step}
        onChange={(e) => {
          const v = e.target.value;
          if (v === "") onChange(null);
          else {
            const n = Number(v);
            onChange(Number.isFinite(n) ? n : null);
          }
        }}
      />
    </Field>
  );
}

function ProviderSelect({
  value,
  onChange,
  providers,
}: {
  value: ProviderKind;
  onChange: (v: ProviderKind) => void;
  providers: ProviderInfo[];
}) {
  const builtInOptions: ProviderKind[] = ["openai", "anthropic", "gemini"];
  const have = new Set<string>(providers.map((p) => (p.kind === "custom" ? "custom" : p.id)));
  const missingBuiltIn = builtInOptions.filter((k) => !have.has(k));
  const fallbackOptions: ProviderInfo[] = missingBuiltIn.map((k) => ({
    id: k,
    kind: k as "openai" | "anthropic" | "gemini",
    display_name:
      k === "openai"
        ? "OpenAI (not configured)"
        : k === "anthropic"
          ? "Anthropic (not configured)"
          : "Gemini (not configured)",
    api_format: k,
    enabled: false,
  }));
  const allProviders = [...providers, ...fallbackOptions];
  return (
    <select
      className="select"
      value={value}
      onChange={(e) => onChange(e.target.value as ProviderKind)}
    >
      {allProviders.map((p) => (
        <option key={p.id} value={p.kind === "custom" ? "custom" : p.id}>
          {p.display_name}
        </option>
      ))}
    </select>
  );
}

function Toggle({
  label,
  value,
  onChange,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="row" style={{ gap: 8, cursor: "pointer" }}>
      <input
        type="checkbox"
        checked={value}
        onChange={(e) => onChange(e.target.checked)}
        aria-label={label}
      />
      <span style={{ fontSize: 13 }}>{label}</span>
    </label>
  );
}

/* ─── Live test-runner matcher ─────────────────────────────────── */

interface SimCtx {
  model: string;
  tags: string[];
  needsVision: boolean;
  needsAudio: boolean;
  needsTools: boolean;
  approxTokens: number;
  hasImage: boolean;
  hasAudio: boolean;
  hasPdf: boolean;
}

function ruleMatches(rule: RoutingRule, ctx: SimCtx): boolean {
  if (!rule.enabled) return false;
  if (rule.match_tags_any.length > 0) {
    if (!rule.match_tags_any.some((t) => ctx.tags.includes(t))) return false;
  }
  if (rule.match_tags_all.length > 0) {
    if (!rule.match_tags_all.every((t) => ctx.tags.includes(t))) return false;
  }
  if (rule.match_model_contains.length > 0) {
    const m = ctx.model.toLowerCase();
    if (!rule.match_model_contains.some((s) => m.includes(s.toLowerCase()))) return false;
  }
  if (rule.needs.vision && !ctx.needsVision) return false;
  if (rule.needs.audio && !ctx.needsAudio) return false;
  if (rule.needs.tools && !ctx.needsTools) return false;
  if (rule.needs.min_context && rule.needs.min_context > 0) {
    if (ctx.approxTokens < rule.needs.min_context) return false;
  }
  if (rule.max_context_tokens && rule.max_context_tokens > 0) {
    if (ctx.approxTokens <= rule.max_context_tokens) return false;
  }
  if (rule.when_multimodal.image && !ctx.hasImage) return false;
  if (rule.when_multimodal.audio && !ctx.hasAudio) return false;
  if (rule.when_multimodal.pdf && !ctx.hasPdf) return false;
  return true;
}

/* ─── Main page ────────────────────────────────────────────────── */

type LoadState =
  | { kind: "loading" }
  | { kind: "ready" }
  | { kind: "error"; message: string };

export function Routing() {
  const toast = useToast();
  const [rules, setRules] = useState<RoutingRule[]>([]);
  const [defaultTags, setDefaultTags] = useState<string[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [models, setModels] = useState<any[]>([]);
  const [load, setLoad] = useState<LoadState>({ kind: "loading" });
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [dirty, setDirty] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showTemplates, setShowTemplates] = useState(false);
  const [showTestRunner, setShowTestRunner] = useState(false);
  const [showJson, setShowJson] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState<Record<string, boolean>>({});

  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);
  const [dropPosition, setDropPosition] = useState<"before" | "after">("before");

  const [simModel, setSimModel] = useState("gpt-5");
  const [simTags, setSimTags] = useState<string[]>([]);
  const [simVision, setSimVision] = useState(false);
  const [simTools, setSimTools] = useState(false);
  const [simAudio, setSimAudio] = useState(false);
  const [simHasImage, setSimHasImage] = useState(false);
  const [simHasAudio, setSimHasAudio] = useState(false);
  const [simHasPdf, setSimHasPdf] = useState(false);
  const [simTokens, setSimTokens] = useState<number>(0);

  async function reload() {
    setLoad({ kind: "loading" });
    try {
      const [cfg, prov] = await Promise.all([api.routing(), api.providers()]);
      setRules((cfg.rules ?? []).map(readRule));
      setDefaultTags(cfg.default_tags ?? []);
      setProviders(prov.providers as any);
      setModels(prov.models ?? []);
      setLoad({ kind: "ready" });
      setDirty(false);
      setSavedAt(Date.now());
    } catch (e) {
      setLoad({ kind: "error", message: String(e) });
    }
  }

  useEffect(() => {
    reload();
  }, []);

  /* ─── rule mutation helpers ───────────────────────────────── */

  function patchRule(id: string, patch: Partial<RoutingRule>) {
    setRules((prev) => prev.map((r) => (r.id === id ? { ...r, ...patch } : r)));
    setDirty(true);
  }

  function patchTarget(id: string, patch: Partial<RouteTarget>) {
    setRules((prev) =>
      prev.map((r) => (r.id === id ? { ...r, target: { ...r.target, ...patch } } : r)),
    );
    setDirty(true);
  }

  function patchNeeds(id: string, patch: Partial<CapabilityNeeds>) {
    setRules((prev) =>
      prev.map((r) =>
        r.id === id ? { ...r, needs: { ...r.needs, ...patch } } : r,
      ),
    );
    setDirty(true);
  }

  function patchMultimodal(id: string, patch: Partial<MultimodalNeeds>) {
    setRules((prev) =>
      prev.map((r) =>
        r.id === id
          ? { ...r, when_multimodal: { ...r.when_multimodal, ...patch } }
          : r,
      ),
    );
    setDirty(true);
  }

  function addRule(template?: RuleTemplate) {
    const base = template ? { ...template.rule, id: makeId() } : newRule(`rule-${rules.length + 1}`);
    let pri = base.priority;
    const used = new Set(rules.map((r) => r.priority));
    while (used.has(pri)) pri += 1;
    const rule = { ...base, priority: pri };
    setRules((prev) => [...prev, rule]);
    setSelectedId(rule.id);
    setDirty(true);
    toast.show(
      template ? `Template "${template.name}" added` : "New rule added",
      "ok",
    );
  }

  function deleteRule(id: string) {
    setRules((prev) => prev.filter((r) => r.id !== id));
    if (selectedId === id) setSelectedId(null);
    setDirty(true);
  }

  function duplicateRule(id: string) {
    const source = rules.find((r) => r.id === id);
    if (!source) return;
    const copy: RoutingRule = {
      ...source,
      id: makeId(),
      name: `${source.name || "rule"}-copy`,
      enabled: source.enabled,
    };
    setRules((prev) => [...prev, copy]);
    setSelectedId(copy.id);
    setDirty(true);
  }

  /** Move a rule by a delta (-1 = up = higher priority). */
  function moveRule(id: string, dir: -1 | 1) {
    setRules((prev) => {
      const sorted = [...prev].sort((a, b) => a.priority - b.priority);
      const idx = sorted.findIndex((r) => r.id === id);
      const j = idx + dir;
      if (idx < 0 || j < 0 || j >= sorted.length) return prev;
      [sorted[idx], sorted[j]] = [sorted[j], sorted[idx]];
      // Re-assign priority from sorted order so the new order is sticky.
      sorted.forEach((r, i) => (r.priority = (i + 1) * 10));
      return sorted;
    });
    setDirty(true);
  }

  function addFallback(id: string) {
    setRules((prev) =>
      prev.map((r) =>
        r.id === id
          ? {
              ...r,
              targets: [...r.targets, { provider: "openai", model: "", headers: {} }],
            }
          : r,
      ),
    );
    setDirty(true);
  }

  function patchFallback(id: string, fbIdx: number, patch: Partial<RouteTarget>) {
    setRules((prev) =>
      prev.map((r) => {
        if (r.id !== id) return r;
        const targets = r.targets.slice();
        targets[fbIdx] = { ...targets[fbIdx], ...patch };
        return { ...r, targets };
      }),
    );
    setDirty(true);
  }

  function removeFallback(id: string, fbIdx: number) {
    setRules((prev) =>
      prev.map((r) => {
        if (r.id !== id) return r;
        return { ...r, targets: r.targets.filter((_, i) => i !== fbIdx) };
      }),
    );
    setDirty(true);
  }

  /* ─── DnD handlers ────────────────────────────────────────── */

  function handleDragStart(e: React.DragEvent<HTMLDivElement>, id: string) {
    setDraggingId(id);
    e.dataTransfer.effectAllowed = "move";
    // dataTransfer must have some data for Firefox to fire drag events.
    e.dataTransfer.setData("text/plain", id);
  }

  function handleDragOver(
    e: React.DragEvent<HTMLDivElement>,
    id: string,
  ) {
    if (!draggingId || draggingId === id) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    const rect = e.currentTarget.getBoundingClientRect();
    const midpoint = rect.top + rect.height / 2;
    setDropTargetId(id);
    setDropPosition(e.clientY < midpoint ? "before" : "after");
  }

  function handleDragEnd() {
    const from = draggingId;
    const to = dropTargetId;
    const pos = dropPosition;
    setDraggingId(null);
    setDropTargetId(null);
    if (!from || !to || from === to) return;
    setRules((prev) => {
      const sorted = [...prev].sort((a, b) => a.priority - b.priority);
      const fromIdx = sorted.findIndex((r) => r.id === from);
      let toIdx = sorted.findIndex((r) => r.id === to);
      if (fromIdx < 0 || toIdx < 0) return prev;
      const [moved] = sorted.splice(fromIdx, 1);
      // Recompute insertion index after removing.
      toIdx = sorted.findIndex((r) => r.id === to);
      const insertAt = pos === "before" ? toIdx : toIdx + 1;
      sorted.splice(insertAt, 0, moved);
      sorted.forEach((r, i) => (r.priority = (i + 1) * 10));
      return sorted;
    });
    setDirty(true);
  }

  function handleKeyReorder(e: React.KeyboardEvent, id: string) {
    if (e.key === "ArrowUp") {
      e.preventDefault();
      moveRule(id, -1);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      moveRule(id, 1);
    }
  }

  /* ─── save ────────────────────────────────────────────────── */

  async function save() {
    try {
      const payload = {
        rules: rules.map(writeRule),
        default_tags: defaultTags,
      };
      const next = await api.patchRouting(payload);
      setRules((next.rules ?? []).map(readRule));
      setDefaultTags(next.default_tags ?? []);
      setDirty(false);
      setSavedAt(Date.now());
      toast.show("Routing saved", "ok");
    } catch (e) {
      toast.show(`Save failed: ${String(e)}`, "err");
    }
  }

  /* ─── derived view ────────────────────────────────────────── */

  const sortedRules = useMemo(
    () => [...rules].sort((a, b) => a.priority - b.priority),
    [rules],
  );
  const selectedRule =
    selectedId != null ? rules.find((r) => r.id === selectedId) ?? null : null;

  const simCtx: SimCtx = {
    model: simModel,
    tags: [...simTags, ...defaultTags],
    needsVision: simVision,
    needsAudio: simAudio,
    needsTools: simTools,
    approxTokens: simTokens,
    hasImage: simHasImage,
    hasAudio: simHasAudio,
    hasPdf: simHasPdf,
  };
  const evaluated = useMemo(
    () =>
      sortedRules.map((rule, i) => ({
        rule,
        idx: i,
        hit: ruleMatches(rule, simCtx),
      })),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      sortedRules,
      simModel,
      simTags,
      simVision,
      simTools,
      simAudio,
      simHasImage,
      simHasAudio,
      simHasPdf,
      simTokens,
      defaultTags,
    ],
  );
  const firstHit = evaluated.find((e) => e.hit);
  const decision = firstHit ? firstHit.rule.target : null;

  /* ─── render ──────────────────────────────────────────────── */

  return (
    <>
      <div className="page-header">
        <h1 style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <IconRoute /> Routing
        </h1>
        <div className="sub">
          Rules run in priority order — lower number fires first. The first matching rule
          wins and its target receives the request. Override anything from a client with
          the <code>X-AutoRouter-Target</code> header.
        </div>
        <div className="spacer" />
        {dirty && (
          <span className="badge warn" title="You have unsaved changes">
            Unsaved changes
          </span>
        )}
        {!dirty && savedAt && (
          <span className="badge ok">Saved {new Date(savedAt).toLocaleTimeString()}</span>
        )}
      </div>

      <div className="row" style={{ marginBottom: 16, flexWrap: "wrap", gap: 8 }}>
        <button
          className={"btn" + (showTestRunner ? " primary" : "")}
          onClick={() => setShowTestRunner((s) => !s)}
          title="Open the live test runner"
        >
          <IconBeaker /> {showTestRunner ? "Hide" : "Open"} test runner
        </button>
        <button
          className="btn"
          onClick={() => setShowTemplates((s) => !s)}
          title="Insert a rule template"
        >
          <IconWand /> {showTemplates ? "Hide" : "Templates"}
        </button>
        <button
          className="btn"
          onClick={() => setShowJson((s) => !s)}
          title="Show the JSON the server will receive"
        >
          <IconCode /> {showJson ? "Hide" : "View"} JSON
        </button>
        <div className="spacer" />
        <button className="btn" onClick={reload} disabled={dirty}>
          <IconRefresh /> Reload
        </button>
        <button className="btn primary" onClick={save} disabled={!dirty}>
          <IconCheck /> Save routing
        </button>
      </div>

      {load.kind === "error" && (
        <div className="notice" style={{ borderLeftColor: "var(--danger)" }}>
          <span className="icon" style={{ color: "var(--danger)" }}>
            <IconAlertTriangle />
          </span>
          <div className="body">
            <div className="title">Could not load routing config</div>
            <div className="sub">{load.message}</div>
          </div>
          <button className="btn" onClick={reload}>
            <IconRefresh /> Retry
          </button>
        </div>
      )}

      {load.kind === "loading" && (
        <div className="routing-layout">
          <div className="rule-list-pane">
            <div className="rule-card rule-card-skeleton" />
            <div className="rule-card rule-card-skeleton" />
            <div className="rule-card rule-card-skeleton" />
          </div>
          <div className="rule-editor-pane">
            <div className="empty">
              <div className="empty-icon">
                <IconRefresh />
              </div>
              <div className="empty-title">Loading rules…</div>
              <div className="empty-sub">
                Fetching the current routing config from the gateway.
              </div>
            </div>
          </div>
        </div>
      )}

      {load.kind === "ready" && (
        <>
          {showTemplates && (
            <div className="section-card">
              <div className="row between" style={{ marginBottom: 10 }}>
                <h2 style={{ margin: 0 }}>Templates</h2>
                <span className="sub">Insert one of these as a new rule.</span>
              </div>
              <div className="grid cols-3">
                {TEMPLATES.map((t) => (
                  <div
                    key={t.id}
                    className="card clickable"
                    onClick={() => addRule(t)}
                  >
                    <div className="row between">
                      <h3 style={{ color: "var(--fg)" }}>{t.name}</h3>
                      <IconPlus />
                    </div>
                    <div className="sub" style={{ marginTop: 4 }}>
                      {t.blurb}
                    </div>
                    <div className="row" style={{ gap: 6, marginTop: 8, flexWrap: "wrap" }}>
                      <span className="badge">priority {t.rule.priority}</span>
                      <span className={providerDotClass(t.rule.target.provider)} />
                      <span className="badge info">
                        {providerLabel(providers, t.rule.target.provider)}
                      </span>
                      {t.rule.targets.length > 0 && (
                        <span className="badge">
                          <IconLayers /> {t.rule.targets.length} fallback
                        </span>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {showJson && (
            <div className="section-card">
              <div className="row between" style={{ marginBottom: 10 }}>
                <h2 style={{ margin: 0 }}>Outgoing JSON</h2>
                <span className="sub">
                  What the gateway will persist to <code>config.toml</code>.
                </span>
              </div>
              <pre className="code-block mono">
                {JSON.stringify(
                  { rules: rules.map(writeRule), default_tags: defaultTags },
                  null,
                  2,
                )}
              </pre>
            </div>
          )}

          {showTestRunner && (
            <TestRunner
              simModel={simModel}
              setSimModel={setSimModel}
              simTags={simTags}
              setSimTags={setSimTags}
              simVision={simVision}
              setSimVision={setSimVision}
              simTools={simTools}
              setSimTools={setSimTools}
              simAudio={simAudio}
              setSimAudio={setSimAudio}
              simHasImage={simHasImage}
              setSimHasImage={setSimHasImage}
              simHasAudio={simHasAudio}
              setSimHasAudio={setSimHasAudio}
              simHasPdf={simHasPdf}
              setSimHasPdf={setSimHasPdf}
              simTokens={simTokens}
              setSimTokens={setSimTokens}
              evaluated={evaluated}
              decision={decision}
              providerLabelFn={(k) => providerLabel(providers, k)}
              onSelect={(id) => setSelectedId(id)}
            />
          )}

          <div className="routing-layout">
            {/* Left: rule list with DnD */}
            <div className="rule-list-pane">
              <div className="section-card" style={{ marginBottom: 12 }}>
                <h2
                  style={{
                    margin: 0,
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                  }}
                >
                  <IconRoute /> Rules
                  <span className="badge" style={{ marginLeft: 4 }}>
                    {rules.length}
                  </span>
                </h2>
                <div className="sub" style={{ marginTop: 4 }}>
                  First match wins. Drag to reorder, or focus a row and press Up/Down.
                </div>
                <div className="row" style={{ marginTop: 10, gap: 6 }}>
                  <button
                    className="btn primary"
                    onClick={() => addRule()}
                    style={{ flex: 1 }}
                  >
                    <IconPlus /> Add rule
                  </button>
                </div>
              </div>

              {sortedRules.length === 0 && (
                <div className="empty">
                  <div className="empty-icon">
                    <IconRoute />
                  </div>
                  <div className="empty-title">No routing rules yet</div>
                  <div className="empty-sub">
                    Your requests will use the default model. Click <strong>+ Add rule</strong>{" "}
                    to get started, or pick a template above.
                  </div>
                </div>
              )}

              {sortedRules.map((rule, idx) => (
                <RuleListItem
                  key={rule.id}
                  rule={rule}
                  idx={idx}
                  isSelected={selectedId === rule.id}
                  isDragging={draggingId === rule.id}
                  isDropBefore={
                    dropTargetId === rule.id &&
                    dropPosition === "before" &&
                    draggingId !== rule.id
                  }
                  isDropAfter={
                    dropTargetId === rule.id &&
                    dropPosition === "after" &&
                    draggingId !== rule.id
                  }
                  providerLabel={(k) => providerLabel(providers, k)}
                  onSelect={() => setSelectedId(rule.id)}
                  onDragStart={(e) => handleDragStart(e, rule.id)}
                  onDragOver={(e) => handleDragOver(e, rule.id)}
                  onDragEnd={handleDragEnd}
                  onKeyReorder={(e) => handleKeyReorder(e, rule.id)}
                  onToggleEnabled={() =>
                    patchRule(rule.id, { enabled: !rule.enabled })
                  }
                  onMoveUp={() => moveRule(rule.id, -1)}
                  onMoveDown={() => moveRule(rule.id, 1)}
                  isFirst={idx === 0}
                  isLast={idx === sortedRules.length - 1}
                />
              ))}
            </div>

            {/* Center: rule editor with progressive disclosure */}
            <div className="rule-editor-pane">
              {!selectedRule && (
                <div className="empty">
                  <div className="empty-icon">
                    <IconWand />
                  </div>
                  <div className="empty-title">Select a rule to edit</div>
                  <div className="empty-sub">
                    Or click <strong>Add rule</strong> to create one. Use a template to get
                    started fast.
                  </div>
                </div>
              )}
              {selectedRule && (
                <RuleEditor
                  key={selectedRule.id}
                  rule={selectedRule}
                  providers={providers}
                  models={models}
                  advancedOpen={!!advancedOpen[selectedRule.id]}
                  onToggleAdvanced={() =>
                    setAdvancedOpen((m) => ({
                      ...m,
                      [selectedRule.id]: !m[selectedRule.id],
                    }))
                  }
                  onPatch={(p) => patchRule(selectedRule.id, p)}
                  onPatchTarget={(p) => patchTarget(selectedRule.id, p)}
                  onPatchNeeds={(p) => patchNeeds(selectedRule.id, p)}
                  onPatchMultimodal={(p) => patchMultimodal(selectedRule.id, p)}
                  onAddFallback={() => addFallback(selectedRule.id)}
                  onPatchFallback={(fbIdx, p) =>
                    patchFallback(selectedRule.id, fbIdx, p)
                  }
                  onRemoveFallback={(fbIdx) =>
                    removeFallback(selectedRule.id, fbIdx)
                  }
                  onDuplicate={() => duplicateRule(selectedRule.id)}
                  onDelete={() => deleteRule(selectedRule.id)}
                  providerLabel={(k) => providerLabel(providers, k)}
                />
              )}
            </div>

            {/* Right: plain-English preview */}
            <div className="rule-preview-pane">
              {selectedRule ? (
                <RulePreview
                  rule={selectedRule}
                  providerLabel={(k) => providerLabel(providers, k)}
                />
              ) : (
                <div className="empty">
                  <div className="empty-icon">
                    <IconInfo />
                  </div>
                  <div className="empty-title">Rule preview</div>
                  <div className="empty-sub">
                    Pick a rule to see a plain-English explanation of when it fires and
                    where it routes.
                  </div>
                </div>
              )}
            </div>
          </div>

          {/* Default tags strip */}
          <div className="section-card" style={{ marginTop: 16 }}>
            <h2 style={{ margin: 0, display: "flex", alignItems: "center", gap: 8 }}>
              Default tags
            </h2>
            <div className="sub" style={{ marginTop: 4, marginBottom: 10 }}>
              Auto-attach these tags to every request. Useful for environment markers like{" "}
              <code>prod</code> or <code>batch</code>.
            </div>
            <ChipEditor
              label=""
              hint="Added to ctx.tags for every rule evaluation (does not overwrite caller-supplied tags)."
              values={defaultTags}
              onChange={(next) => {
                setDefaultTags(next);
                setDirty(true);
              }}
              placeholder="e.g. prod"
            />
          </div>
        </>
      )}
    </>
  );
}

/* ─── Rule list item (draggable card) ──────────────────────────── */

function RuleListItem({
  rule,
  idx,
  isSelected,
  isDragging,
  isDropBefore,
  isDropAfter,
  providerLabel,
  onSelect,
  onDragStart,
  onDragOver,
  onDragEnd,
  onKeyReorder,
  onToggleEnabled,
  onMoveUp,
  onMoveDown,
  isFirst,
  isLast,
}: {
  rule: RoutingRule;
  idx: number;
  isSelected: boolean;
  isDragging: boolean;
  isDropBefore: boolean;
  isDropAfter: boolean;
  providerLabel: (k: ProviderKind) => string;
  onSelect: () => void;
  onDragStart: (e: React.DragEvent<HTMLDivElement>) => void;
  onDragOver: (e: React.DragEvent<HTMLDivElement>) => void;
  onDragEnd: () => void;
  onKeyReorder: (e: React.KeyboardEvent) => void;
  onToggleEnabled: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  isFirst: boolean;
  isLast: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const cardCls =
    "rule-card" +
    (isSelected ? " active" : "") +
    (isDragging ? " dragging" : "") +
    (!rule.enabled ? " disabled" : "");
  return (
    <>
      {isDropBefore && <div className="rule-drop-indicator" aria-hidden />}
      <div
        ref={ref}
        className={cardCls}
        draggable
        onClick={onSelect}
        onDragStart={onDragStart}
        onDragOver={onDragOver}
        onDragEnd={onDragEnd}
        tabIndex={0}
        role="button"
        aria-pressed={isSelected}
        aria-label={`Rule ${idx + 1}: ${rule.name || "(unnamed)"}`}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onSelect();
            return;
          }
          onKeyReorder(e);
        }}
      >
        <div className="rule-card-head">
          <button
            className="rule-grip"
            title="Drag to reorder"
            aria-label="Drag to reorder"
            onClick={(e) => e.stopPropagation()}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <IconGripVertical />
          </button>
          <span className="rule-pri" title={`Priority ${rule.priority}`}>
            #{idx + 1}
          </span>
          <span className={providerDotClass(rule.target.provider)} />
          <div className="rule-card-meta">
            <div className="rule-card-name">
              {rule.name || (
                <em style={{ color: "var(--fg-faint)" }}>(unnamed)</em>
              )}
            </div>
            <div className="rule-card-target mono">
              {providerLabel(rule.target.provider)}/
              {rule.target.model || (
                <em style={{ color: "var(--fg-faint)" }}>(empty)</em>
              )}
            </div>
          </div>
          <div
            className="rule-card-actions"
            onClick={(e) => e.stopPropagation()}
          >
            <label
              className="rule-enable-toggle"
              title={rule.enabled ? "Disable rule" : "Enable rule"}
            >
              <input
                type="checkbox"
                checked={rule.enabled}
                onChange={onToggleEnabled}
                aria-label={rule.enabled ? "Disable rule" : "Enable rule"}
              />
              <span className="rule-enable-track">
                <span className="rule-enable-thumb" />
              </span>
            </label>
            <button
              className="btn ghost icon-only"
              title="Move up"
              aria-label="Move up"
              onClick={onMoveUp}
              disabled={isFirst}
            >
              <IconChevronUp />
            </button>
            <button
              className="btn ghost icon-only"
              title="Move down"
              aria-label="Move down"
              onClick={onMoveDown}
              disabled={isLast}
            >
              <IconChevronDown />
            </button>
          </div>
        </div>
        <div className="rule-card-badges">
          {!rule.enabled && <span className="badge">disabled</span>}
          {rule.needs.vision && <span className="badge info">vision</span>}
          {rule.needs.audio && <span className="badge info">audio</span>}
          {rule.needs.tools && <span className="badge info">tools</span>}
          {rule.needs.min_context ? (
            <span className="badge info">≥{rule.needs.min_context} ctx</span>
          ) : null}
          {rule.prefer_free && <span className="badge ok">free</span>}
          {rule.match_tags_any.length > 0 && (
            <span className="badge warn">
              tag: {rule.match_tags_any.join(", ")}
            </span>
          )}
          {rule.match_model_contains.length > 0 && (
            <span className="badge">
              model: {rule.match_model_contains.join(", ")}
            </span>
          )}
          {rule.max_context_tokens ? (
            <span className="badge">ctx &gt; {rule.max_context_tokens}</span>
          ) : null}
          {rule.targets.length > 0 && (
            <span className="badge">
              <IconLayers /> {rule.targets.length} fallback
            </span>
          )}
        </div>
      </div>
      {isDropAfter && <div className="rule-drop-indicator" aria-hidden />}
    </>
  );
}

/* ─── Rule editor ──────────────────────────────────────────────── */

function RuleEditor({
  rule,
  providers,
  models,
  advancedOpen,
  onToggleAdvanced,
  onPatch,
  onPatchTarget,
  onPatchNeeds,
  onPatchMultimodal,
  onAddFallback,
  onPatchFallback,
  onRemoveFallback,
  onDuplicate,
  onDelete,
  providerLabel,
}: {
  rule: RoutingRule;
  providers: ProviderInfo[];
  models: any[];
  advancedOpen: boolean;
  onToggleAdvanced: () => void;
  onPatch: (patch: Partial<RoutingRule>) => void;
  onPatchTarget: (patch: Partial<RouteTarget>) => void;
  onPatchNeeds: (patch: Partial<CapabilityNeeds>) => void;
  onPatchMultimodal: (patch: Partial<MultimodalNeeds>) => void;
  onAddFallback: () => void;
  onPatchFallback: (fbIdx: number, patch: Partial<RouteTarget>) => void;
  onRemoveFallback: (fbIdx: number) => void;
  onDuplicate: () => void;
  onDelete: () => void;
  providerLabel: (k: ProviderKind) => string;
}) {
  const providerModels = useMemo(() => {
    const providerId = rule.target.provider;
    return models.filter((m: any) => {
      const mp = String(m.provider || "").toLowerCase();
      if (providerId === "openai") return mp === "openai";
      if (providerId === "anthropic") return mp === "anthropic";
      if (providerId === "gemini") return mp === "gemini";
      return mp === providerId;
    });
  }, [models, rule.target.provider]);

  return (
    <div className="section-card">
      <div className="row between" style={{ marginBottom: 12, flexWrap: "wrap", gap: 8 }}>
        <h2 style={{ margin: 0, display: "flex", alignItems: "center", gap: 8 }}>
          Edit rule
          <span style={{ color: "var(--fg-dim)" }}>·</span>
          <span className="mono" style={{ fontSize: 14, color: "var(--fg-dim)" }}>
            {rule.name || "(unnamed)"}
          </span>
        </h2>
        <div className="row" style={{ gap: 6 }}>
          <button className="btn" onClick={onDuplicate} title="Duplicate this rule">
            <IconCopy /> Duplicate
          </button>
          <button className="btn danger" onClick={onDelete} title="Delete this rule">
            <IconTrash /> Delete
          </button>
        </div>
      </div>

      {/* ─── Basic: identity ─── */}
      <div className="section-head" style={{ margin: "12px 0 6px" }}>
        <h2>Identity</h2>
      </div>
      <div className="grid cols-2">
        <Field
          label="Rule name"
          hint="Shown in logs and the test runner. Should be unique-ish, e.g. tools-to-haiku."
        >
          <input
            className="input mono"
            value={rule.name}
            placeholder="e.g. tools-to-haiku"
            onChange={(e) => onPatch({ name: e.target.value })}
          />
        </Field>
        <NumberField
          label="Priority"
          hint="Lower = higher priority. 100 is the catch-all default. Reordering the list above also changes this."
          value={rule.priority}
          min={0}
          step={1}
          onChange={(v) => onPatch({ priority: v ?? 50 })}
        />
      </div>

      {/* ─── Basic: target ─── */}
      <div className="section-head" style={{ margin: "20px 0 6px" }}>
        <h2>Where to send the request</h2>
      </div>
      <div className="target-row">
        <div className="field" style={{ flex: 1, minWidth: 200 }}>
          <label>Provider</label>
          <ProviderSelect
            value={rule.target.provider}
            onChange={(v) => onPatchTarget({ provider: v })}
            providers={providers}
          />
          <div className="hint">
            Built-in: OpenAI, Anthropic, Gemini. Custom for everything else.
          </div>
        </div>
        <div className="field" style={{ flex: 2, minWidth: 240 }}>
          <label>Model</label>
          {providerModels.length > 0 ? (
            <select
              className="select mono"
              value={rule.target.model}
              onChange={(e) => onPatchTarget({ model: e.target.value })}
            >
              <option value="">— pick a model —</option>
              {providerModels.map((m: any) => (
                <option key={m.id} value={m.id}>
                  {m.id} ({m.context_window.toLocaleString()} ctx)
                </option>
              ))}
              {!providerModels.some((m: any) => m.id === rule.target.model) &&
                rule.target.model && (
                  <option value={rule.target.model}>{rule.target.model} (custom)</option>
                )}
            </select>
          ) : (
            <input
              className="input mono"
              value={rule.target.model}
              placeholder="e.g. gpt-5"
              onChange={(e) => onPatchTarget({ model: e.target.value })}
            />
          )}
          <div className="hint">
            Will be sent to{" "}
            <strong className="mono">
              {providerLabel(rule.target.provider)}/{rule.target.model || "(empty)"}
            </strong>
            .
          </div>
        </div>
      </div>

      {/* ─── Basic: match when ─── */}
      <div className="section-head" style={{ margin: "20px 0 6px" }}>
        <h2>Match when…</h2>
        <span className="sub">
          All fields optional. Leave empty to match everything (catch-all).
        </span>
      </div>
      <div className="grid cols-2">
        <ChipEditor
          label="Tags (any of)"
          hint="Match if the request has at least one of these tags, e.g. 'premium', 'prod'."
          values={rule.match_tags_any}
          onChange={(next) => onPatch({ match_tags_any: next })}
          placeholder="e.g. premium"
        />
        <ChipEditor
          label="Tags (all of)"
          hint="Match only when the request has ALL of these tags, e.g. 'prod' AND 'batch'."
          values={rule.match_tags_all}
          onChange={(next) => onPatch({ match_tags_all: next })}
          placeholder="e.g. batch"
        />
        <ChipEditor
          label="Model name contains"
          hint="Match if the requested model name contains any substring, e.g. 'gpt-4-vision'."
          values={rule.match_model_contains}
          onChange={(next) => onPatch({ match_model_contains: next })}
          placeholder="e.g. gpt-4-vision"
        />
        <Field
          label="Capabilities the request needs"
          hint="Tick the boxes that must be true for this rule to fire."
        >
          <div className="row" style={{ gap: 14, flexWrap: "wrap" }}>
            <Toggle
              label="Vision (images)"
              value={!!rule.needs.vision}
              onChange={(v) => onPatchNeeds({ vision: v })}
            />
            <Toggle
              label="Audio"
              value={!!rule.needs.audio}
              onChange={(v) => onPatchNeeds({ audio: v })}
            />
            <Toggle
              label="Tools (function calling)"
              value={!!rule.needs.tools}
              onChange={(v) => onPatchNeeds({ tools: v })}
            />
          </div>
          <div className="row" style={{ gap: 8, alignItems: "center", marginTop: 8 }}>
            <span style={{ fontSize: 12, color: "var(--fg-dim)" }}>Min context:</span>
            <input
              className="input mono"
              type="number"
              value={rule.needs.min_context ?? 0}
              style={{ width: 100 }}
              min={0}
              step={1000}
              onChange={(e) =>
                onPatchNeeds({ min_context: Number(e.target.value) || 0 })
              }
            />
            <span style={{ fontSize: 11, color: "var(--fg-faint)" }}>tokens</span>
          </div>
          <div className="hint" style={{ marginTop: 6 }}>
            Tick 'Vision' if your request includes an image; tick 'Tools' if it uses
            function calling; set Min context for big inputs.
          </div>
        </Field>
      </div>

      {/* ─── Basic: cost preferences ─── */}
      <div className="section-head" style={{ margin: "20px 0 6px" }}>
        <h2>Cost preferences</h2>
      </div>
      <div className="grid cols-2">
        <Field
          label="Prefer free tier"
          hint="Routes to free providers first (e.g. Gemini Flash, OpenRouter free tier)."
        >
          <Toggle
            label="Enable free-tier preference"
            value={rule.prefer_free}
            onChange={(v) => onPatch({ prefer_free: v })}
          />
        </Field>
        <Field
          label="Reason"
          hint="Internal note — why this rule exists. Helps you remember months later."
        >
          <input
            className="input"
            value={rule.reason}
            placeholder="e.g. Vision inputs route to Gemini Pro for best multimodal quality."
            onChange={(e) => onPatch({ reason: e.target.value })}
          />
        </Field>
      </div>

      {/* ─── Basic: fallback chain ─── */}
      <div className="section-head" style={{ margin: "20px 0 6px" }}>
        <h2>Fallback chain</h2>
        <span className="sub">
          If the primary target is unhealthy, try these in order.
        </span>
      </div>
      {rule.targets.length === 0 && (
        <div className="hint" style={{ marginBottom: 8 }}>
          No fallbacks — if the primary target is down, the gateway will return an error.
        </div>
      )}
      {rule.targets.map((fb, fbIdx) => (
        <div key={fbIdx} className="target-row fallback">
          <span className="fallback-idx">#{fbIdx + 1}</span>
          <div className="field" style={{ flex: 1, minWidth: 160 }}>
            <label>Provider</label>
            <ProviderSelect
              value={fb.provider}
              onChange={(v) => onPatchFallback(fbIdx, { provider: v })}
              providers={providers}
            />
          </div>
          <div className="field" style={{ flex: 2, minWidth: 220 }}>
            <label>Model</label>
            <input
              className="input mono"
              value={fb.model}
              placeholder="model id"
              onChange={(e) => onPatchFallback(fbIdx, { model: e.target.value })}
            />
          </div>
          <button
            className="btn ghost icon-only"
            onClick={() => onRemoveFallback(fbIdx)}
            title="Remove fallback"
            aria-label="Remove fallback"
          >
            <IconTrash />
          </button>
        </div>
      ))}
      <button className="btn" onClick={onAddFallback} style={{ marginTop: 8 }}>
        <IconPlus /> Add fallback
      </button>

      {/* ─── Advanced disclosure ─── */}
      <div
        className="section-head advanced-toggle"
        style={{ margin: "20px 0 6px", cursor: "pointer" }}
        onClick={onToggleAdvanced}
        role="button"
        tabIndex={0}
        aria-expanded={advancedOpen}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggleAdvanced();
          }
        }}
      >
        {advancedOpen ? <IconChevronDown /> : <IconChevronRight />}
        <h2>Advanced options</h2>
        <span className="sub">
          Multimodal needs, runtime thresholds, context cap. Hide if you don't need them.
        </span>
      </div>
      {advancedOpen && (
        <div className="advanced-body">
          <div className="grid cols-2">
            <Field
              label="Multimodal content present"
              hint="Match only when the request includes the chosen attachment type."
            >
              <div className="row" style={{ gap: 14, flexWrap: "wrap" }}>
                <Toggle
                  label="Image"
                  value={!!rule.when_multimodal.image}
                  onChange={(v) => onPatchMultimodal({ image: v })}
                />
                <Toggle
                  label="Audio"
                  value={!!rule.when_multimodal.audio}
                  onChange={(v) => onPatchMultimodal({ audio: v })}
                />
                <Toggle
                  label="PDF"
                  value={!!rule.when_multimodal.pdf}
                  onChange={(v) => onPatchMultimodal({ pdf: v })}
                />
              </div>
            </Field>
            <NumberField
              label="Max context threshold"
              hint="Match only when approx input tokens are STRICTLY greater than this. Useful for routing to 1M-context models."
              value={rule.max_context_tokens}
              suffix="tokens"
              min={0}
              step={1000}
              onChange={(v) => onPatch({ max_context_tokens: v })}
              placeholder="e.g. 100000"
            />
          </div>

          <div className="section-head" style={{ margin: "16px 0 6px" }}>
            <h2>Runtime thresholds</h2>
            <span className="sub">
              Match only when the live metrics are in range. Leave empty to ignore.
            </span>
          </div>
          <div className="grid cols-3">
            <NumberField
              label="Match latency below"
              hint="Only fire when current p50 latency is under this."
              value={rule.match_latency_below_ms}
              suffix="ms"
              onChange={(v) => onPatch({ match_latency_below_ms: v })}
              placeholder="e.g. 2000"
            />
            <NumberField
              label="Match cost below"
              hint="Only fire when the per-million-token cost is under this."
              value={rule.match_cost_below_per_million}
              suffix="$/M tok"
              step={0.5}
              onChange={(v) => onPatch({ match_cost_below_per_million: v })}
              placeholder="e.g. 5"
            />
            <NumberField
              label="Quota below"
              hint="Only fire when current quota usage is under this percentage."
              value={rule.match_quota_below_pct}
              suffix="%"
              min={0}
              max={100}
              step={5}
              onChange={(v) => onPatch({ match_quota_below_pct: v })}
              placeholder="e.g. 80"
            />
            <NumberField
              label="Benchmark above"
              hint="Only fire when the model's benchmark score is at or above this."
              value={rule.match_benchmark_above}
              suffix="score"
              min={0}
              max={100}
              step={1}
              onChange={(v) => onPatch({ match_benchmark_above: v })}
              placeholder="e.g. 70"
            />
          </div>
        </div>
      )}
    </div>
  );
}

/* ─── Plain-English preview pane ───────────────────────────────── */

function RulePreview({
  rule,
  providerLabel,
}: {
  rule: RoutingRule;
  providerLabel: (k: ProviderKind) => string;
}) {
  return (
    <div className="section-card rule-preview-card">
      <h2 style={{ margin: 0, display: "flex", alignItems: "center", gap: 8 }}>
        <IconInfo /> What this rule does
      </h2>
      <div className="sub" style={{ marginTop: 4, marginBottom: 14 }}>
        A plain-English summary. If the match description doesn't match your intent, tweak
        the fields on the left.
      </div>

      <div className="preview-row">
        <span className="preview-label">Name</span>
        <span className="preview-value mono">
          {rule.name || <em style={{ color: "var(--fg-faint)" }}>(unnamed)</em>}
        </span>
      </div>

      <div className="preview-row">
        <span className="preview-label">Routes to</span>
        <span className="preview-value">
          <span className={providerDotClass(rule.target.provider)} />
          <strong className="mono">
            {providerLabel(rule.target.provider)}/{rule.target.model || "(empty)"}
          </strong>
        </span>
      </div>

      {rule.targets.length > 0 && (
        <div className="preview-row">
          <span className="preview-label">Fallbacks</span>
          <span className="preview-value preview-fallbacks">
            {rule.targets.map((t, i) => (
              <span key={i} className="mono preview-fallback">
                <IconArrowRight /> {providerLabel(t.provider)}/{t.model || "(empty)"}
              </span>
            ))}
          </span>
        </div>
      )}

      <div className="preview-divider" />

      <div className="preview-row preview-row-stack">
        <span className="preview-label">Match condition</span>
        <span className="preview-match">
          <IconZap /> {matchPreview(rule)}
        </span>
      </div>

      {rule.reason && (
        <>
          <div className="preview-divider" />
          <div className="preview-row preview-row-stack">
            <span className="preview-label">Why</span>
            <span className="preview-reason">{rule.reason}</span>
          </div>
        </>
      )}

      <div className="preview-divider" />

      <div className="preview-row">
        <span className="preview-label">Enabled</span>
        <span className="preview-value">
          {rule.enabled ? (
            <span className="badge ok">on</span>
          ) : (
            <span className="badge">off (skipped)</span>
          )}
        </span>
      </div>
    </div>
  );
}

/* ─── Test runner panel ────────────────────────────────────────── */

function TestRunner(props: {
  simModel: string;
  setSimModel: (v: string) => void;
  simTags: string[];
  setSimTags: (v: string[]) => void;
  simVision: boolean;
  setSimVision: (v: boolean) => void;
  simTools: boolean;
  setSimTools: (v: boolean) => void;
  simAudio: boolean;
  setSimAudio: (v: boolean) => void;
  simHasImage: boolean;
  setSimHasImage: (v: boolean) => void;
  simHasAudio: boolean;
  setSimHasAudio: (v: boolean) => void;
  simHasPdf: boolean;
  setSimHasPdf: (v: boolean) => void;
  simTokens: number;
  setSimTokens: (v: number) => void;
  evaluated: { rule: RoutingRule; idx: number; hit: boolean }[];
  decision: RouteTarget | null;
  providerLabelFn: (k: ProviderKind) => string;
  onSelect: (id: string) => void;
}) {
  const firstHit = props.evaluated.find((e) => e.hit);
  return (
    <div className="section-card">
      <div className="row between" style={{ marginBottom: 10 }}>
        <h2 style={{ margin: 0, display: "flex", alignItems: "center", gap: 8 }}>
          <IconBeaker /> Test runner
        </h2>
        <span className="sub">Simulate a request and see which rule wins.</span>
      </div>
      <div className="grid cols-3">
        <Field
          label="Model name"
          hint="Substring matched against match_model_contains."
        >
          <input
            className="input mono"
            value={props.simModel}
            onChange={(e) => props.setSimModel(e.target.value)}
            placeholder="gpt-5"
          />
        </Field>
        <NumberField
          label="Approx input tokens"
          hint="~4 chars per token."
          value={props.simTokens || null}
          onChange={(v) => props.setSimTokens(v ?? 0)}
          placeholder="0"
        />
        <ChipEditor
          label="Tags"
          hint="Added to the request tags. Default tags are auto-included."
          values={props.simTags}
          onChange={props.setSimTags}
          placeholder="e.g. premium"
        />
      </div>
      <div className="row" style={{ gap: 14, flexWrap: "wrap", marginTop: 8 }}>
        <Toggle label="Needs vision" value={props.simVision} onChange={props.setSimVision} />
        <Toggle label="Needs audio" value={props.simAudio} onChange={props.setSimAudio} />
        <Toggle label="Needs tools" value={props.simTools} onChange={props.setSimTools} />
        <span style={{ width: 1, background: "var(--border)", height: 18 }} />
        <Toggle label="Has image" value={props.simHasImage} onChange={props.setSimHasImage} />
        <Toggle label="Has audio" value={props.simHasAudio} onChange={props.setSimHasAudio} />
        <Toggle label="Has PDF" value={props.simHasPdf} onChange={props.setSimHasPdf} />
      </div>

      <div
        className="decision-banner"
        style={{
          borderColor: props.decision
            ? "rgba(61, 220, 151, 0.5)"
            : "rgba(245, 165, 36, 0.4)",
        }}
      >
        {props.decision ? (
          <>
            <div className="decision-label">
              <IconZap /> ROUTES TO
            </div>
            <div className="decision-target">
              <span className="badge info">
                {props.providerLabelFn(props.decision.provider)}
              </span>{" "}
              <span className="mono">{props.decision.model || "(empty)"}</span>
            </div>
            {firstHit && (
              <div className="decision-reason">
                matched by <strong>{firstHit.rule.name || "(unnamed)"}</strong>
              </div>
            )}
          </>
        ) : (
          <>
            <div className="decision-label warn">
              <IconInfo /> NO ROUTE
            </div>
            <div className="decision-reason">
              No rule matched the simulated request. The gateway will return an error.
            </div>
          </>
        )}
      </div>

      <div className="eval-list">
        {props.evaluated.map(({ rule, idx, hit }) => (
          <div
            key={rule.id}
            className={"eval-row" + (hit ? " hit" : "")}
            onClick={() => props.onSelect(rule.id)}
          >
            <span className="eval-idx">#{idx + 1}</span>
            <span className="eval-pri">p{rule.priority}</span>
            <span className={"eval-name" + (rule.name ? "" : " unnamed")}>
              {rule.name || <em>(unnamed)</em>}
            </span>
            <span className="eval-arrow">→</span>
            <span className="eval-target mono">
              {props.providerLabelFn(rule.target.provider)}/
              {rule.target.model || "(empty)"}
            </span>
            {rule.targets.length > 0 && (
              <span className="badge">
                <IconLayers /> {rule.targets.length}
              </span>
            )}
            <span className="spacer" />
            {hit ? (
              <span className="badge ok">matches</span>
            ) : (
              <span className="badge">—</span>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
