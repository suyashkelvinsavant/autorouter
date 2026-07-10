use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use crate::AppState;
use autorouter_config::AppConfig;
use autorouter_core::ProviderKind;

const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const ARTIFICIAL_ANALYSIS_LEADERBOARD_URL: &str =
    "https://artificialanalysis.ai/leaderboards/models";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapedModel {
    pub id: String,
    pub name: String,
    pub input_price_per_million: f64,
    pub output_price_per_million: f64,
    pub blended_price_per_million: Option<f64>,
    pub context_length: u32,
    pub intelligence_index: Option<f64>,
    pub coding_index: Option<f64>,
    pub is_free: bool,
    pub provider: String,
}

/// Canonical form of a model id, used as the *only* comparison key
/// across the model DB, failover candidate matching, and allowlist
/// lookups. Lowercases and strips any `provider/` (or vendor-alias)
/// prefix so that:
///
///   * `"gpt-4o"`, `"openai/gpt-4o"`, `"OpenAI/GPT-4o"` all collapse to
///     `"gpt-4o"`, and
///   * the bidirectional-`contains` confusion from the original `get()`
///     (where `"gpt-4o"` also matched `"gpt-4o-mini"`) cannot occur.
///
/// Returns a borrowed view when possible (no allocation), owned
/// otherwise. Callers must use this — direct `String ==` comparisons
/// against model ids are inconsistent and have already shipped bugs
/// (the `find_similar` skip used to compare `"gpt-4o"` against
/// `"openai/gpt-4o"` and fail to skip the failed model).
pub fn canonical_model_id(id: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = id.trim();
    let lower = trimmed.to_ascii_lowercase();
    match lower.split_once('/') {
        Some((prefix, rest)) if !prefix.is_empty() && !rest.is_empty() => {
            Cow::Owned(rest.to_string())
        }
        _ => Cow::Owned(lower),
    }
}

