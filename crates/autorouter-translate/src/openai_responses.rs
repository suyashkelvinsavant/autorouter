//! OpenAI Responses API adapter.
//!
//! Wire reference: <https://platform.openai.com/docs/api-reference/responses>.
//!
//! The Responses API is the successor to Chat Completions. AutoRouter
//! keeps a separate adapter for it because the request shape (`input`
//! items) differs from chat messages, reasoning items are first-class
//! citizens, and tool call ids must be preserved round-trip.

use async_trait::async_trait;
use serde_json::{json, Value};

use autorouter_core::{
    ContentPart, FinishReason, Message, MessageRole, ModelDescriptor, ProviderKind, StreamChunk,
    StreamEvent, ToolCall, UniversalRequest, UniversalResponse,
};

use crate::error::TranslateResult;
use crate::openai_chat::{push_split, OpenAiChatAdapter};
use crate::reasoning_extractor::{split_reasoning, streamer_feed, streamer_finish, ReasoningSplit};
use crate::traits::{ProviderAdapter, UpstreamResponse};

/// OpenAI Responses adapter.
#[derive(Debug, Default, Clone)]
pub struct OpenAiResponsesAdapter;

impl OpenAiResponsesAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiResponsesAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn display_name(&self) -> &'static str {
        "OpenAI Responses"
    }

    fn models(&self) -> Vec<ModelDescriptor> {
        OpenAiChatAdapter::new().models()
    }

    fn validate(&self, _request: &UniversalRequest) -> TranslateResult<()> {
        Ok(())
    }

    fn encode_request(&self, request: &UniversalRequest) -> TranslateResult<Value> {
        let mut input: Vec<Value> = Vec::new();
        let mut instructions: Vec<String> = Vec::new();
        for m in &request.messages {
            match m.role {
                MessageRole::System => instructions.push(m.text()),
                MessageRole::User => {
                    input.push(json!({
                        "role": "user",
                        "content": encode_user_content(m),
                    }));
                }
                MessageRole::Assistant => {
                    for part in &m.content {
                        match part {
                            ContentPart::Text { text } => input.push(json!({
                                "role": "assistant",
                                "content": [{ "type": "output_text", "text": text }],
                            })),
                            ContentPart::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                input.push(json!({
                                    "type": "function_call",
                                    "id": id,
                                    "name": name,
                                    "arguments": serde_json::to_string(arguments).unwrap_or_default(),
                                }));
                            }
                            ContentPart::ToolCallRaw {
                                id,
                                name,
                                arguments_raw,
                            } => {
                                input.push(json!({
                                    "type": "function_call",
                                    "id": id,
                                    "name": name,
                                    "arguments": arguments_raw,
                                }));
                            }
                            ContentPart::Reasoning { .. } => {
                                // Reasoning is response-only content;
                                // skip silently when encoding a request.
                                // The Responses wire format accepts
                                // `reasoning` items in `input[]` but we
                                // don't have a use case for round-tripping
                                // prior reasoning into an outgoing request
                                // yet.
                            }
                            _ => {}
                        }
                    }
                }
                MessageRole::Tool => {
                    for part in &m.content {
                        if let ContentPart::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                        } = part
                        {
                            let output = match content {
                                autorouter_core::ToolResultPayload::Text { text } => text.clone(),
                                autorouter_core::ToolResultPayload::Json { value } => {
                                    serde_json::to_string(value).unwrap_or_default()
                                }
                            };
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_call_id,
                                "output": output,
                                "status": if *is_error { "failed" } else { "completed" },
                            }));
                        }
                    }
                }
            }
        }
        let mut body = json!({
            "model": request.model,
            "input": input,
        });
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions.join("\n"));
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                        "strict": t.strict,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = request.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(m) = request.max_output_tokens {
            body["max_output_tokens"] = json!(m);
        }
        if !request.stop.is_empty() {
            body["stop"] = json!(request.stop);
        }
        if request.stream {
            body["stream"] = json!(true);
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
        let response = decode_responses_response(body)?;
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
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                // Drain any pending reasoning state before signaling finish.
                for split in streamer_finish(request_ptr) {
                    push_split(&mut out, split);
                }
                out.push(StreamChunk::from(StreamEvent::Finish {
                    reason: FinishReason::Stop,
                    usage: None,
                }));
                continue;
            }
            let value: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match event_type {
                "response.created" => {
                    let id = value
                        .pointer("/response/id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let model = value
                        .pointer("/response/model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push(StreamChunk::from(StreamEvent::Start { id, model }));
                }
                "response.output_text.delta" => {
                    if let Some(text) = value.get("delta").and_then(|v| v.as_str()) {
                        if text.is_empty() {
                            continue;
                        }
                        // ALWAYS feed through the streamer so partial openers
                        // from previous chunks are correctly recognized.
                        for split in streamer_feed(request_ptr, text) {
                            push_split(&mut out, split);
                        }
                    }
                }
                "response.reasoning_summary_text.delta" => {
                    if let Some(text) = value.get("delta").and_then(|v| v.as_str()) {
                        out.push(StreamChunk::from(StreamEvent::ReasoningDelta {
                            text: text.to_string(),
                        }));
                    }
                }
                "response.reasoning_text.delta" => {
                    if let Some(text) = value.get("delta").and_then(|v| v.as_str()) {
                        out.push(StreamChunk::from(StreamEvent::ReasoningDelta {
                            text: text.to_string(),
                        }));
                    }
                }
                "response.output_item.added" => {
                    if let Some(item) = value.get("item") {
                        if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                            let id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // The OpenAI Responses wire format is
                            // ambiguous about `arguments`: it can be
                            // either a JSON string OR a pre-parsed
                            // JSON object (some upstream versions
                            // emit the object form). The previous
                            // implementation called `v.as_str()` and
                            // silently dropped the data when the
                            // upstream sent a JSON object.
                            let arguments =
                                item.get("arguments")
                                    .map(|v| match v {
                                        // Pre-parsed object: pass it through.
                                        Value::Object(_) | Value::Array(_) => v.clone(),
                                        // JSON string: parse it.
                                        Value::String(s) => serde_json::from_str(s)
                                            .unwrap_or(Value::String(s.clone())),
                                        // Anything else (number, bool,
                                        // null): pass it through as-is
                                        // rather than silently dropping.
                                        other => other.clone(),
                                    })
                                    .unwrap_or(Value::Null);
                            out.push(StreamChunk::from(StreamEvent::ToolCallStart {
                                call: ToolCall {
                                    id,
                                    name,
                                    arguments,
                                },
                            }));
                        }
                    }
                }
                "response.function_call_arguments.delta" => {
                    let id = value
                        .get("item_id")
                        .or_else(|| value.get("call_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                        out.push(StreamChunk::from(StreamEvent::ToolCallDelta {
                            id,
                            arguments_fragment: delta.to_string(),
                        }));
                    }
                }
                "response.output_item.done" => {
                    if let Some(item) = value.get("item") {
                        if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                            let id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            out.push(StreamChunk::from(StreamEvent::ToolCallEnd { id }));
                        }
                    }
                }
                "response.completed" => {
                    // Drain any pending reasoning state before the Finish event.
                    for split in streamer_finish(request_ptr) {
                        push_split(&mut out, split);
                    }
                    let usage = value
                        .pointer("/response/usage")
                        .and_then(decode_responses_usage);
                    out.push(StreamChunk::from(StreamEvent::Finish {
                        reason: FinishReason::Stop,
                        usage,
                    }));
                }
                "response.failed" | "response.incomplete" => {
                    // Drain any pending reasoning state so the per-request
                    // streamer entry is cleaned up. Without this call, every
                    // failed or incomplete Responses stream would leak a
                    // ReasoningStreamer (up to 64 KiB carry) until process
                    // restart.
                    for split in streamer_finish(request_ptr) {
                        push_split(&mut out, split);
                    }
                    let message = value
                        .pointer("/response/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("responses stream error")
                        .to_string();
                    let code = value
                        .pointer("/response/error/code")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    out.push(StreamChunk::from(StreamEvent::Error { message, code }));
                }
                _ => {}
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
            out.push_str(&crate::streaming::encode_openai_responses_sse(event));
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}

