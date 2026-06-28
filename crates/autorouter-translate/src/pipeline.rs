//! Translation pipeline.
//!
//! The pipeline orchestrates the chain:
//!
//!   provider wire -> parser -> UniversalRequest -> transformer -> serializer -> target wire
//!
//! In Phase 1 the transformer is the identity function: the routing
//! engine is the only thing that decides the target provider, and the
//! transformer only normalises provider-specific quirks.

use std::sync::Arc;

use autorouter_core::{ProviderKind, RequestContext, UniversalRequest, UniversalResponse};

use crate::error::{TranslateError, TranslateResult};
use crate::traits::{ProviderAdapter, UpstreamResponse, UpstreamStream};

/// Direction of translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Source -> Universal (parse the wire format).
    In,
    /// Universal -> Source (serialise the wire format).
    Out,
}

/// Wire format variant for the OpenAI family. The Responses API uses a
/// different shape from Chat Completions (`input` / `instructions` vs
/// `messages`), so the pipeline must know which one to use. M2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAiWireFormat {
    /// `/openai/v1/chat/completions` -- default.
    #[default]
    ChatCompletions,
    /// `/openai/v1/responses`.
    Responses,
}

/// A pipeline of providers. The router keeps one of these around and
/// uses it to translate every request.
#[derive(Clone, Default)]
pub struct TranslationPipeline {
    adapters: Vec<Arc<dyn ProviderAdapter>>,
}

impl TranslationPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider adapter with the pipeline.
    pub fn register(mut self, adapter: Arc<dyn ProviderAdapter>) -> Self {
        self.adapters.push(adapter);
        self
    }

    /// Look up an adapter by provider kind. Returns
    /// [`TranslateError::UnsupportedTarget`] if no adapter is registered.
    pub fn adapter_for(&self, kind: ProviderKind) -> TranslateResult<Arc<dyn ProviderAdapter>> {
        self.adapters
            .iter()
            .find(|a| a.kind() == kind)
            .cloned()
            .ok_or_else(|| TranslateError::UnsupportedTarget(kind.to_string()))
    }

    /// Parse a provider request body into a [`UniversalRequest`].
    pub fn parse_request(
        &self,
        source: ProviderKind,
        body: &serde_json::Value,
    ) -> TranslateResult<UniversalRequest> {
        self.parse_request_with_format(source, OpenAiWireFormat::default(), body)
    }

    /// Same as [`parse_request`] but with an explicit OpenAI wire format
    /// hint. Use this from the gateway so the Chat Completions and
    /// Responses endpoints dispatch to the right decoder.
    pub fn parse_request_with_format(
        &self,
        source: ProviderKind,
        openai_format: OpenAiWireFormat,
        body: &serde_json::Value,
    ) -> TranslateResult<UniversalRequest> {
        // Custom is the pass-through case: it has no canonical wire
        // format and no registered adapter, so skip adapter lookup.
        if source == ProviderKind::Custom {
            return decode_custom(body);
        }
        let adapter = self.adapter_for(source)?;
        let request = decode_from_source(&*adapter, openai_format, body)?;
        adapter.validate(&request)?;
        Ok(request)
    }

    /// Serialise a [`UniversalRequest`] into the target provider's body.
    pub fn serialise_request(
        &self,
        target: ProviderKind,
        request: &UniversalRequest,
    ) -> TranslateResult<serde_json::Value> {
        let adapter = self.adapter_for(target)?;
        adapter.validate(request)?;
        adapter.encode_request(request)
    }

    /// Translate a non-streaming response back to the source provider.
    pub fn translate_response(
        &self,
        ctx: &RequestContext,
        upstream: UpstreamResponse,
    ) -> TranslateResult<UniversalResponse> {
        // The UpstreamResponse is already in universal form; the
        // re-shape to source-provider wire format happens in the
        // gateway (Phase 3). We use this hook to apply global rules:
        //   * attach a request id if missing,
        //   * log finish reason for observability.
        if upstream.response.id.is_empty() {
            return Ok(UniversalResponse {
                id: ctx.request_id.to_string(),
                ..upstream.response
            });
        }
        Ok(upstream.response)
    }

    /// Borrow the underlying stream of chunks. The pipeline performs
    /// any in-flight rewrites (rate-limit backoff, content filters)
    /// here in later phases.
    pub fn translate_stream(
        &self,
        _ctx: &RequestContext,
        stream: UpstreamStream,
    ) -> UpstreamStream {
        // Identity for Phase 1.
        stream
    }
}

