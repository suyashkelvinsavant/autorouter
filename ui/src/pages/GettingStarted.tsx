import { useState } from "react";
import { useToast } from "../components/Toast";
import { BrandMark } from "../components/Icons";
import { IconCheck, IconArrowRight, IconPlug, IconZap } from "../components/Icons";

const STEPS = [
  {
    title: "Add your first provider",
    description: "Configure an AI provider (OpenAI, Anthropic, OpenRouter, etc.) with your API key.",
    icon: IconPlug,
  },
  {
    title: "AutoRouter sets the default",
    description: "Your first provider automatically becomes the default. No need to manually configure it.",
    icon: IconZap,
  },
  {
    title: "Connect your AI tool",
    description: "Point Claude Code, Cursor, Aider, or any AI tool at the local gateway endpoint.",
    icon: IconCheck,
  },
];

export function GettingStarted({ onNavigate }: { onNavigate: (page: string) => void }) {
  const [currentStep, setCurrentStep] = useState(0);
  const { show } = useToast();

  const nextStep = () => {
    if (currentStep < STEPS.length - 1) {
      setCurrentStep(currentStep + 1);
    } else {
      onNavigate("providers");
    }
  };

  const skipToProviders = () => {
    onNavigate("providers");
  };

  const QuickPreset = ({
    name,
    id,
    baseUrl,
    apiKeyEnvVar,
    defaultModel,
  }: {
    name: string;
    id: string;
    baseUrl: string;
    apiKeyEnvVar: string;
    defaultModel: string;
  }) => {
    const [adding, setAdding] = useState(false);
    const [added, setAdded] = useState(false);

    const addProvider = async () => {
      setAdding(true);
      try {
        const { api } = await import("../api");
        const isCustom = id === "openrouter";
        const patch = isCustom
          ? {
              providers: {
                custom: {
                  openrouter: {
                    display_name: name,
                    base_url: baseUrl,
                    enabled: true,
                    api_format: "openai",
                    api_key_secret_id: `env:${apiKeyEnvVar}`,
                    model_allowlist: [defaultModel],
                  },
                },
              },
            }
          : {
              providers: {
                [id]: {
                  base_url: baseUrl,
                  enabled: true,
                  api_key_secret_id: `env:${apiKeyEnvVar}`,
                },
              },
            };
        await api.patchSettings(patch);

        await api.patchSettings({
          defaults: {
            default_provider: id,
            default_model: defaultModel,
          },
        });

        setAdded(true);
        show(`Added ${name}! Set ${apiKeyEnvVar} before sending requests.`, "ok");
        setTimeout(() => onNavigate("providers"), 1500);
      } catch (e) {
        show(String(e), "err");
      } finally {
        setAdding(false);
      }
    };

    return (
      <button
        type="button"
        className={`getting-started-preset ${added ? "added" : ""}`}
        onClick={addProvider}
        disabled={adding || added}
      >
        <div className="getting-started-preset-name">{name}</div>
        <div className="getting-started-preset-desc">
          {added ? "Added!" : adding ? "Adding..." : "Quick add"}
        </div>
      </button>
    );
  };

  return (
    <div className="getting-started">
      <div className="getting-started-card">
        <div className="getting-started-header">
          <div className="getting-started-brand">
            <BrandMark width={40} height={40} />
            <span>AutoRouter</span>
          </div>
          <div className="getting-started-title">
            Welcome! Let's get you set up
          </div>
          <div className="getting-started-sub">
            Configure your first AI provider to start routing requests through the
            local gateway.
          </div>
        </div>

        {/* Progress steps */}
        <div className="getting-started-steps">
          {STEPS.map((step, i) => {
            const Icon = step.icon;
            const isPast = i < currentStep;
            const isCurrent = i === currentStep;
            const isFuture = i > currentStep;

            return (
              <div
                key={i}
                className={`getting-started-step-item ${
                  isCurrent ? "current" : ""
                } ${isPast ? "past" : ""}`}
              >
                <div className="getting-started-step-icon">
                  {isPast ? <IconCheck /> : <Icon />}
                </div>
                <div className="getting-started-step-content">
                  <div className="getting-started-step-title">{step.title}</div>
                  <div className="getting-started-step-desc">
                    {step.description}
                  </div>
                </div>
                {i < STEPS.length - 1 && (
                  <div className="getting-started-step-arrow">
                    <IconArrowRight />
                  </div>
                )}
              </div>
            );
          })}
        </div>

        {/* Quick add presets */}
        {currentStep === 0 && (
          <div className="getting-started-presets">
            <div className="getting-started-presets-title">
              Quick setup — add a free provider:
            </div>
            <div className="getting-started-presets-list">
              <QuickPreset
                name="OpenRouter (free)"
                id="openrouter"
                baseUrl="https://openrouter.ai/api/v1"
                apiKeyEnvVar="OPENROUTER_API_KEY"
                defaultModel="nvidia/nemotron-3-ultra-550b-a55b:free"
              />
            </div>
            <div className="getting-started-presets-note">
              You'll need an API key. OpenRouter offers free models.
            </div>
          </div>
        )}

        {/* Actions */}
        <div className="getting-started-actions">
          {currentStep === 0 && (
            <>
              <button type="button" className="btn ghost" onClick={skipToProviders}>
                I'll configure manually
              </button>
              <button type="button" className="btn primary" onClick={nextStep}>
                Next <IconArrowRight />
              </button>
            </>
          )}
          {currentStep === 1 && (
            <>
              <button type="button" className="btn ghost" onClick={() => setCurrentStep(0)}>
                Back
              </button>
              <button type="button" className="btn primary" onClick={nextStep}>
                Next <IconArrowRight />
              </button>
            </>
          )}
          {currentStep === 2 && (
            <>
              <button type="button" className="btn ghost" onClick={() => setCurrentStep(1)}>
                Back
              </button>
              <button type="button" className="btn primary" onClick={skipToProviders}>
                Go to Providers <IconArrowRight />
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
