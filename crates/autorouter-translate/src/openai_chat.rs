//! OpenAI Chat Completions adapter.
//!
//! Wire reference: <https://platform.openai.com/docs/api-reference/chat>.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use autorouter_core::{
    ContentPart, FinishReason, Message, MessageRole, ModelDescriptor, ProviderKind, StreamChunk,
    StreamEvent, ToolCall, ToolDefinition, UniversalRequest, UniversalResponse,
};

use crate::error::{TranslateError, TranslateResult};
use crate::reasoning_extractor::{
    split_reasoning, streamer_feed, streamer_finish, ReasoningSplit,
};
use crate::traits::{ProviderAdapter, UpstreamResponse};

/// OpenAI Chat Completions adapter.
#[derive(Debug, Default, Clone)]
pub struct OpenAiChatAdapter;

impl OpenAiChatAdapter {
    pub fn new() -> Self {
        Self
    }
}

/// Convert a single `ReasoningSplit` into the appropriate `StreamChunk`
/// and push it onto the output buffer.
pub(crate) fn push_split(out: &mut Vec<StreamChunk>, split: ReasoningSplit) {
    match split {
        ReasoningSplit::Text(text) => {
            if !text.is_empty() {
                out.push(StreamChunk::from(StreamEvent::TextDelta { text }));
            }
        }
        ReasoningSplit::Reasoning(text) => {
            if !text.is_empty() {
                out.push(StreamChunk::from(StreamEvent::ReasoningDelta { text }));
            }
        }
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiChatAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn display_name(&self) -> &'static str {
        "OpenAI Chat Completions"
    }
    fn models(&self) -> Vec<ModelDescriptor> {
        vec![]
    }

    fn validate(&self, _request: &UniversalRequest) -> TranslateResult<()> {
        Ok(())
    }

    fn encode_request(&self, request: &UniversalRequest) -> TranslateResult<Value> {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(encode_message)
            .collect::<TranslateResult<Vec<_>>>()?;

        let tools: Vec<Value> = request.tools.iter().map(encode_tool).collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": request.stream,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = request.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(m) = request.max_output_tokens {
            body["max_tokens"] = json!(m);
        }
        if !request.stop.is_empty() {
            body["stop"] = json!(request.stop);
        }
        if let Some(user) = &request.user {
            body["user"] = json!(user);
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
        let response = decode_chat_response_body(body)?;
        let _ = request;
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
                // Drain any pending reasoning and clean up tool-call
                // state. If the upstream did not send a finish_reason
                // chunk (some non-compliant providers), emit a fallback
                // Finish so the stream terminates cleanly. Track
                // finish-emitted to avoid a spurious second Finish when
                // the upstream already sent a finish_reason chunk.
                for split in streamer_finish(request_ptr) {
                    push_split(&mut out, split);
                }
                flush_tool_call_ends(request_ptr, &mut out);
                openai_tool_call_drop(request_ptr);
                if !finish_already_emitted(request_ptr) {
                    mark_finish_emitted(request_ptr);
                    out.push(StreamChunk::from(StreamEvent::Finish {
                        reason: FinishReason::Stop,
                        usage: None,
                    }));
                }
                openai_finish_emitted_drop(request_ptr);
                continue;
            }
            let value: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Surface upstream errors instead of silently dropping
            // them. OpenAI-compat upstreams may emit a payload of the
            // shape `{"error": {"message": "...", "type": "..."}}` to
            // signal a fatal mid-stream error; the previous
            // implementation `continue`d past it without emitting any
            // event, leaving the caller with a silently-truncated
            // stream.
            if let Some(err) = value.get("error") {
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("upstream stream error")
                    .to_string();
                let code = err.get("type").and_then(|v| v.as_str()).map(String::from);
                // Drain pending reasoning and drop the tool-call map
                // so the Error event is the last thing the caller
                // sees for this request.
                for split in streamer_finish(request_ptr) {
                    push_split(&mut out, split);
                }
                openai_tool_call_drop(request_ptr);
                openai_finish_emitted_drop(request_ptr);
                out.push(StreamChunk::from(StreamEvent::Error { message, code }));
                continue;
            }
            let Some(choices) = value.get("choices").and_then(|v| v.as_array()) else {
                continue;
            };
            for choice in choices {
                let delta = choice.get("delta");
                if let Some(delta) = delta {
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        // Route inline <!--reasoning-->...<!--/reasoning--> tags
                        // out of `content` into a dedicated ReasoningDelta so
                        // callers see them on the reasoning channel, not as
                        // text. ALWAYS feed through the streamer (even when
                        // this chunk has no `<`) because the streamer's carry
                        // buffer may already hold a partial opener from a
                        // previous chunk; only the streamer has that state.
                        if content.is_empty() {
                            continue;
                        }
                        for split in streamer_feed(request_ptr, content) {
                            push_split(&mut out, split);
                        }
                    }
                    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str())
                    {
                        if !reasoning.is_empty() {
                            // The upstream signalled reasoning via a dedicated
                            // `reasoning_content` field — emit it verbatim as a
                            // ReasoningDelta. We deliberately do NOT prime the
                            // inline-tag streamer here: models that use a
                            // separate `reasoning_content` field (e.g. DeepSeek
                            // R1) send their answer in `content` without any
                            // inline tags, and entering the reasoning state would
                            // misclassify that answer as reasoning. Inline tags
                            // embedded directly in `content` are still handled by
                            // the streamer's Normal-state opener detection.
                            out.push(StreamChunk::from(StreamEvent::ReasoningDelta {
                                text: reasoning.to_string(),
                            }));
                        }
                    }
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tool_calls {
                            handle_openai_tool_call_delta(tc, request_ptr, &mut out);
                        }
                    }
                }
                if let Some(finish) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    if !finish.is_empty() && finish != "null" {
                        // Drain pending reasoning state before the Finish event.
                        for split in streamer_finish(request_ptr) {
                            push_split(&mut out, split);
                        }
                        // Flush in-progress tool calls so consumers
                        // always see ToolCallEnd after the last delta.
                        flush_tool_call_ends(request_ptr, &mut out);
                        openai_tool_call_drop(request_ptr);
                        mark_finish_emitted(request_ptr);
                        let usage = value.get("usage").and_then(decode_usage);
                        out.push(StreamChunk::from(StreamEvent::Finish {
                            reason: self.map_finish_reason(finish),
                            usage,
                        }));
                    }
                }
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
            out.push_str(&crate::streaming::encode_openai_sse(event));
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}

