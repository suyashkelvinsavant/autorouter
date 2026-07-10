export interface StatusResponse {
  version: string;
  bind: string;
  started_at: string;
  uptime_seconds: number;
  log_lines: number;
  session_count: number;
  providers: {
    openai: "configured" | "disabled" | "missing";
    anthropic: "configured" | "disabled" | "missing";
    gemini: "configured" | "disabled" | "missing";
  };
}

export interface ProviderInfo {
  id: string;
  kind: "openai" | "anthropic" | "gemini" | "custom";
  display_name: string;
  base_url: string;
  enabled: boolean;
  api_key_secret_id: string | null;
  default_headers: Record<string, string>;
  model_allowlist: string[];
  /** Wire format auto-detected from base_url, or explicitly set. */
  api_format: "openai" | "anthropic" | "gemini";
}

export interface ModelInfo {
  id: string;
  provider: string;
  context_window: number;
  max_output_tokens: number;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_audio: boolean;
  supports_streaming: boolean;
}

export interface ProvidersResponse {
  providers: ProviderInfo[];
  models: ModelInfo[];
}

export interface SessionInfo {
  id: string;
  label: string | null;
  source_provider: string;
  created_at: string;
  last_request_at: string | null;
  last_request_id: string | null;
  request_count: number;
}

export interface SessionsResponse {
  sessions: SessionInfo[];
}

export interface LogEntry {
  ts: string;
  level: string;
  target: string;
  message: string;
}

export interface LogsResponse {
  lines: LogEntry[];
  next_since: number;
}

export interface AppConfig {
  server: {
    bind: string;
    max_body_bytes: number;
    request_timeout_seconds: number;
    stream_idle_timeout_seconds: number;
    enable_cors: boolean;
    require_auth: boolean;
    auth_token: string | null;
  };
  /**
   * `true` when the gateway has a bearer credential configured.
   * Returned alongside `server.auth_token` (which is always `null`
   * for security) so the UI can show "(set — type to replace)"
   * without leaking the existing value.
   */
  has_auth_token: boolean;
  providers: {
    openai?: ProviderInfo;
    anthropic?: ProviderInfo;
    gemini?: ProviderInfo;
    custom: Record<string, ProviderInfo>;
  };
  defaults: {
    default_model: string;
    default_provider: string;
    stream_by_default: boolean;
    max_total_tokens: number;
  };
  storage: {
    data_dir: string;
    database_file: string;
    backup_on_shutdown: boolean;
    backup_keep: number;
  };
  logging: {
    level: string;
    json: boolean;
    file: string | null;
  };
  routing: {
    rules: unknown[];
    default_tags: string[];
  };
}
