//! Google Gemini `generateContent` adapter.
//!
//! Wire reference: <https://ai.google.dev/api/generate-content>.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::{json, Value};

use autorouter_core::{
    ContentPart, FinishReason, Message, MessageRole, ModelDescriptor, ProviderKind, StreamChunk,
    StreamEvent, ToolCall, ToolDefinition, UniversalRequest, UniversalResponse,
};

use crate::error::{TranslateError, TranslateResult};
use crate::reasoning_extractor::{split_reasoning, streamer_feed, streamer_finish, ReasoningSplit};
use crate::traits::{ProviderAdapter, UpstreamResponse};

// TODO(#audit3): Requires explicit cleanup (inline `remove` in the
// `finishReason` arm + `gemini_cleanup_drop` from upstream.rs) in every
// stream termination path. See reasoning_extractor.rs STREAMERS for the
// RAII-guard recommendation.
/// Per-stream flag set once we've emitted a `StreamEvent::Start` for
/// the request, so we don't emit it twice on a multi-chunk Gemini
/// stream. Mirrors the role of the OpenAI Responses
/// `response.created` event and the Anthropic `message_start` event.
fn gemini_starts_emitted() -> &'static Mutex<HashSet<usize>> {
    static MAP: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashSet::new()))
}

fn gemini_start_emitted(ptr: usize) -> bool {
    gemini_starts_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&ptr)
}

fn mark_gemini_start_emitted(ptr: usize) {
    gemini_starts_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ptr);
}