fn encode_message(message: &Message) -> TranslateResult<Value> {
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), Value::String(role.into()));
    if let Some(name) = &message.name {
        obj.insert("name".into(), Value::String(name.clone()));
    }
    if message
        .content
        .iter()
        .all(|p| matches!(p, ContentPart::Text { .. }))
    {
        let text = message
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        obj.insert("content".into(), Value::String(text));
    } else {
        let mut parts: Vec<Value> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        let mut tool_results: Vec<(String, String)> = Vec::new();
        for part in &message.content {
            match part {
                ContentPart::Text { text } => {
                    parts.push(json!({ "type": "text", "text": text }));
                }
                ContentPart::Image { source, .. } => match source {
                    autorouter_core::ImageSource::Url { url } => {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    }
                    autorouter_core::ImageSource::Base64 { media_type, data } => {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", media_type, data)
                            }
                        }));
                    }
                    autorouter_core::ImageSource::FileId { id } => {
                        parts.push(json!({
                            "type": "image_file",
                            "image_file": { "file_id": id }
                        }));
                    }
                },
                ContentPart::Audio { source } => {
                    if let autorouter_core::ImageSource::Base64 { media_type, data } = source {
                        parts.push(json!({
                            "type": "input_audio",
                            "input_audio": { "data": data, "format": media_type }
                        }));
                    }
                }
                ContentPart::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(arguments).unwrap_or_default()
                        }
                    }));
                }
                ContentPart::ToolCallRaw {
                    id,
                    name,
                    arguments_raw,
                } => {
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments_raw }
                    }));
                }
                ContentPart::ToolResult {
                    tool_call_id: id,
                    content,
                    is_error,
                } => {
                    let text = match content {
                        autorouter_core::ToolResultPayload::Text { text } => text.clone(),
                        autorouter_core::ToolResultPayload::Json { value } => {
                            serde_json::to_string(value).unwrap_or_default()
                        }
                    };
                    if *is_error {
                        tool_results.push((id.clone(), format!("[tool error] {}", text)));
                    } else {
                        tool_results.push((id.clone(), text));
                    }
                }
                ContentPart::Document { source, filename } => {
                    // OpenAI Chat Completions has no native document part.
                    // Convert the base64 bytes to a data URL and attach as
                    // a generic image_url part so the model at least sees
                    // something, then surface a warning.
                    if let autorouter_core::ImageSource::Base64 { media_type, data } = source {
                        let url = format!("data:{};base64,{}", media_type, data);
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    }
                    let _ = filename;
                }
                ContentPart::ToolUse { id, name, input } => {
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_default()
                        }
                    }));
                }
                ContentPart::Reasoning { .. } => {
                    // Reasoning / thinking content is response-only.
                    // Skip silently when encoding a request — the
                    // upstream model should never be asked to consume
                    // prior reasoning text as input. The streaming /
                    // non-streaming response paths emit reasoning via
                    // `reasoning_content` on the assistant message.
                }
                ContentPart::Unknown { raw, .. } => {
                    return Err(TranslateError::invalid_payload(
                        "openai_chat",
                        format!("cannot encode unknown content part: {}", raw),
                    ));
                }
            }
        }
        if !tool_results.is_empty() {
            // OpenAI's tool-role message accepts a single `tool_call_id`
            // and a single content string. The common case is exactly
            // one tool result — emit it verbatim so the upstream model
            // sees the clean tool output. The degenerate multi-result
            // case (more than one ToolResult in one message) cannot be
            // represented faithfully; we disambiguate with an `[id]`
            // prefix per result rather than silently dropping all but
            // the last.
            let combined_content = if tool_results.len() == 1 {
                tool_results[0].1.clone()
            } else {
                tool_results
                    .iter()
                    .map(|(id, content)| format!("[{}] {}", id, content))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let text_prefix: String = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            let combined = if text_prefix.is_empty() {
                combined_content
            } else {
                format!("{}\n{}", text_prefix, combined_content)
            };
            obj.insert("content".into(), Value::String(combined));
            if let Some((first_id, _)) = tool_results.first() {
                obj.insert("tool_call_id".into(), Value::String(first_id.clone()));
            }
        } else if !parts.is_empty() {
            obj.insert("content".into(), Value::Array(parts));
        } else {
            obj.insert("content".into(), Value::String(String::new()));
        }
        if !tool_calls.is_empty() {
            obj.insert("tool_calls".into(), Value::Array(tool_calls));
        }
    }
    Ok(Value::Object(obj))
}