fn encode_user_content(message: &Message) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    for p in &message.content {
        match p {
            ContentPart::Text { text } => parts.push(json!({ "type": "input_text", "text": text })),
            ContentPart::Image { source, .. } => {
                let url = match source {
                    autorouter_core::ImageSource::Url { url } => url.clone(),
                    autorouter_core::ImageSource::Base64 { media_type, data } => {
                        format!("data:{};base64,{}", media_type, data)
                    }
                    autorouter_core::ImageSource::FileId { id } => {
                        format!("file://{}", id)
                    }
                };
                parts.push(json!({ "type": "input_image", "image_url": url }));
            }
            ContentPart::Audio {
                source: autorouter_core::ImageSource::Base64 { media_type, data },
            } => {
                parts.push(json!({
                    "type": "input_audio",
                    "input_audio": { "data": data, "format": media_type }
                }));
            }
            ContentPart::Document { source, .. } => {
                // Responses API has no native document part; inline as a data URL.
                if let autorouter_core::ImageSource::Base64 { media_type, data } = source {
                    parts.push(json!({
                        "type": "input_image",
                        "image_url": { "url": format!("data:{};base64,{}", media_type, data) }
                    }));
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "type": "input_text", "text": "" }));
    }
    parts
}

fn decode_responses_response(body: &Value) -> TranslateResult<UniversalResponse> {
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut content: Vec<ContentPart> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(output) = body.get("output").and_then(|v| v.as_array()) {
        for item in output {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                        for part in parts {
                            if part.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    // Split inline reasoning tags out of the
                                    // output_text so they surface as Reasoning
                                    // parts instead of Text.
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
                        }
                    }
                }
                "function_call" => {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                serde_json::from_str(s).unwrap_or(Value::Null)
                            } else {
                                v.clone()
                            }
                        })
                        .unwrap_or(Value::Null);
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
                "reasoning" => {
                    // Per OpenAI Responses API, a reasoning item carries
                    // either `summary` (array of `{type:"summary_text", text}`)
                    // or `content` (array of `{type:"reasoning_text", text}`).
                    // Join all text fragments with a single newline so the
                    // downstream encoder can emit one reasoning block.
                    let mut buf = String::new();
                    if let Some(summary) = item.get("summary").and_then(|v| v.as_array()) {
                        for s in summary {
                            if let Some(t) = s.get("text").and_then(|v| v.as_str()) {
                                if !buf.is_empty() {
                                    buf.push('\n');
                                }
                                buf.push_str(t);
                            }
                        }
                    }
                    if let Some(parts) = item.get("content").and_then(|v| v.as_array()) {
                        for p in parts {
                            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                if !buf.is_empty() {
                                    buf.push('\n');
                                }
                                buf.push_str(t);
                            }
                        }
                    }
                    if !buf.is_empty() {
                        content.push(ContentPart::Reasoning { text: buf });
                    }
                }
                _ => {}
            }
        }
    }
    let finish_reason = body
        .pointer("/status")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "completed" => FinishReason::Stop,
            "incomplete" => FinishReason::Length,
            "failed" => FinishReason::Other,
            _ => FinishReason::Stop,
        })
        .unwrap_or(FinishReason::Stop);
    let usage = body
        .get("usage")
        .and_then(decode_responses_usage)
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
        created_at: body
            .get("created_at")
            .and_then(|v| v.as_i64())
            .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)),
    })
}

fn decode_responses_usage(value: &Value) -> Option<autorouter_core::Usage> {
    let mut usage = autorouter_core::Usage::default();
    if let Some(input) = value.get("input_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.input = Some(input);
    }
    if let Some(output) = value.get("output_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.output = Some(output);
    }
    if let Some(reasoning) = value.get("reasoning_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.reasoning = Some(reasoning);
    }
    if usage.tokens.input.is_some() || usage.tokens.output.is_some() {
        Some(usage)
    } else {
        None
    }
}