/// True when `a` and `b` refer to the same logical model under
/// [`canonical_model_id`]. The single comparison all model-matching
/// code paths should funnel through.
pub fn same_model(a: &str, b: &str) -> bool {
    canonical_model_id(a) == canonical_model_id(b)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelDb {
    pub models: Vec<ScrapedModel>,
}

impl ModelDb {
    pub fn new() -> Self {
        Self { models: Vec::new() }
    }

    pub fn load_or_default(data_dir: &Path) -> Self {
        let db_path = data_dir.join("models_data.json");
        if db_path.exists() {
            if let Ok(text) = std::fs::read_to_string(&db_path) {
                if let Ok(db) = serde_json::from_str::<ModelDb>(&text) {
                    if !db.models.is_empty() {
                        tracing::info!(
                            count = db.models.len(),
                            "Loaded models database from models_data.json"
                        );
                        return db;
                    }
                }
            }
        }

        // Fallback default models if file does not exist or parsing fails
        tracing::warn!("Failed to load models_data.json, using bundled default models");
        Self::bundled_defaults()
    }

    /// A minimal fallback used when `models_data.json` is absent or
    /// corrupt. Aligned with the adapter `models()` lists so the
    /// pricing/capability data matches the well-known models the
    /// gateway already advertises. The full enriched DB is populated
    /// by the background scraping job when `features.model_scraping`
    /// is enabled.
    pub fn bundled_defaults() -> Self {
        let defaults = vec![
            ScrapedModel {
                id: "openai/gpt-5".to_string(),
                name: "GPT-5".to_string(),
                input_price_per_million: 5.00,
                output_price_per_million: 15.00,
                blended_price_per_million: Some(7.0),
                context_length: 200000,
                intelligence_index: Some(55.0),
                coding_index: Some(50.0),
                is_free: false,
                provider: "openai".to_string(),
            },
            ScrapedModel {
                id: "openai/gpt-5-mini".to_string(),
                name: "GPT-5 mini".to_string(),
                input_price_per_million: 0.25,
                output_price_per_million: 2.00,
                blended_price_per_million: Some(0.775),
                context_length: 200000,
                intelligence_index: Some(40.0),
                coding_index: Some(35.0),
                is_free: false,
                provider: "openai".to_string(),
            },
            ScrapedModel {
                id: "anthropic/claude-sonnet-4-5".to_string(),
                name: "Claude Sonnet 4.5".to_string(),
                input_price_per_million: 3.00,
                output_price_per_million: 15.00,
                blended_price_per_million: Some(6.60),
                context_length: 200000,
                intelligence_index: Some(53.0),
                coding_index: Some(52.0),
                is_free: false,
                provider: "anthropic".to_string(),
            },
            ScrapedModel {
                id: "anthropic/claude-haiku-4-5".to_string(),
                name: "Claude Haiku 4.5".to_string(),
                input_price_per_million: 0.80,
                output_price_per_million: 4.00,
                blended_price_per_million: Some(1.76),
                context_length: 200000,
                intelligence_index: Some(42.0),
                coding_index: Some(35.0),
                is_free: false,
                provider: "anthropic".to_string(),
            },
            ScrapedModel {
                id: "anthropic/claude-opus-4-5".to_string(),
                name: "Claude Opus 4.5".to_string(),
                input_price_per_million: 15.00,
                output_price_per_million: 75.00,
                blended_price_per_million: Some(33.00),
                context_length: 200000,
                intelligence_index: Some(56.0),
                coding_index: Some(48.0),
                is_free: false,
                provider: "anthropic".to_string(),
            },
            ScrapedModel {
                id: "google/gemini-2.5-pro".to_string(),
                name: "Gemini 2.5 Pro".to_string(),
                input_price_per_million: 1.25,
                output_price_per_million: 10.00,
                blended_price_per_million: Some(3.875),
                context_length: 2000000,
                intelligence_index: Some(47.0),
                coding_index: Some(40.0),
                is_free: false,
                provider: "google".to_string(),
            },
            ScrapedModel {
                id: "google/gemini-2.5-flash".to_string(),
                name: "Gemini 2.5 Flash".to_string(),
                input_price_per_million: 0.0,
                output_price_per_million: 0.0,
                blended_price_per_million: Some(0.0),
                context_length: 1000000,
                intelligence_index: Some(41.0),
                coding_index: Some(34.0),
                is_free: true,
                provider: "google".to_string(),
            },
        ];
        Self { models: defaults }
    }

    /// Look up a scraped model by id. Matching is canonicalized first
    /// (lowercase + stripped provider prefix via [`canonical_model_id`]),
    /// then falls back to a *one-directional* substring heuristic where
    /// the request id contains the stored id. The previous bidirectional
    /// `contains` match was unsafe — `"gpt-4o"` matched `"gpt-4o-mini"` by
    /// accident (the stored id contains the request id). Now only
    /// *longer* request ids can alias *shorter* stored ids, which keeps
    /// the natural "user sent a bare id, db has `openai/gpt-4o`" case
    /// working without producing false hits.
    pub fn get(&self, model_id: &str) -> Option<&ScrapedModel> {
        let canon = canonical_model_id(model_id);
        if let Some(m) = self
            .models
            .iter()
            .find(|m| canonical_model_id(&m.id) == canon)
        {
            return Some(m);
        }
        self.models.iter().find(|m| {
            let m_canon = canonical_model_id(&m.id);
            m_canon.len() < canon.len() && canon.contains(m_canon.as_ref())
        })
    }

    pub fn all(&self) -> &[ScrapedModel] {
        &self.models
    }
}

/// Classify a model id into a provider family. Used to slot scraped
/// OpenRouter/Artificial-Analysis ids into the four built-in
/// `ProviderKind`s. Matching is deliberately conservative: bare
/// `contains("o1")` would misclassify ids like `neo1-mini` or
/// `pro3-medium`. We instead look for:
///   * a `provider/...` prefix (the OpenRouter shape), or
///   * well-known family *namespaces* — `openai`, `gpt-`, `anthropic`,
///     `claude`, `gemini`, `google` — appearing at the start of the id
///     or right after a `/`, or, for `o1`/`o3` only, as a *whole
///     segment* (`openai/o1-mini` matches; `neo1` does not).
pub fn detect_provider_from_id(id: &str) -> String {
    let lower = id.to_ascii_lowercase();

    // Helper: does the id start with `prefix` (at offset 0 or right
    // after a `/`)?
    fn starts_with_segment(haystack: &str, prefix: &str) -> bool {
        haystack == prefix
            || haystack.starts_with(&format!("{prefix}/"))
            || haystack.starts_with(&format!("{prefix}-"))
    }

    // The `provider/model` shape is authoritative when it exists —
    // the OpenRouter convention.
    if let Some((prefix, _rest)) = lower.split_once('/') {
        return match prefix {
            "openai" => "openai".to_string(),
            "anthropic" => "anthropic".to_string(),
            "google" => "gemini".to_string(),
            other => other.to_string(),
        };
    }

    // No slash — classify by family-namespace, not bare substring.
    if starts_with_segment(&lower, "gpt")
        || starts_with_segment(&lower, "openai")
        || starts_with_segment(&lower, "o1")
        || starts_with_segment(&lower, "o3")
    {
        return "openai".to_string();
    }
    if starts_with_segment(&lower, "claude") || starts_with_segment(&lower, "anthropic") {
        return "anthropic".to_string();
    }
    if starts_with_segment(&lower, "gemini") || starts_with_segment(&lower, "google") {
        return "gemini".to_string();
    }
    "custom".to_string()
}

pub fn provider_kind_from_str(s: &str) -> ProviderKind {
    match s.to_ascii_lowercase().as_str() {
        "openai" => ProviderKind::OpenAI,
        "anthropic" | "claude" => ProviderKind::Anthropic,
        "gemini" | "google" => ProviderKind::Gemini,
        _ => ProviderKind::Custom,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OrModelsResponse {
    data: Vec<OrModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct OrModel {
    id: String,
    name: String,
    pricing: OrPricing,
    context_length: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct OrPricing {
    prompt: String,
    completion: String,
}

struct AaModel {
    name: String,
    intelligence_index: Option<f64>,
    coding_index: Option<f64>,
    price_in: Option<f64>,
    price_out: Option<f64>,
    price_blended: Option<f64>,
    openrouter_api_id: Option<String>,
    context_window: Option<u32>,
    is_free: bool,
}

pub async fn run_scraping_job() -> Result<ModelDb, String> {
    tracing::info!("Starting AI model database scraping task...");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    // 1. Fetch OpenRouter models
    let or_resp = client
        .get(OPENROUTER_MODELS_URL)
        .send()
        .await
        .map_err(|e| format!("OpenRouter fetch failed: {e}"))?;

    let or_data: OrModelsResponse = or_resp
        .json()
        .await
        .map_err(|e| format!("OpenRouter JSON parse failed: {e}"))?;

    let mut merged: HashMap<String, ScrapedModel> = HashMap::new();
    for m in or_data.data {
        let price_in = m.pricing.prompt.parse::<f64>().unwrap_or(0.0) * 1_000_000.0;
        let price_out = m.pricing.completion.parse::<f64>().unwrap_or(0.0) * 1_000_000.0;
        let provider = detect_provider_from_id(&m.id);
        let is_free = price_in == 0.0 && price_out == 0.0;

        merged.insert(
            m.id.clone(),
            ScrapedModel {
                id: m.id.clone(),
                name: m.name.clone(),
                input_price_per_million: price_in,
                output_price_per_million: price_out,
                blended_price_per_million: Some(price_in * 0.7 + price_out * 0.3),
                context_length: m.context_length,
                intelligence_index: None,
                coding_index: None,
                is_free,
                provider,
            },
        );
    }

    // 2. Fetch Artificial Analysis models leaderboard.
    // Cap the response body to prevent an oversized or hostile
    // response from exhausting memory. 8 MiB is generous for a
    // leaderboard page.
    const AA_MAX_BYTES: usize = 8 * 1024 * 1024;
    let aa_resp = client.get(ARTIFICIAL_ANALYSIS_LEADERBOARD_URL)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .send()
        .await
        .map_err(|e| format!("Artificial Analysis fetch failed: {e}"))?;

    let body_bytes = aa_resp
        .bytes()
        .await
        .map_err(|e| format!("Artificial Analysis body read failed: {e}"))?;
    if body_bytes.len() > AA_MAX_BYTES {
        return Err(format!(
            "Artificial Analysis response exceeded {} byte cap (got {}); aborting scrape",
            AA_MAX_BYTES,
            body_bytes.len()
        ));
    }
    let html = String::from_utf8_lossy(&body_bytes).into_owned();

    static NAME_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"name\\?":\s*\\?"([^"\\]{3,60}?)\\?""#).unwrap());
    static INTEL_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"intelligenceIndex\\?":\s*([\d.]+)"#).unwrap());
    static CODING_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"codingIndex\\?":\s*([\d.]+)"#).unwrap());
    static PRICE_IN_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"price1mInputTokens\\?":\s*\\?"?([\d.]+)"#).unwrap());
    static PRICE_OUT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"price1mOutputTokens\\?":\s*\\?"?([\d.]+)"#).unwrap());
    static PRICE_BLENDED_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"price1mBlended7To2To1\\?":\s*([\d.]+)"#).unwrap());
    static OPENROUTER_API_ID_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"openrouterApiId\\?":\s*\\?"([^"\\]+)"#).unwrap());
    static CONTEXT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"contextWindowTokens\\?":\s*(\d+)"#).unwrap());
    static IS_FREE_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\\?"isFree\\?":\s*(true|false)"#).unwrap());

    let mut aa_models = Vec::new();
    let mut seen = HashSet::new();

    for cap in NAME_RE.captures_iter(&html) {
        let name = cap.get(1).unwrap().as_str().to_string();
        if seen.contains(&name) {
            continue;
        }
        seen.insert(name.clone());

        let start = cap.get(0).unwrap().start();
        // Snap the end index to a valid UTF-8 char boundary.
        // `html` is produced via `String::from_utf8_lossy`, which may
        // introduce 3-byte U+FFFD replacement characters. Slicing at an
        // arbitrary byte offset can land inside such a multi-byte char
        // and panic.  Walk backward from the desired end until we hit a
        // char boundary.
        let mut end = (start + 4000).min(html.len());
        while end > start && !html.is_char_boundary(end) {
            end -= 1;
        }
        let chunk = &html[start..end];

        let intelligence_index = INTEL_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok());

        if intelligence_index.is_none() {
            continue;
        }

        let coding_index = CODING_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok());

        let price_in = PRICE_IN_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok());

        let price_out = PRICE_OUT_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok());

        let price_blended = PRICE_BLENDED_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok());

        let openrouter_api_id = OPENROUTER_API_ID_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        let context_window = CONTEXT_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<u32>().ok());

        let is_free = IS_FREE_RE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str() == "true")
            .unwrap_or(false);

        aa_models.push(AaModel {
            name,
            intelligence_index,
            coding_index,
            price_in,
            price_out,
            price_blended,
            openrouter_api_id,
            context_window,
            is_free,
        });
    }

    tracing::info!(
        count = aa_models.len(),
        "Extracted models from Artificial Analysis HTML"
    );

    // 3. Merge AA onto OpenRouter models
    for aa in aa_models {
        if let Some(ref or_id) = aa.openrouter_api_id {
            if let Some(existing) = merged.get_mut(or_id) {
                existing.intelligence_index = aa.intelligence_index;
                existing.coding_index = aa.coding_index;
                if aa.price_blended.is_some() {
                    existing.blended_price_per_million = aa.price_blended;
                }
                continue;
            }
        }

        let model_id = aa.openrouter_api_id.clone().unwrap_or_else(|| {
            format!(
                "{}/{}",
                detect_provider_from_id(&aa.name),
                aa.name.to_ascii_lowercase().replace(' ', "-")
            )
        });

        let price_in = aa.price_in.unwrap_or(0.0);
        let price_out = aa.price_out.unwrap_or(0.0);
        let provider = detect_provider_from_id(&model_id);

        merged.insert(
            model_id.clone(),
            ScrapedModel {
                id: model_id,
                name: aa.name,
                input_price_per_million: price_in,
                output_price_per_million: price_out,
                blended_price_per_million: aa
                    .price_blended
                    .or(Some(price_in * 0.7 + price_out * 0.3)),
                context_length: aa.context_window.unwrap_or(128000),
                intelligence_index: aa.intelligence_index,
                coding_index: aa.coding_index,
                is_free: aa.is_free,
                provider,
            },
        );
    }

    let result_db = ModelDb {
        models: merged.into_values().collect(),
    };

    tracing::info!(
        count = result_db.models.len(),
        "Model database merge completed successfully"
    );
    Ok(result_db)
}