/// Extract a `StreamEvent::Start` from a Gemini streaming payload, if
/// the payload carries the metadata we need (responseId +
/// modelVersion). Returns `None` when the metadata is missing.
fn first_start_event(value: &Value) -> Option<StreamEvent> {
    let id = value
        .get("responseId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = value
        .get("modelVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if id.is_empty() && model.is_empty() {
        None
    } else {
        Some(StreamEvent::Start { id, model })
    }
}

/// Gemini generateContent adapter.
#[derive(Debug, Default, Clone)]
pub struct GeminiAdapter;

impl GeminiAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProviderAdapter for GeminiAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn display_name(&self) -> &'static str {
        "Google Gemini"
    }
    fn models(&self) -> Vec<ModelDescriptor> {
        vec![]
    }

    fn validate(&self, _request: &UniversalRequest) -> TranslateResult<()> {
        Ok(())
    }

    fn encode_request(&self, request: &UniversalRequest) -> TranslateResult<Value> {
        let mut system_parts: Vec<Value> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();
        for m in &request.messages {
            match m.role {
                MessageRole::System => {
                    system_parts.push(json!({ "text": m.text() }));
                }
                MessageRole::User => {
                    contents.push(json!({ "role": "user", "parts": encode_user_parts(m) }));
                }
                MessageRole::Assistant => {
                    contents.push(json!({ "role": "model", "parts": encode_assistant_parts(m) }));
                }
                MessageRole::Tool => {
                    let parts: Vec<Value> = m
                        .content
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::ToolResult {
                                tool_call_id,
                                content,
                                ..
                            } => {
                                let value = match content {
                                    autorouter_core::ToolResultPayload::Text { text } => {
                                        json!({ "text": text })
                                    }
                                    autorouter_core::ToolResultPayload::Json { value } => {
                                        value.clone()
                                    }
                                };
                                Some(json!({
                                    "functionResponse": {
                                        "name": tool_call_id,
                                        "response": value,
                                    }
                                }))
                            }
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        contents.push(json!({ "role": "user", "parts": parts }));
                    }
                }
            }
        }
        if contents.is_empty() {
            return Err(TranslateError::invalid_payload(
                "gemini",
                "request had no user or model contents",
            ));
        }
        let mut body = json!({ "contents": contents });
        if !system_parts.is_empty() {
            body["systemInstruction"] = json!({ "parts": system_parts });
        }
        let mut generation_config = serde_json::Map::new();
        if let Some(t) = request.temperature {
            generation_config.insert("temperature".into(), json!(t));
        }
        if let Some(p) = request.top_p {
            generation_config.insert("topP".into(), json!(p));
        }
        if let Some(m) = request.max_output_tokens {
            generation_config.insert("maxOutputTokens".into(), json!(m));
        }
        if !request.stop.is_empty() {
            generation_config.insert("stopSequences".into(), json!(request.stop));
        }
        if !generation_config.is_empty() {
            body["generationConfig"] = Value::Object(generation_config);
        }
        // Include the model id in the body. The Gemini wire protocol
        // normally carries the model in the URL path, but the upstream
        // `build_url` reads `body.model` to construct that path, and
        // Gemini-compatible servers (Google, OpenRouter) accept an extra
        // `model` field harmlessly. Without it the request fails with
        // "missing model field" / "model is required".
        if !request.model.is_empty() {
            body["model"] = json!(request.model);
        }
        if !request.tools.is_empty() {
            let declarations: Vec<Value> = request.tools.iter().map(encode_tool).collect();
            body["tools"] = json!([{ "functionDeclarations": declarations }]);
        }
        if let Some(obj) = request.extra.as_object() {
            if let Some(map) = body.as_object_mut() {
                for (k, v) in obj {
                    if !map.contains_key(k) {
                        map.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        Ok(body)
    }

    fn decode_response(
        &self,
        request: &UniversalRequest,
        body: &Value,
        status: u16,
    ) -> TranslateResult<UpstreamResponse> {
        let _ = request;
        let response = decode_gemini_response(body)?;
        Ok(UpstreamResponse {
            response,
            status,
            raw: body.clone(),
        })
    }

    fn decode_stream_chunk(
        &self,
        request: &UniversalRequest,
        chunk: &str,
    ) -> TranslateResult<Vec<StreamChunk>> {
        let request_ptr = request.stream_id as usize;
        let mut out = Vec::new();
        for line in chunk.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let payload = if let Some(stripped) = line.strip_prefix("data:") {
                stripped.trim()
            } else {
                line
            };
            if payload.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Gemini streams do not have a dedicated "start" event
            // like `response.created` (Responses) or `message_start`
            // (Anthropic). The first candidate's metadata carries
            // `responseId` and `modelVersion`, which we surface as
            // the universal `StreamEvent::Start` so the consumer can
            // log/track per-stream identity. We track emission per
            // stream to avoid emitting Start twice (some Gemini
            // streams interleave metadata across multiple chunks).
            let start = first_start_event(&value);
            if let Some(start_event) = start {
                if !gemini_start_emitted(request_ptr) {
                    mark_gemini_start_emitted(request_ptr);
                    out.push(StreamChunk::from(start_event));
                }
            }
            for event in gemini_value_to_events(&value, request_ptr) {
                out.push(StreamChunk::from(event));
            }
        }
        Ok(out)
    }
    fn encode_stream_chunk(&self, chunk: &StreamChunk) -> TranslateResult<Option<String>> {
        // B2: emit a single SSE frame per call. The OpenAI-compatible
        // wire format uses data: <json>\n\n for every frame and a
        // trailing data: [DONE]\n\n (emitted by the gateway after the
        // Finish event is seen).
        let mut out = String::new();
        for event in &chunk.events {
            out.push_str(&crate::streaming::encode_gemini_sse(event));
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}

fn encode_user_parts(message: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    for p in &message.content {
        match p {
            ContentPart::Text { text } => parts.push(json!({ "text": text })),
            ContentPart::Image { source, .. } => {
                let (media_type, data) = match source {
                    autorouter_core::ImageSource::Base64 { media_type, data } => {
                        (media_type.clone(), data.clone())
                    }
                    autorouter_core::ImageSource::Url { url } => {
                        tracing::warn!(url = %url, "image URL cannot be sent to Gemini via fileData; skipping");
                        continue;
                    }
                    autorouter_core::ImageSource::FileId { id } => {
                        tracing::warn!(id = %id, "image FileId cannot be sent to Gemini via fileData; skipping");
                        continue;
                    }
                };
                parts.push(json!({
                    "inlineData": { "mimeType": media_type, "data": data }
                }));
            }
            ContentPart::Audio {
                source: autorouter_core::ImageSource::Base64 { media_type, data },
            } => {
                parts.push(json!({
                    "inlineData": { "mimeType": media_type, "data": data }
                }));
            }
            ContentPart::Document {
                source: autorouter_core::ImageSource::Base64 { media_type, data },
                ..
            } => {
                parts.push(json!({
                    "inlineData": { "mimeType": media_type, "data": data }
                }));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

fn encode_assistant_parts(message: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    for p in &message.content {
        match p {
            ContentPart::Text { text } => parts.push(json!({ "text": text })),
            ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => parts.push(json!({
                "functionCall": { "name": name, "args": arguments, "id": id }
            })),
            ContentPart::ToolCallRaw {
                id,
                name,
                arguments_raw,
            } => parts.push(json!({
                "functionCall": { "name": name, "args": arguments_raw, "id": id }
            })),
            ContentPart::ToolUse { id, name, input } => parts.push(json!({
                "functionCall": { "name": name, "args": input, "id": id }
            })),
            ContentPart::Reasoning { .. } => {
                // Reasoning is response-only; skip silently when
                // encoding a request. The Gemini request wire
                // format does not accept thought parts in
                // `contents[]`.
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

fn encode_tool(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

fn gemini_value_to_events(value: &Value, request_ptr: usize) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    if let Some(candidates) = value.get("candidates").and_then(|v| v.as_array()) {
        for cand in candidates {
            if let Some(parts) = cand
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        // Gemini emits thinking / "thought" parts with
                        // `thought: true` alongside normal text parts
                        // (https://ai.google.dev/api/generate-content#thought).
                        // Route them through the universal reasoning
                        // delta so the SSE encoders can re-emit them
                        // correctly to the consumer wire format.
                        let thought = part
                            .get("thought")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if thought {
                            out.push(StreamEvent::ReasoningDelta {
                                text: text.to_string(),
                            });
                        } else if text.is_empty() {
                            // skip
                        } else {
                            // Always feed through the streamer because
                            // the carry buffer may already hold a
                            // partial opener from a previous part — only the
                            // streamer owns that state.
                            for split in streamer_feed(request_ptr, text) {
                                match split {
                                    ReasoningSplit::Text(t) => {
                                        if !t.is_empty() {
                                            out.push(StreamEvent::TextDelta { text: t });
                                        }
                                    }
                                    ReasoningSplit::Reasoning(t) => {
                                        if !t.is_empty() {
                                            out.push(StreamEvent::ReasoningDelta { text: t });
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fc) = part.get("functionCall") {
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
                        let arguments = fc.get("args").cloned().unwrap_or(Value::Null);
                        out.push(StreamEvent::ToolCallStart {
                            call: ToolCall {
                                id: id.clone(),
                                name,
                                arguments,
                            },
                        });
                        out.push(StreamEvent::ToolCallEnd { id });
                    }
                }
            }
            if let Some(reason) = cand.get("finishReason").and_then(|v| v.as_str()) {
                // Drain any pending reasoning state before the Finish event.
                for split in streamer_finish(request_ptr) {
                    match split {
                        ReasoningSplit::Text(t) => {
                            if !t.is_empty() {
                                out.push(StreamEvent::TextDelta { text: t });
                            }
                        }
                        ReasoningSplit::Reasoning(t) => {
                            if !t.is_empty() {
                                out.push(StreamEvent::ReasoningDelta { text: t });
                            }
                        }
                    }
                }
                let mapped = match reason {
                    "STOP" => FinishReason::Stop,
                    "MAX_TOKENS" => FinishReason::Length,
                    "SAFETY" => FinishReason::Safety,
                    "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
                        FinishReason::ContentFilter
                    }
                    _ => FinishReason::Other,
                };
                let usage = value.get("usageMetadata").and_then(decode_gemini_usage);
                // Clear the per-stream Start-emitted flag so the
                // map doesn't accumulate entries across streams.
                gemini_starts_emitted()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&request_ptr);
                out.push(StreamEvent::Finish {
                    reason: mapped,
                    usage,
                });
            }
        }
    }
    if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
        out.push(StreamEvent::TextDelta {
            text: text.to_string(),
        });
    }
    out
}

fn decode_gemini_response(body: &Value) -> TranslateResult<UniversalResponse> {
    let id = body
        .get("responseId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = body
        .get("modelVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut content: Vec<ContentPart> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(candidates) = body.get("candidates").and_then(|v| v.as_array()) {
        for cand in candidates {
            if let Some(parts) = cand
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        let thought = part
                            .get("thought")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if thought {
                            content.push(ContentPart::Reasoning {
                                text: text.to_string(),
                            });
                        } else if !text.is_empty() {
                            // Split inline reasoning tags out of text parts so
                            // they surface as Reasoning parts instead of Text.
                            for split in split_reasoning(text) {
                                match split {
                                    ReasoningSplit::Text(t) => {
                                        content.push(ContentPart::Text { text: t });
                                    }
                                    ReasoningSplit::Reasoning(t) => {
                                        content.push(ContentPart::Reasoning { text: t });
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fc) = part.get("functionCall") {
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
                        let arguments = fc.get("args").cloned().unwrap_or(Value::Null);
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        });
                        content.push(ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                }
            }
        }
    }
    let finish_reason = body
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("finishReason"))
        .and_then(|v| v.as_str())
        .map(|r| match r {
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            "SAFETY" => FinishReason::Safety,
            "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
                FinishReason::ContentFilter
            }
            _ => FinishReason::Other,
        })
        .unwrap_or(FinishReason::Stop);
    let usage = body
        .get("usageMetadata")
        .and_then(decode_gemini_usage)
        .unwrap_or_default();
    Ok(UniversalResponse {
        id,
        model,
        message: Message {
            role: MessageRole::Assistant,
            content,
            name: None,
        },
        tool_calls,
        finish_reason,
        usage,
        created_at: None,
    })
}

fn decode_gemini_usage(value: &Value) -> Option<autorouter_core::Usage> {
    let mut usage = autorouter_core::Usage::default();
    if let Some(prompt) = value.get("promptTokenCount").and_then(|v| v.as_u64()) {
        usage.tokens.input = Some(prompt);
    }
    if let Some(candidates) = value.get("candidatesTokenCount").and_then(|v| v.as_u64()) {
        usage.tokens.output = Some(candidates);
    }
    if let Some(thoughts) = value.get("thoughtsTokenCount").and_then(|v| v.as_u64()) {
        usage.tokens.reasoning = Some(thoughts);
    }
    if let Some(cached) = value
        .get("cachedContentTokenCount")
        .and_then(|v| v.as_u64())
    {
        usage.tokens.cache_read = Some(cached);
    }
    if usage.tokens.input.is_some() || usage.tokens.output.is_some() {
        Some(usage)
    } else {
        None
    }
}

/// Remove the per-request Start-emitted flag so the
/// `gemini_starts_emitted` set does not accumulate entries when a
/// stream terminates without a `finishReason` (connection drop,
/// error, or proxy truncation). Called from the server's upstream
/// cleanup alongside `streamer_drop` and `openai_tool_call_drop`.
pub fn gemini_cleanup_drop(request_ptr: usize) {
    gemini_starts_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&request_ptr);
}