fn encode_tool(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
            "strict": tool.strict,
        }
    })
}

/// Per-stream tool-call index→id map, keyed by request pointer.
/// Used to correlate subsequent argument deltas (which lack `id`)
/// with the original `ToolCallStart` event.
static TOOL_CALL_IDS: OnceLock<Mutex<HashMap<usize, HashMap<u64, String>>>> = OnceLock::new();

/// Per-stream finish-emitted flag. Prevents a spurious second
/// Finish event from the `[DONE]` sentinel when the upstream already
/// emitted a finish_reason chunk.
static FINISH_EMITTED: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();

fn tool_call_ids() -> &'static Mutex<HashMap<usize, HashMap<u64, String>>> {
    TOOL_CALL_IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn finish_emitted() -> &'static Mutex<HashSet<usize>> {
    FINISH_EMITTED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_finish_emitted(request_ptr: usize) {
    finish_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(request_ptr);
}

fn finish_already_emitted(request_ptr: usize) -> bool {
    finish_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&request_ptr)
}

/// Remove the per-stream finish flag so the map doesn't accumulate
/// entries across streams. Called alongside `openai_tool_call_drop`.
/// Also called from `autorouter_server::upstream` when the byte stream
/// ends without a `[DONE]` sentinel (e.g. an upstream that closes the
/// connection right after `finish_reason`), so the flag cannot leak
/// and pin a stream_id that a future stream reuses.
pub fn openai_finish_emitted_drop(request_ptr: usize) {
    finish_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&request_ptr);
}

/// Clean up the per-request tool-call id map. Called when the stream
/// ends (either via [DONE] or Finish) to prevent memory leaks.
pub fn openai_tool_call_drop(request_ptr: usize) {
    let mut map = tool_call_ids().lock().unwrap_or_else(|e| e.into_inner());
    map.remove(&request_ptr);
}