/// Provider-specific decoders. Phase 1 ships one function per provider
/// here; Phase 3 will fold them into the adapter's decode method.
fn decode_from_source(
    adapter: &dyn ProviderAdapter,
    openai_format: OpenAiWireFormat,
    body: &serde_json::Value,
) -> TranslateResult<UniversalRequest> {
    match adapter.kind() {
        ProviderKind::OpenAI => match openai_format {
            OpenAiWireFormat::ChatCompletions => decode_openai_chat(body),
            OpenAiWireFormat::Responses => decode_openai_responses(body),
        },
        ProviderKind::Anthropic => decode_anthropic(body),
        ProviderKind::Gemini => decode_gemini(body),
        ProviderKind::Custom => decode_custom(body),
    }
}

fn decode_openai_chat(body: &serde_json::Value) -> TranslateResult<UniversalRequest> {
    // Reuse the chat adapter's encoding by inverting the same shape
    // through the public adapter API. This is intentionally a thin
    // wrapper: Phase 1 callers may go through [`OpenAiChatAdapter`]
    // directly when they have a typed adapter handle.
    use autorouter_core::{ContentPart, Message, MessageRole, ToolDefinition};
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TranslateError::invalid_payload("openai_chat", "model is required"))?
        .to_string();
    let mut messages: Vec<Message> = Vec::new();
    if let Some(arr) = body.get("messages").and_then(|v| v.as_array()) {
        for raw in arr {
            let role = raw.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let role = match role {
                "system" | "developer" => MessageRole::System,
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "tool" => MessageRole::Tool,
                _ => continue,
            };
            let mut content: Vec<ContentPart> = Vec::new();
            if let Some(text) = raw.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    content.push(ContentPart::Text {
                        text: text.to_string(),
                    });
                }
            } else if let Some(parts) = raw.get("content").and_then(|v| v.as_array()) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        content.push(ContentPart::Text {
                            text: text.to_string(),
                        });
                    } else if let Some(image) = part.get("image_url") {
                        if let Some(url) = image.get("url").and_then(|v| v.as_str()) {
                            content.push(ContentPart::Image {
                                source: autorouter_core::ImageSource::Url {
                                    url: url.to_string(),
                                },
                                detail: None,
                            });
                        }
                    }
                }
            }
            if let Some(tcs) = raw.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
                            } else {
                                v.clone()
                            }
                        })
                        .unwrap_or(serde_json::Value::Null);
                    content.push(ContentPart::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
            messages.push(Message {
                role,
                content,
                name: None,
            });
        }
    }
    let mut tools: Vec<ToolDefinition> = Vec::new();
    if let Some(arr) = body.get("tools").and_then(|v| v.as_array()) {
        for t in arr {
            if let Some(func) = t.get("function") {
                let name = func
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = func
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let parameters = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::json!({ "type": "object", "properties": {} }));
                let strict = func
                    .get("strict")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                tools.push(ToolDefinition {
                    name,
                    description,
                    parameters,
                    strict,
                });
            }
        }
    }
    Ok(UniversalRequest {
        model,
        messages,
        system: None,
        tool_choice: None,
        metadata: serde_json::Value::Null,
        tools,
        temperature: body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32),
        top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|f| f as f32),
        max_output_tokens: body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|n| n as u32),
        stop: body
            .get("stop")
            .map(|v| match v {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default(),
        stream: body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        extra: body
            .get("extra")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        user: body.get("user").and_then(|v| v.as_str()).map(String::from),
        prior_usage: Default::default(),
        ..Default::default()
    })
}