pub fn check_and_save_scraped_models(data_dir: &Path, db: &ModelDb) -> Result<(), String> {
    let db_path = data_dir.join("models_data.json");
    let serialized = serde_json::to_string_pretty(db).map_err(|e| e.to_string())?;
    std::fs::write(&db_path, serialized).map_err(|e| e.to_string())?;
    tracing::info!(path = ?db_path, "Saved scraped models to database");
    Ok(())
}

pub fn should_update_scraped_models(data_dir: &Path) -> bool {
    let db_path = data_dir.join("models_data.json");
    let metadata = match std::fs::metadata(&db_path) {
        Ok(m) => m,
        Err(_) => return true,
    };

    let modified = match metadata.modified() {
        Ok(t) => t,
        Err(_) => return true,
    };

    let now = std::time::SystemTime::now();
    match now.duration_since(modified) {
        Ok(duration) => duration.as_secs() > 24 * 3600,
        Err(_) => true,
    }
}

pub fn find_similar_model_and_provider(
    state: &AppState,
    failed_model: &str,
) -> Option<(String, ProviderKind)> {
    let db = state.model_db.read();
    let (failed_intel, failed_price) = if let Some(m) = db.get(failed_model) {
        (
            m.intelligence_index.unwrap_or(45.0),
            m.input_price_per_million + m.output_price_per_million,
        )
    } else {
        // Safe heuristics if the failed model isn't in our DB
        let lower = failed_model.to_ascii_lowercase();
        if lower.contains("sonnet") {
            (53.0, 3.0 + 15.0)
        } else if lower.contains("opus") {
            (55.0, 15.0 + 75.0)
        } else if lower.contains("haiku") {
            (42.0, 0.8 + 4.0)
        } else if lower.contains("gpt-4o-mini") {
            (38.0, 0.15 + 0.6)
        } else if lower.contains("gpt-4o") {
            (50.0, 2.5 + 10.0)
        } else if lower.contains("gemini-2.0-flash")
            || lower.contains("gemini-2.5-flash")
            || lower.contains("gemini-1.5-flash")
        {
            (40.0, 0.0)
        } else if lower.contains("gemini-1.5-pro") || lower.contains("gemini-2.5-pro") {
            (47.0, 1.25 + 10.0)
        } else {
            (45.0, 5.0)
        }
    };

    let config = state.config.read();
    let mut enabled_providers = Vec::new();
    if config
        .providers
        .openai
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
    {
        enabled_providers.push(ProviderKind::OpenAI);
    }
    if config
        .providers
        .anthropic
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
    {
        enabled_providers.push(ProviderKind::Anthropic);
    }
    if config
        .providers
        .gemini
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
    {
        enabled_providers.push(ProviderKind::Gemini);
    }

    let mut enabled_custom_providers = Vec::new();
    for (name, entry) in &config.providers.custom {
        if entry.enabled {
            enabled_custom_providers.push(name.clone());
        }
    }

    let mut candidates = Vec::new();
    let failed_canon = canonical_model_id(failed_model);
    for m in db.all() {
        // Skip the model that just failed. Comparison is canonical so a
        // request for `"gpt-4o"` correctly excludes the DB entry
        // `"openai/gpt-4o"` — the previous exact-`String` == skip would
        // let it through and the failover could pick the failing model.
        if canonical_model_id(&m.id) == failed_canon {
            continue;
        }

        let p_kind = provider_kind_from_str(&m.provider);
        let mut is_supported = false;

        if p_kind == ProviderKind::Custom {
            for custom_name in &enabled_custom_providers {
                if let Some(entry) = config.providers.custom.get(custom_name) {
                    if entry.model_allowlist.is_empty()
                        || entry.model_allowlist.iter().any(|a| same_model(a, &m.id))
                    {
                        is_supported = true;
                        break;
                    }
                }
            }
        } else if enabled_providers.contains(&p_kind) {
            let allowlist = match p_kind {
                ProviderKind::OpenAI => {
                    config.providers.openai.as_ref().map(|p| &p.model_allowlist)
                }
                ProviderKind::Anthropic => config
                    .providers
                    .anthropic
                    .as_ref()
                    .map(|p| &p.model_allowlist),
                ProviderKind::Gemini => {
                    config.providers.gemini.as_ref().map(|p| &p.model_allowlist)
                }
                _ => None,
            };
            if let Some(list) = allowlist {
                if list.is_empty() || list.iter().any(|a| same_model(a, &m.id)) {
                    is_supported = true;
                }
            }
        }

        if is_supported {
            candidates.push(m.clone());
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // Filter candidates within 5.0 points of intelligence
    let mut filtered_candidates: Vec<&ScrapedModel> = candidates
        .iter()
        .filter(|c| (c.intelligence_index.unwrap_or(45.0) - failed_intel).abs() <= 5.0)
        .collect();

    if filtered_candidates.is_empty() {
        filtered_candidates = candidates.iter().collect();
    }

    // Sort by price difference ascending, then by intelligence difference ascending
    filtered_candidates.sort_by(|a, b| {
        let price_a = a.input_price_per_million + a.output_price_per_million;
        let price_b = b.input_price_per_million + b.output_price_per_million;
        let price_diff_a = (price_a - failed_price).abs();
        let price_diff_b = (price_b - failed_price).abs();

        price_diff_a
            .partial_cmp(&price_diff_b)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let intel_a = a.intelligence_index.unwrap_or(45.0);
                let intel_b = b.intelligence_index.unwrap_or(45.0);
                let intel_diff_a = (intel_a - failed_intel).abs();
                let intel_diff_b = (intel_b - failed_intel).abs();
                intel_diff_a
                    .partial_cmp(&intel_diff_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    filtered_candidates
        .first()
        .map(|m| (m.id.clone(), provider_kind_from_str(&m.provider)))
}

pub fn is_any_provider_configured(config: &AppConfig) -> bool {
    config
        .providers
        .openai
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
        || config
            .providers
            .anthropic
            .as_ref()
            .map(|p| p.enabled)
            .unwrap_or(false)
        || config
            .providers
            .gemini
            .as_ref()
            .map(|p| p.enabled)
            .unwrap_or(false)
        || config.providers.custom.values().any(|p| p.enabled)
}

pub fn filter_scraped_models(mut db: ModelDb, config: &AppConfig) -> ModelDb {
    let mut enabled_providers = HashSet::new();
    if config
        .providers
        .openai
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
    {
        enabled_providers.insert("openai");
    }
    if config
        .providers
        .anthropic
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
    {
        enabled_providers.insert("anthropic");
    }
    if config
        .providers
        .gemini
        .as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
    {
        enabled_providers.insert("gemini");
        enabled_providers.insert("google");
    }
    let has_custom = config.providers.custom.values().any(|p| p.enabled);

    db.models.retain(|m| {
        if has_custom {
            return true;
        }
        let lower_provider = m.provider.to_ascii_lowercase();
        enabled_providers
            .iter()
            .any(|&p| lower_provider.contains(p))
    });
    db
}

pub fn trigger_scraping_if_needed(state: &AppState, config: &AppConfig, data_dir: &Path) {
    // Privacy gate: scraping is off by default. The operator must
    // opt in via `[features] model_scraping = true` in the config
    // file or a PATCH /ui/settings that sets it. Without this gate a
    // local-first app would silently phone home to two third-party
    // domains the first time a provider is configured.
    if !config.features.model_scraping {
        tracing::debug!("Model scraping is disabled (features.model_scraping = false). Skipping.");
        return;
    }
    if !is_any_provider_configured(config) {
        tracing::debug!("No providers configured. Skipping background model scraping.");
        return;
    }

    if should_update_scraped_models(data_dir) {
        let state_clone = state.clone();
        let data_dir_clone = data_dir.to_path_buf();
        let config_clone = config.clone();
        tokio::spawn(async move {
            tracing::info!(
                "Starting background model scraping job because a provider is configured..."
            );
            match run_scraping_job().await {
                Ok(new_db) => {
                    let filtered_db = filter_scraped_models(new_db, &config_clone);
                    if let Err(e) = check_and_save_scraped_models(&data_dir_clone, &filtered_db) {
                        tracing::error!(error = %e, "Failed to save scraped models to file");
                    }
                    *state_clone.model_db.write() = filtered_db.clone();
                    let new_router = crate::state::build_smart_router(
                        &state_clone.pipeline,
                        &state_clone.config.read(),
                        (*state_clone.health).clone(),
                        &filtered_db,
                    );
                    state_clone.replace_router(new_router);
                    tracing::info!("SmartRouter successfully updated with freshly scraped and filtered model database.");
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to run background model scraping job");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autorouter_config::{AppConfig, ProviderEntry};

    #[test]
    fn test_canonical_model_id() {
        assert_eq!(canonical_model_id("openai/gpt-4o"), "gpt-4o");
        assert_eq!(canonical_model_id("gpt-4o"), "gpt-4o");
        assert_eq!(canonical_model_id("  openai/gpt-4o  "), "gpt-4o");
    }

    #[test]
    fn test_is_any_provider_configured() {
        let mut config = AppConfig::default();
        assert!(!is_any_provider_configured(&config));

        // Enable OpenAI
        config.providers.openai = Some(ProviderEntry {
            enabled: true,
            ..Default::default()
        });
        assert!(is_any_provider_configured(&config));

        // Disable OpenAI
        if let Some(ref mut p) = config.providers.openai {
            p.enabled = false;
        }
        assert!(!is_any_provider_configured(&config));
    }

    #[test]
    fn test_filter_scraped_models() {
        let mut db = ModelDb::new();
        db.models = vec![
            ScrapedModel {
                id: "openai/gpt-4o".to_string(),
                name: "GPT-4o".to_string(),
                input_price_per_million: 2.50,
                output_price_per_million: 10.00,
                blended_price_per_million: None,
                context_length: 128000,
                intelligence_index: None,
                coding_index: None,
                is_free: false,
                provider: "openai".to_string(),
            },
            ScrapedModel {
                id: "anthropic/claude-3-5-sonnet".to_string(),
                name: "Claude 3.5 Sonnet".to_string(),
                input_price_per_million: 3.00,
                output_price_per_million: 15.00,
                blended_price_per_million: None,
                context_length: 200000,
                intelligence_index: None,
                coding_index: None,
                is_free: false,
                provider: "anthropic".to_string(),
            },
        ];

        let mut config = AppConfig::default();
        config.providers.openai = Some(ProviderEntry {
            enabled: true,
            ..Default::default()
        });

        let filtered = filter_scraped_models(db.clone(), &config);
        assert_eq!(filtered.models.len(), 1);
        assert_eq!(filtered.models[0].id, "openai/gpt-4o");

        // Enable custom provider -> keep all
        config.providers.custom.insert(
            "my-custom".to_string(),
            ProviderEntry {
                enabled: true,
                ..Default::default()
            },
        );
        let filtered_custom = filter_scraped_models(db, &config);
        assert_eq!(filtered_custom.models.len(), 2);
    }
}