fn handle_openai_tool_call_delta(tc: &Value, request_ptr: usize, out: &mut Vec<StreamChunk>) {
    let id = tc.get("id").and_then(|v| v.as_str()).map(String::from);
    let name = tc
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let args = tc
        .get("function")
        .and_then(|f| f.get("arguments"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    // Is this the first delta for this tool-call index? We key a "start"
    // on the presence of an unseen id rather than on the function name:
    // some non-compliant upstreams (Azure, certain proxies) emit the
    // first delta with an id + arguments but no name. Treating that as a
    // "subsequent" delta would emit a ToolCallDelta with no preceding
    // ToolCallStart, so the consumer can never assemble the call.
    let already_started = {
        let map = tool_call_ids().lock().unwrap_or_else(|e| e.into_inner());
        map.get(&request_ptr)
            .map(|m| m.contains_key(&index))
            .unwrap_or(false)
    };

    if !already_started {
        if let Some(id) = id {
            {
                let mut map = tool_call_ids().lock().unwrap_or_else(|e| e.into_inner());
                map.entry(request_ptr)
                    .or_default()
                    .insert(index, id.clone());
            }
            // Fall back to an empty name if the upstream omitted it on the
            // opening delta; the consumer still gets a well-formed Start it
            // can correlate subsequent deltas against.
            let name = name.unwrap_or_default();
            let arguments: Value = match &args {
                Some(a) => serde_json::from_str(a).unwrap_or(Value::String(a.clone())),
                None => Value::Null,
            };
            out.push(StreamChunk::from(StreamEvent::ToolCallStart {
                call: ToolCall { id, name, arguments },
            }));
            // ToolCallEnd is deferred until finish_reason arrives or the
            // stream terminates — emitting it here would signal completion
            // before subsequent argument deltas arrive.
            return;
        }
    }

    if let Some(args) = args {
        // Subsequent delta — look up the id by index.
        let id = {
            let map = tool_call_ids().lock().unwrap_or_else(|e| e.into_inner());
            map.get(&request_ptr)
                .and_then(|m| m.get(&index))
                .cloned()
                .unwrap_or_else(|| format!("call_{index}"))
        };
        out.push(StreamChunk::from(StreamEvent::ToolCallDelta {
            id,
            arguments_fragment: args,
        }));
    }
}

/// Emit `ToolCallEnd` for every in-progress tool call on the given
/// stream. Called when finish_reason arrives or when the stream ends.
fn flush_tool_call_ends(request_ptr: usize, out: &mut Vec<StreamChunk>) {
    let ids: Vec<String> = {
        let map = tool_call_ids().lock().unwrap_or_else(|e| e.into_inner());
        map.get(&request_ptr)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    };
    for id in &ids {
        out.push(StreamChunk::from(StreamEvent::ToolCallEnd {
            id: id.clone(),
        }));
    }
}

fn decode_chat_response_body(body: &Value) -> TranslateResult<UniversalResponse> {
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
    let created = body
        .get("created")
        .and_then(|v| v.as_i64())
        .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0));
    let mut content: Vec<ContentPart> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let finish_reason;
    let usage;
    if let Some(choices) = body.get("choices").and_then(|v| v.as_array()) {
        if choices.len() > 1 {
            tracing::debug!("multiple choices not yet supported; only first is used");
        }
        if let Some(choice) = choices.first() {
            if let Some(message) = choice.get("message") {
                if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        // Split inline reasoning tags out of the content so they
                        // surface as Reasoning parts rather than text.
                        for split in split_reasoning(text) {
                            match split {
                                crate::reasoning_extractor::ReasoningSplit::Text(t) => {
                                    content.push(ContentPart::Text { text: t });
                                }
                                crate::reasoning_extractor::ReasoningSplit::Reasoning(t) => {
                                    content.push(ContentPart::Reasoning { text: t });
                                }
                            }
                        }
                    }
                }
                if let Some(reasoning) = message.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        content.push(ContentPart::Reasoning {
                            text: reasoning.to_string(),
                        });
                    }
                }
                if let Some(tcs) = message.get("tool_calls").and_then(|v| v.as_array()) {
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
                }
            }
            finish_reason = choice
                .get("finish_reason")
                .and_then(|v| v.as_str())
                .map(|s| match s {
                    "stop" => FinishReason::Stop,
                    "length" => FinishReason::Length,
                    "tool_calls" => FinishReason::ToolCalls,
                    "content_filter" => FinishReason::ContentFilter,
                    _ => FinishReason::Other,
                })
                .unwrap_or(FinishReason::Stop);
            usage = body.get("usage").and_then(decode_usage).unwrap_or_default();
            return Ok(UniversalResponse {
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
                created_at: created,
            });
        }
    }
    Err(TranslateError::invalid_payload(
        "openai_chat",
        "response had no choices array",
    ))
}

fn decode_usage(value: &Value) -> Option<autorouter_core::Usage> {
    let mut usage = autorouter_core::Usage::default();
    if let Some(prompt) = value.get("prompt_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.input = Some(prompt);
    }
    if let Some(completion) = value.get("completion_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.output = Some(completion);
    }
    if let Some(cached) = value
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
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