/// Decode an OpenAI Responses request body into a UniversalRequest.
/// M2: `/openai/v1/responses` uses a different wire format than Chat
/// Completions. The Responses body has `input` (string or array of
/// items) and `instructions`, while Chat Completions uses `messages`.
fn decode_openai_responses(body: &serde_json::Value) -> TranslateResult<UniversalRequest> {
    use autorouter_core::{ContentPart, Message, MessageRole, ToolDefinition};
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TranslateError::invalid_payload("openai_responses", "model is required"))?
        .to_string();
    let mut messages: Vec<Message> = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(Message {
                role: MessageRole::System,
                content: vec![ContentPart::Text {
                    text: instructions.to_string(),
                }],
                name: None,
            });
        }
    }
    if let Some(input) = body.get("input") {
        if let Some(s) = input.as_str() {
            if !s.is_empty() {
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: s.to_string(),
                    }],
                    name: None,
                });
            }
        } else if let Some(arr) = input.as_array() {
            for raw in arr {
                let role = raw.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let role = match role {
                    "system" | "developer" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    _ => continue,
                };
                let mut content: Vec<ContentPart> = Vec::new();
                if let Some(text) = raw.get("content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        content.push(ContentPart::Text {
                            text: text.to_string(),
                        });
                    }
                } else if let Some(parts) = raw.get("content").and_then(|v| v.as_array()) {
                    for part in parts {
                        let pt = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if pt == "input_text" || pt == "output_text" || pt == "text" {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                content.push(ContentPart::Text {
                                    text: text.to_string(),
                                });
                            }
                        } else if pt == "input_image" {
                            if let Some(url) = part.get("image_url").and_then(|v| v.as_str()) {
                                content.push(ContentPart::Image {
                                    source: autorouter_core::ImageSource::Url {
                                        url: url.to_string(),
                                    },
                                    detail: None,
                                });
                            }
                        }
                    }
                }
                if let Some(tcs) = raw.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .map(|v| {
                                if let Some(s) = v.as_str() {
                                    serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
                                } else {
                                    v.clone()
                                }
                            })
                            .unwrap_or(serde_json::Value::Null);
                        content.push(ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                }
                messages.push(Message {
                    role,
                    content,
                    name: None,
                });
            }
        }
    }
    let mut tools: Vec<ToolDefinition> = Vec::new();
    if let Some(arr) = body.get("tools").and_then(|v| v.as_array()) {
        for t in arr {
            // Responses API uses flat tools: { "type": "function", "name", "description", "parameters" }
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let parameters = t
                .get("parameters")
                .cloned()
                .unwrap_or(serde_json::json!({ "type": "object", "properties": {} }));
            let strict = t.get("strict").and_then(|v| v.as_bool()).unwrap_or(false);
            tools.push(ToolDefinition {
                name,
                description,
                parameters,
                strict,
            });
        }
    }
    let temperature = body
        .get("temperature")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let top_p = body.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32);
    let max_output_tokens = body
        .get("max_output_tokens")
        .or_else(|| body.get("max_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    Ok(UniversalRequest {
        model,
        system: None,
        messages,
        tool_choice: None,
        metadata: body
            .get("metadata")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        tools,
        temperature,
        top_p,
        max_output_tokens,
        stop: body
            .get("stop")
            .map(|v| match v {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default(),
        stream: body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        extra: body
            .get("extra")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        user: body.get("user").and_then(|v| v.as_str()).map(String::from),
        prior_usage: Default::default(),
        ..Default::default()
    })
}

fn decode_anthropic(body: &serde_json::Value) -> TranslateResult<UniversalRequest> {
    use autorouter_core::{ContentPart, Message, MessageRole};
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TranslateError::invalid_payload("anthropic", "model is required"))?
        .to_string();
    let mut messages: Vec<Message> = Vec::new();
    // Anthropic's `system` parameter can be a plain string OR an
    // array of content blocks. The block form lets clients pin
    // `cache_control` metadata to a specific system segment, so
    // dropping it silently would also drop the cache hint and force
    // the upstream provider to re-tokenise on every turn. Concatenate
    // the text portions of every block into one System message so the
    // universal representation stays simple; cache_control metadata
    // is captured in `Message.extra` via the per-part Unknown pass
    // below.
    if let Some(system) = body.get("system") {
        if let Some(text) = system.as_str() {
            if !text.is_empty() {
                messages.push(Message::system(text));
            }
        } else if let Some(blocks) = system.as_array() {
            let mut parts: Vec<ContentPart> = Vec::new();
            for block in blocks {
                match block.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        // A text block may carry `cache_control`
                        // (Anthropic prompt-caching pin) or other
                        // provider-specific metadata. Extracting only
                        // the text would silently strip the cache
                        // hint. Preserve the raw block in an Unknown
                        // part so the encoder can re-emit the
                        // metadata alongside the text.
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                parts.push(ContentPart::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                        if block.get("cache_control").is_some() || block.get("citations").is_some()
                        {
                            parts.push(ContentPart::Unknown {
                                provider: "anthropic".to_string(),
                                raw: block.clone(),
                            });
                        }
                    }
                    // Anything else (image / tool_use / tool_result /
                    // unknown) is preserved in an Unknown part so the
                    // upstream serializer can decide what to do. This
                    // is strictly better than dropping the block.
                    _ => {
                        parts.push(ContentPart::Unknown {
                            provider: "anthropic".to_string(),
                            raw: block.clone(),
                        });
                    }
                }
            }
            if !parts.is_empty() {
                messages.push(Message {
                    role: MessageRole::System,
                    content: parts,
                    name: None,
                });
            }
        }
    }
    if let Some(arr) = body.get("messages").and_then(|v| v.as_array()) {
        for raw in arr {
            let role = raw.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let role = match role {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                _ => continue,
            };
            let mut content: Vec<ContentPart> = Vec::new();
            if let Some(text) = raw.get("content").and_then(|v| v.as_str()) {
                content.push(ContentPart::Text {
                    text: text.to_string(),
                });
            } else if let Some(blocks) = raw.get("content").and_then(|v| v.as_array()) {
                for block in blocks {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            // A text block may carry `cache_control`
                            // (Anthropic prompt-caching pin). Capture
                            // the text AND preserve the raw block in
                            // an Unknown part so the encoder can
                            // re-emit the cache hint on the upstream
                            // side.
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    content.push(ContentPart::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            if block.get("cache_control").is_some() {
                                content.push(ContentPart::Unknown {
                                    provider: "anthropic".to_string(),
                                    raw: block.clone(),
                                });
                            }
                        }
                        Some("image") => {
                            if let Some(source) = block.get("source") {
                                if let (Some(media_type), Some(data)) = (
                                    source.get("media_type").and_then(|v| v.as_str()),
                                    source.get("data").and_then(|v| v.as_str()),
                                ) {
                                    content.push(ContentPart::Image {
                                        source: autorouter_core::ImageSource::Base64 {
                                            media_type: media_type.to_string(),
                                            data: data.to_string(),
                                        },
                                        detail: None,
                                    });
                                }
                            }
                        }
                        Some("tool_use") => {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = block
                                .get("input")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            content.push(ContentPart::ToolCall {
                                id,
                                name,
                                arguments,
                            });
                        }
                        Some("tool_result") => {
                            let tool_call_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let payload = match block.get("content") {
                                Some(serde_json::Value::String(s)) => {
                                    autorouter_core::ToolResultPayload::Text { text: s.clone() }
                                }
                                Some(other) => autorouter_core::ToolResultPayload::Json {
                                    value: other.clone(),
                                },
                                None => autorouter_core::ToolResultPayload::Text {
                                    text: String::new(),
                                },
                            };
                            content.push(ContentPart::ToolResult {
                                tool_call_id,
                                content: payload,
                                is_error,
                            });
                        }
                        _ => {}
                    }
                }
            }
            messages.push(Message {
                role,
                content,
                name: None,
            });
        }
    }
    let mut tools = Vec::new();
    if let Some(arr) = body.get("tools").and_then(|v| v.as_array()) {
        for t in arr {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let parameters = t
                .get("input_schema")
                .cloned()
                .unwrap_or(serde_json::json!({ "type": "object", "properties": {} }));
            tools.push(autorouter_core::ToolDefinition {
                name,
                description,
                parameters,
                strict: false,
            });
        }
    }
    Ok(UniversalRequest {
        model,
        messages,
        system: None,
        tool_choice: None,
        metadata: serde_json::Value::Null,
        tools,
        temperature: body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32),
        top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|f| f as f32),
        max_output_tokens: body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|n| n as u32),
        stop: body
            .get("stop_sequences")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        stream: body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        extra: Default::default(),
        user: None,
        prior_usage: Default::default(),
        ..Default::default()
    })
}

