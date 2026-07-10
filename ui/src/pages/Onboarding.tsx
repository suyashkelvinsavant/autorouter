import { BrandMark, IconRefresh } from "../components/Icons";
import { CopyButton } from "../components/CopyButton";
import { useToast } from "../components/Toast";

const DEFAULT_BIND = "127.0.0.1:4073";
const BASE_URL = `http://${DEFAULT_BIND}`;
const SOURCE_HEADER = "X-AutoRouter-Source: openai | anthropic | gemini";
const PYTHON_SNIPPET = `from openai import OpenAI
client = OpenAI(
    base_url="${BASE_URL}/v1",
    api_key="any-non-empty-string",  # gateway uses its own key
    default_headers={"X-AutoRouter-Source": "openai"},
)
print(client.chat.completions.create(
    model="gpt-4o-mini",
    messages=[{"role": "user", "content": "hello"}],
).choices[0].message.content)`;

const STEPS: { title: string; body: React.ReactNode }[] = [
  {
    title: "Start the desktop app",
    body: (
      <>
        The gateway binds to <code>{DEFAULT_BIND}</code> by default.
        Override with the <code>AUTOROUTER_BIND</code> environment variable.
      </>
    ),
  },
  {
    title: "Point an AI tool at the local endpoint",
    body: (
      <>
        Set the tool's base URL to <code>{BASE_URL}</code> and add the
        <code> X-AutoRouter-Source</code> header to declare which provider
        you are impersonating (e.g. <code>openai</code>).
      </>
    ),
  },
  {
    title: "Open the dashboard",
    body: (
      <>
        Sessions, providers, and live logs update as requests flow.
        Copy the snippet below to make your first call.
      </>
    ),
  },
];

export function Onboarding({
  error,
  onRetry,
}: {
  error: string;
  onRetry: () => void;
}) {
  const { show } = useToast();

  const openDashboard = () => {
    const url = `${BASE_URL}/ui/?page=dashboard`;
    try {
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      show("Could not open browser", "err");
    }
  };

  return (
    <div className="onboarding-v2">
      <div className="onboarding-v2-card">
        <div className="onboarding-v2-head">
          <div className="onboarding-v2-brand">
            <BrandMark width={48} height={48} />
          </div>
          <div>
            <h1>AutoRouter</h1>
            <div className="sub">
              Local-first AI protocol translation. The gateway routes your
              AI tool requests to the right provider, on a single local
              endpoint.
            </div>
          </div>
        </div>

        {error ? (
          <div className="onboarding-v2-error" role="alert">
            <strong>Gateway not reachable.</strong> {error}
            <button type="button" className="btn" onClick={onRetry}>
              <IconRefresh /> Retry
            </button>
          </div>
        ) : null}

        <div className="onboarding-v2-endpoint">
          <div className="onboarding-v2-label">Local endpoint</div>
          <div className="onboarding-v2-endpoint-row">
            <code className="onboarding-v2-bind">{BASE_URL}</code>
            <CopyButton
              text={BASE_URL}
              size="md"
              variant="inline"
              successMsg="Endpoint copied"
              title="Copy endpoint URL"
            />
          </div>
        </div>

        <div className="onboarding-v2-steps">
          {STEPS.map((step, i) => (
            <div key={i} className="onboarding-v2-step">
              <div className="onboarding-v2-step-n">{i + 1}</div>
              <div className="onboarding-v2-step-body">
                <div className="onboarding-v2-step-title">{step.title}</div>
                <div className="onboarding-v2-step-text">{step.body}</div>
              </div>
            </div>
          ))}
        </div>

        <div className="onboarding-v2-source">
          <div className="onboarding-v2-label">X-AutoRouter-Source header</div>
          <div className="onboarding-v2-source-row">
            <code>{SOURCE_HEADER}</code>
            <CopyButton
              text={SOURCE_HEADER}
              size="sm"
              variant="inline"
              successMsg="Header copied"
            />
          </div>
        </div>

        <div className="onboarding-v2-snippet">
          <div className="onboarding-v2-label">Quick Python call</div>
          <pre className="onboarding-v2-code">
            <code>{PYTHON_SNIPPET}</code>
            <CopyButton
              text={PYTHON_SNIPPET}
              size="sm"
              variant="inline"
              successMsg="Snippet copied"
              title="Copy Python snippet"
            />
          </pre>
        </div>

        <div className="onboarding-v2-actions">
          <button type="button" className="btn" onClick={openDashboard}>
            Open dashboard
          </button>
          <button type="button" className="btn primary" onClick={onRetry}>
            <IconRefresh /> Retry connection
          </button>
        </div>
      </div>
    </div>
  );
}