fn decode_gemini(body: &serde_json::Value) -> TranslateResult<UniversalRequest> {
    use autorouter_core::{ContentPart, Message, MessageRole};
    let mut messages: Vec<Message> = Vec::new();
    if let Some(system) = body
        .pointer("/systemInstruction/parts/0/text")
        .and_then(|v| v.as_str())
    {
        messages.push(Message::system(system));
    }
    if let Some(arr) = body.get("contents").and_then(|v| v.as_array()) {
        for raw in arr {
            // The Gemini protocol defaults `role` to "user" when it
            // is absent from a `contents` entry. The previous
            // implementation read `role` as `""` and the match
            // arm `_ => continue` silently dropped the message,
            // so a Gemini request like
            //   {"contents":[{"parts":[{"text":"hi"}]}]}
            // produced a UniversalRequest with zero messages and
            // the upstream would later return 400 ("Input
            // required"). Treat the empty role as the documented
            // default of `user`.
            let role = match raw.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
                "user" => MessageRole::User,
                "model" => MessageRole::Assistant,
                _ => continue,
            };
            let mut content: Vec<ContentPart> = Vec::new();
            if let Some(parts) = raw.get("parts").and_then(|v| v.as_array()) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        content.push(ContentPart::Text {
                            text: text.to_string(),
                        });
                    } else if let Some(inline) = part.get("inlineData") {
                        let media_type = inline
                            .get("mimeType")
                            .and_then(|v| v.as_str())
                            .unwrap_or("application/octet-stream")
                            .to_string();
                        let data = inline
                            .get("data")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        content.push(ContentPart::Image {
                            source: autorouter_core::ImageSource::Base64 { media_type, data },
                            detail: None,
                        });
                    } else if let Some(fc) = part.get("functionCall") {
                        let id = fc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = fc
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = fc.get("args").cloned().unwrap_or(serde_json::Value::Null);
                        content.push(ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                        });
                    } else if let Some(fr) = part.get("functionResponse") {
                        let tool_call_id = fr
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let payload = fr
                            .get("response")
                            .cloned()
                            .map(|v| autorouter_core::ToolResultPayload::Json { value: v })
                            .unwrap_or(autorouter_core::ToolResultPayload::Text {
                                text: String::new(),
                            });
                        content.push(ContentPart::ToolResult {
                            tool_call_id,
                            content: payload,
                            is_error: false,
                        });
                    }
                }
            }
            messages.push(Message {
                role,
                content,
                name: None,
            });
        }
    }
    let mut tools = Vec::new();
    if let Some(arr) = body
        .pointer("/tools/0/functionDeclarations")
        .and_then(|v| v.as_array())
    {
        for t in arr {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let parameters = t
                .get("parameters")
                .cloned()
                .unwrap_or(serde_json::json!({ "type": "object", "properties": {} }));
            tools.push(autorouter_core::ToolDefinition {
                name,
                description,
                parameters,
                strict: false,
            });
        }
    }
    let generation_config = body.get("generationConfig");
    let max_tokens = generation_config
        .and_then(|g| g.get("maxOutputTokens"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let temperature = generation_config
        .and_then(|g| g.get("temperature"))
        .and_then(|v| v.as_f64())
        .map(|f| f as f32);
    let top_p = generation_config
        .and_then(|g| g.get("topP"))
        .and_then(|v| v.as_f64())
        .map(|f| f as f32);
    let stop = generation_config
        .and_then(|g| g.get("stopSequences"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // The model id is embedded in the URL, not the body. Phase 1 keeps
    // an empty placeholder; the gateway fills it in from the path.
    Ok(UniversalRequest {
        model: String::new(),
        system: None,
        messages,
        tool_choice: None,
        metadata: serde_json::Value::Null,
        tools,
        temperature,
        top_p,
        max_output_tokens: max_tokens,
        stop,
        stream: false,
        extra: Default::default(),
        user: None,
        prior_usage: Default::default(),
        ..Default::default()
    })
}

/// Decode a mock upstream response into a [`UniversalResponse`].
///
/// Mock upstream bodies use the OpenAI Chat Completions shape; the
/// gateway normalises everything through this helper for tests.
pub fn decode_mock_response(
    kind: &autorouter_core::ProviderKind,
    body: &serde_json::Value,
) -> TranslateResult<autorouter_core::UniversalResponse> {
    match kind {
        autorouter_core::ProviderKind::OpenAI => {
            // Use the chat adapter to decode the body. The response
            // shape is the canonical OpenAI one for mocks.
            let adapter = crate::openai_chat::OpenAiChatAdapter::new();
            let request = autorouter_core::UniversalRequest {
                model: body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ..UniversalRequest::default()
            };
            adapter
                .decode_response(&request, body, 200)
                .map(|u| u.response)
        }
        autorouter_core::ProviderKind::Anthropic => {
            let adapter = crate::anthropic::AnthropicAdapter::new();
            let request = autorouter_core::UniversalRequest {
                model: body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ..UniversalRequest::default()
            };
            adapter
                .decode_response(&request, body, 200)
                .map(|u| u.response)
        }
        autorouter_core::ProviderKind::Gemini => {
            let adapter = crate::gemini::GeminiAdapter::new();
            let request = autorouter_core::UniversalRequest {
                model: body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ..UniversalRequest::default()
            };
            adapter
                .decode_response(&request, body, 200)
                .map(|u| u.response)
        }
        autorouter_core::ProviderKind::Custom => decode_custom_response(body),
    }
}

/// Pass-through decoder for the [ProviderKind::Custom] case. The body
/// is expected to follow a generic shape similar to OpenAI Chat
/// Completions: a JSON object with model, messages, and optional
///temperature, top_p, max_tokens, stop, stream, and tools.
/// Unknown fields are preserved in [UniversalRequest::extra] so
/// downstream serializers can re-emit them.
fn decode_custom(body: &serde_json::Value) -> TranslateResult<UniversalRequest> {
    use autorouter_core::{ContentPart, Message, MessageRole, ToolDefinition};
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    let mut messages: Vec<Message> = Vec::new();
    if let Some(arr) = body.get("messages").and_then(|v| v.as_array()) {
        for raw in arr {
            let role = raw.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let role = match role {
                "system" | "developer" => MessageRole::System,
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "tool" => MessageRole::Tool,
                _ => MessageRole::User,
            };
            let mut content: Vec<ContentPart> = Vec::new();
            if let Some(text) = raw.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    content.push(ContentPart::Text {
                        text: text.to_string(),
                    });
                }
            } else if let Some(parts) = raw.get("content").and_then(|v| v.as_array()) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        content.push(ContentPart::Text {
                            text: text.to_string(),
                        });
                    }
                }
            }
            messages.push(Message {
                role,
                content,
                name: None,
            });
        }
    }
    let mut tools: Vec<ToolDefinition> = Vec::new();
    if let Some(arr) = body.get("tools").and_then(|v| v.as_array()) {
        for t in arr {
            let name = t
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .or_else(|| t.get("name").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let description = t
                .get("function")
                .and_then(|f| f.get("description"))
                .and_then(|v| v.as_str())
                .or_else(|| t.get("description").and_then(|v| v.as_str()))
                .map(String::from);
            let parameters = t
                .get("function")
                .and_then(|f| f.get("parameters"))
                .cloned()
                .or_else(|| t.get("parameters").cloned())
                .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
            tools.push(ToolDefinition {
                name,
                description,
                parameters,
                strict: false,
            });
        }
    }
    let known_keys = vec![
        "model",
        "messages",
        "tools",
        "temperature",
        "top_p",
        "max_tokens",
        "max_completion_tokens",
        "stop",
        "stream",
    ];
    let mut extra = serde_json::Map::new();
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            if !known_keys.contains(&k.as_str()) {
                extra.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(UniversalRequest {
        model,
        messages,
        system: None,
        tool_choice: None,
        metadata: serde_json::Value::Null,
        tools,
        temperature: body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32),
        top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|f| f as f32),
        max_output_tokens: body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|n| n as u32),
        stop: body
            .get("stop")
            .map(|v| match v {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default(),
        stream: body
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        extra: if extra.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(extra)
        },
        user: body.get("user").and_then(|v| v.as_str()).map(String::from),
        prior_usage: Default::default(),
        ..Default::default()
    })
}

/// Pass-through decoder for a [ProviderKind::Custom] response body.
/// Tries common shapes (OpenAI Chat, Anthropic Messages, raw content
/// list) and falls back to wrapping the entire body as a text delta
/// when the structure is unrecognised. This keeps custom providers
/// usable as best-effort pass-throughs in the gateway.
fn decode_custom_response(body: &serde_json::Value) -> TranslateResult<UniversalResponse> {
    use autorouter_core::{ContentPart, FinishReason, Message, MessageRole, Usage};
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut text = String::new();
    let mut finish_reason = FinishReason::Stop;
    if let Some(content) = body.get("content").and_then(|v| v.as_str()) {
        text.push_str(content);
    } else if let Some(arr) = body.get("content").and_then(|v| v.as_array()) {
        for part in arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                text.push_str(t);
            }
        }
    } else if let Some(arr) = body.get("choices").and_then(|v| v.as_array()) {
        for choice in arr {
            if let Some(t) = choice
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_str())
            {
                text.push_str(t);
            }
            if let Some(r) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                finish_reason = match r {
                    "length" | "max_tokens" => FinishReason::Length,
                    "tool_calls" | "tool_use" => FinishReason::ToolCalls,
                    "content_filter" | "safety" => FinishReason::ContentFilter,
                    _ => FinishReason::Stop,
                };
            }
        }
    } else if let Some(s) = body.as_str() {
        text.push_str(s);
    } else {
        text.push_str(&body.to_string());
    }
    let usage = body
        .get("usage")
        .map(|u| Usage {
            tokens: autorouter_core::TokenBreakdown {
                input: u
                    .get("prompt_tokens")
                    .or_else(|| u.get("input_tokens"))
                    .and_then(|v| v.as_u64()),
                output: u
                    .get("completion_tokens")
                    .or_else(|| u.get("output_tokens"))
                    .and_then(|v| v.as_u64()),
                ..Default::default()
            },
            cost_micro_cents: None,
        })
        .unwrap_or_default();
    Ok(UniversalResponse {
        id,
        model,
        message: Message {
            role: MessageRole::Assistant,
            content: if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentPart::Text { text }]
            },
            name: None,
        },
        tool_calls: Vec::new(),
        finish_reason,
        usage,
        created_at: None,
    })
}
