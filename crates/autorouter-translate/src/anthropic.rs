//! Anthropic Messages API adapter.
//!
//! Wire reference: <https://docs.anthropic.com/en/api/messages>.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use autorouter_core::{
    ContentPart, FinishReason, Message, MessageRole, ModelDescriptor, ProviderKind, StreamChunk,
    StreamEvent, ToolCall, ToolDefinition, UniversalRequest, UniversalResponse,
};

use crate::error::{TranslateError, TranslateResult};
use crate::openai_chat::push_split;
use crate::reasoning_extractor::{split_reasoning, streamer_feed, streamer_finish, ReasoningSplit};

/// Per-stream finish-emitted tracker. Prevents a spurious second
/// Finish event when `message_stop` arrives after `message_delta`.
static ANTHROPIC_FINISH_EMITTED: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();

fn anthropic_finish_emitted() -> &'static Mutex<HashSet<usize>> {
    ANTHROPIC_FINISH_EMITTED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_anthropic_finish(request_ptr: usize) {
    anthropic_finish_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(request_ptr);
}

fn anthropic_finish_already(request_ptr: usize) -> bool {
    anthropic_finish_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&request_ptr)
}

/// Remove the per-stream finish flag. Called from the `message_stop`
/// and `error` arms, and also from `autorouter_server::upstream` when
/// the byte stream ends so the flag cannot outlive the stream.
pub fn anthropic_finish_drop(request_ptr: usize) {
    anthropic_finish_emitted()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&request_ptr);
}

/// Per-stream content-block index → tool-call id map for Anthropic.
static ANTHROPIC_TOOL_IDS: OnceLock<Mutex<HashMap<usize, HashMap<i64, String>>>> = OnceLock::new();

fn anthropic_tool_ids() -> &'static Mutex<HashMap<usize, HashMap<i64, String>>> {
    ANTHROPIC_TOOL_IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn anthropic_tool_call_drop(request_ptr: usize) {
    let mut map = anthropic_tool_ids()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.remove(&request_ptr);
}
use crate::traits::{ProviderAdapter, UpstreamResponse};

/// Anthropic Messages adapter.
#[derive(Debug, Default, Clone)]
pub struct AnthropicAdapter;

impl AnthropicAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }
    fn display_name(&self) -> &'static str {
        "Anthropic Messages"
    }

    fn models(&self) -> Vec<ModelDescriptor> {
        vec![]
    }

    fn validate(&self, _request: &UniversalRequest) -> TranslateResult<()> {
        Ok(())
    }

    fn encode_request(&self, request: &UniversalRequest) -> TranslateResult<Value> {
        // Anthropic requires the system prompt as a top-level field,
        // never as a message in the messages array. We also enforce that
        // the last user message exists if any are present.
        //
        // When a system message carries an Unknown part whose `raw`
        // is an Anthropic text block with `cache_control`, emit
        // `system` as the documented array-of-blocks form (not a
        // plain string) so the upstream honours the cache pin. The
        // plain-string fallback is preserved for the cache-free case.
        let mut system_text: Option<String> = None;
        let mut system_blocks: Vec<Value> = Vec::new();
        for m in &request.messages {
            if m.role != MessageRole::System {
                continue;
            }
            for part in &m.content {
                match part {
                    ContentPart::Text { text } => {
                        system_text = Some(match system_text.take() {
                            Some(prev) => format!("{}\n{}", prev, text),
                            None => text.clone(),
                        });
                        if !system_blocks.is_empty() {
                            // We started emitting blocks for an
                            // earlier pin; keep them and append a
                            // matching text block.
                            system_blocks.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    ContentPart::Unknown { provider, raw } if provider == "anthropic" => {
                        // If the raw block was a pinned text segment
                        // (i.e. contains `cache_control`), emit the
                        // original block verbatim — that preserves
                        // both the text and the cache hint. If the
                        // block is something else (image / tool_use
                        // / etc.) we still forward it so the
                        // upstream can decide.
                        if !system_blocks.is_empty() {
                            system_blocks.push(raw.clone());
                        } else if let Some(prev) = system_text.take() {
                            // We already started a plain string —
                            // promote to the block-list form so the
                            // pin metadata isn't lost.
                            system_blocks.push(json!({ "type": "text", "text": prev }));
                            system_blocks.push(raw.clone());
                        } else if let Some(text) = raw.get("text").and_then(|v| v.as_str()) {
                            system_blocks.push(json!({ "type": "text", "text": text }));
                        } else {
                            system_blocks.push(raw.clone());
                        }
                    }
                    ContentPart::Unknown { raw, .. } => {
                        if !system_blocks.is_empty() {
                            system_blocks.push(raw.clone());
                        } else if let Some(prev) = system_text.take() {
                            system_blocks.push(json!({ "type": "text", "text": prev }));
                            system_blocks.push(raw.clone());
                        } else {
                            system_blocks.push(raw.clone());
                        }
                    }
                    _ => {
                        // Other content types (e.g. Image) cannot be
                        // part of the system prompt per Anthropic's
                        // spec; drop them silently rather than
                        // breaking the encode.
                    }
                }
            }
        }
        let mut messages: Vec<Value> = Vec::new();
        for m in &request.messages {
            match m.role {
                MessageRole::System => continue, // handled above
                MessageRole::User => {
                    messages.push(json!({ "role": "user", "content": encode_user_content(m)? }));
                }
                MessageRole::Assistant => {
                    messages.push(
                        json!({ "role": "assistant", "content": encode_assistant_content(m) }),
                    );
                }
                MessageRole::Tool => {
                    // Anthropic uses a `tool_result` content block inside a
                    // user message.
                    let mut content_blocks: Vec<Value> = Vec::new();
                    for part in &m.content {
                        if let ContentPart::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                        } = part
                        {
                            let inner = match content {
                                autorouter_core::ToolResultPayload::Text { text } => json!(text),
                                autorouter_core::ToolResultPayload::Json { value } => value.clone(),
                            };
                            let mut block = json!({
                                "type": "tool_result",
                                "tool_use_id": tool_call_id,
                                "content": inner,
                            });
                            if *is_error {
                                block["is_error"] = json!(true);
                            }
                            content_blocks.push(block);
                        }
                    }
                    if content_blocks.is_empty() {
                        continue;
                    }
                    messages.push(json!({ "role": "user", "content": content_blocks }));
                }
            }
        }
        if messages.is_empty() {
            return Err(TranslateError::invalid_payload(
                "anthropic",
                "request had no user/assistant messages",
            ));
        }
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_output_tokens.unwrap_or(4096),
        });
        if !system_blocks.is_empty() {
            body["system"] = json!(system_blocks);
        } else if let Some(s) = system_text {
            body["system"] = json!(s);
        }
        let tools = encode_tools(&request.tools);
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        if let Some(t) = request.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = request.top_p {
            body["top_p"] = json!(p);
        }
        if !request.stop.is_empty() {
            body["stop_sequences"] = json!(request.stop);
        }
        if let Some(user) = &request.user {
            body["metadata"] = json!({ "user_id": user });
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
        let response = decode_anthropic_response(body)?;
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
            let line = line.trim_end();
            let line = line.trim_start();
            if line.is_empty() {
                continue;
            }
            let (event, payload) = match line.split_once(':') {
                Some((e, p)) => (e.trim(), p.trim()),
                None => continue,
            };
            if event != "data" {
                continue;
            }
            let value: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match event_type {
                "message_start" => {
                    let id = value
                        .get("message")
                        .and_then(|m| m.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let model = value
                        .get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push(StreamChunk::from(StreamEvent::Start { id, model }));
                }
                "content_block_start" => {
                    if let Some(block) = value.get("content_block") {
                        let block_index =
                            value.get("index").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            {
                                let mut map = anthropic_tool_ids()
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                map.entry(request_ptr)
                                    .or_default()
                                    .insert(block_index as i64, id.clone());
                            }
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            out.push(StreamChunk::with_index(
                                vec![StreamEvent::ToolCallStart {
                                    call: ToolCall {
                                        id,
                                        name,
                                        arguments: Value::Null,
                                    },
                                }],
                                block_index,
                            ));
                        }
                    }
                }
                "content_block_delta" => {
                    let block_index =
                        value.get("index").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
                    if let Some(delta) = value.get("delta") {
                        match delta.get("type").and_then(|v| v.as_str()) {
                            Some("text_delta") => {
                                if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    // ALWAYS feed through the streamer so partial
                                    // openers from previous chunks are correctly
                                    // recognized (the streamer owns the carry).
                                    for split in streamer_feed(request_ptr, text) {
                                        push_split(&mut out, split);
                                    }
                                    // Stamp the content-block index on the last
                                    // emitted chunk so the SSE encoder uses it.
                                    if let Some(chunk) = out.last_mut() {
                                        chunk.index = block_index;
                                    }
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                                    out.push(StreamChunk::with_index(
                                        vec![StreamEvent::ReasoningDelta {
                                            text: text.to_string(),
                                        }],
                                        block_index,
                                    ));
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(partial) =
                                    delta.get("partial_json").and_then(|v| v.as_str())
                                {
                                    let id = {
                                        let map = anthropic_tool_ids()
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner());
                                        map.get(&request_ptr)
                                            .and_then(|m| m.get(&(block_index as i64)))
                                            .cloned()
                                            .unwrap_or_else(|| block_index.to_string())
                                    };
                                    out.push(StreamChunk::with_index(
                                        vec![StreamEvent::ToolCallDelta {
                                            id,
                                            arguments_fragment: partial.to_string(),
                                        }],
                                        block_index,
                                    ));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "content_block_stop" => {
                    if let Some(idx) = value.get("index").and_then(|v| v.as_i64()) {
                        let id = {
                            let map = anthropic_tool_ids()
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            map.get(&request_ptr).and_then(|m| m.get(&idx)).cloned()
                        };
                        if let Some(id) = id {
                            out.push(StreamChunk::with_index(
                                vec![StreamEvent::ToolCallEnd { id }],
                                idx as u32,
                            ));
                        }
                    }
                }
                "message_delta" => {
                    // Drain any pending reasoning state before the Finish
                    // event. This is the de-facto terminal: `message_delta`
                    // carries the stop_reason and usage, so we drain
                    // reasoning state AND drop the tool-call map here. The
                    // `message_stop` sentinel (which follows) is merely a
                    // wire-level event that some providers may not send
                    // after a disconnect, so doing the cleanup here avoids
                    // leaking both reasoning streamers and tool-call maps
                    // across streams.
                    for split in streamer_finish(request_ptr) {
                        push_split(&mut out, split);
                    }
                    anthropic_tool_call_drop(request_ptr);
                    mark_anthropic_finish(request_ptr);
                    if let Some(stop_reason) = value
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(|v| v.as_str())
                    {
                        let usage = value.get("usage").and_then(decode_anthropic_usage);
                        out.push(StreamChunk::from(StreamEvent::Finish {
                            reason: map_anthropic_stop_reason(stop_reason),
                            usage,
                        }));
                    }
                }
                "message_stop" => {
                    for split in streamer_finish(request_ptr) {
                        push_split(&mut out, split);
                    }
                    anthropic_tool_call_drop(request_ptr);
                    // Some proxies send message_stop without a preceding
                    // message_delta. Emit a fallback Finish so the
                    // consumer gets a terminal event.
                    if !anthropic_finish_already(request_ptr) {
                        out.push(StreamChunk::from(StreamEvent::Finish {
                            reason: FinishReason::Stop,
                            usage: None,
                        }));
                    }
                    anthropic_finish_drop(request_ptr);
                }
                "error" => {
                    // Drain pending reasoning and drop the tool-call
                    // id map so the upstream-emitted Error event is
                    // the LAST thing the caller sees for this request.
                    // Without `anthropic_tool_call_drop` here, a
                    // stream that terminates on an upstream error
                    // would leak its map entry permanently (until
                    // process restart).
                    for split in streamer_finish(request_ptr) {
                        push_split(&mut out, split);
                    }
                    anthropic_tool_call_drop(request_ptr);
                    anthropic_finish_drop(request_ptr);
                    let message = value
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("anthropic stream error")
                        .to_string();
                    let code = value
                        .get("error")
                        .and_then(|e| e.get("type"))
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
        // B2: Anthropic Messages SSE uses event: <name>\ndata: <json>\n\n per event.
        // The trailing event: message_stop is the gateways responsibility to emit after the Finish event.
        let mut out = String::new();
        let index = chunk.index;
        for event in &chunk.events {
            out.push_str(&crate::streaming::encode_anthropic_sse_with_index(
                event, index,
            ));
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}

fn map_anthropic_stop_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::Safety,
        _ => FinishReason::Other,
    }
}

fn encode_user_content(message: &Message) -> TranslateResult<Value> {
    // A text part may be followed by an Unknown part whose raw is an
    // Anthropic text block carrying `cache_control`. Walk the content
    // parts and attach the cache hint to the preceding text block. If
    // no text precedes the pin, emit the original block verbatim.
    if message
        .content
        .iter()
        .all(|p| matches!(p, ContentPart::Text { .. }))
    {
        return Ok(Value::String(message.text()));
    }
    let mut blocks: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < message.content.len() {
        let part = &message.content[i];
        match part {
            ContentPart::Text { text } => {
                let mut block = json!({ "type": "text", "text": text });
                // Look ahead for an attached Unknown cache_control
                // and merge it onto this text block.
                if let Some(ContentPart::Unknown { provider, raw }) = message.content.get(i + 1) {
                    if provider == "anthropic" && raw.get("cache_control").is_some() {
                        if let Some(cc) = raw.get("cache_control") {
                            block["cache_control"] = cc.clone();
                        }
                        i += 1; // consume the attached Unknown part
                    }
                }
                blocks.push(block);
            }
            _ => {
                if let Some(v) = encode_user_part(part)? {
                    blocks.push(v);
                }
            }
        }
        i += 1;
    }
    Ok(Value::Array(blocks))
}

/// M3: surface `ImageSource::Url` and `ImageSource::FileId` as
/// a hard error rather than silently dropping the content part.
/// Anthropic only accepts base64-encoded image bytes; a URL must be
/// fetched and inlined by the caller first, and a FileId is not part
/// of the Anthropic wire format at all.
fn encode_user_part(part: &ContentPart) -> TranslateResult<Option<Value>> {
    match part {
        ContentPart::Text { text } => Ok(Some(json!({ "type": "text", "text": text }))),
        ContentPart::Image { source, .. } => {
            let (media_type, data) = match source {
                autorouter_core::ImageSource::Base64 { media_type, data } => {
                    (media_type.clone(), data.clone())
                }
                autorouter_core::ImageSource::Url { url } => {
                    return Err(TranslateError::unsupported_content(
                        "anthropic",
                        format!(
                            "image URL '{}' cannot be sent to Anthropic; fetch and inline the bytes first",
                            url
                        ),
                    ));
                }
                autorouter_core::ImageSource::FileId { id } => {
                    return Err(TranslateError::unsupported_content(
                        "anthropic",
                        format!(
                            "image FileId '{}' is not representable in the Anthropic wire format",
                            id
                        ),
                    ));
                }
            };
            Ok(Some(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data }
            })))
        }
        ContentPart::Audio { source } => {
            // Anthropic does not support inline audio; surface a
            // descriptive error so the caller knows to pre-process.
            match source {
                autorouter_core::ImageSource::Base64 { media_type, .. } => {
                    Err(TranslateError::unsupported_content(
                        "anthropic",
                        format!(
                            "audio content ({}) cannot be sent to Anthropic; convert to text first",
                            media_type
                        ),
                    ))
                }
                _ => Err(TranslateError::unsupported_content(
                    "anthropic",
                    "audio content cannot be sent to Anthropic; convert to text first",
                )),
            }
        }
        ContentPart::Document { source, .. } => {
            if let autorouter_core::ImageSource::Base64 { media_type, data } = source {
                Ok(Some(json!({
                    "type": "document",
                    "source": { "type": "base64", "media_type": media_type, "data": data }
                })))
            } else {
                Err(TranslateError::unsupported_content(
                    "anthropic",
                    "document URL references cannot be sent to Anthropic; inline the bytes first",
                ))
            }
        }
        _ => Err(TranslateError::unsupported_content(
            "anthropic",
            "unsupported content part",
        )),
    }
}

fn encode_assistant_content(message: &Message) -> Value {
    if message
        .content
        .iter()
        .all(|p| matches!(p, ContentPart::Text { .. }))
    {
        return Value::Array(vec![json!({ "type": "text", "text": message.text() })]);
    }
    // Walk text parts and any following Unknown cache_control markers,
    // emitting pinned text blocks that the upstream can honour for
    // prompt caching.
    let mut blocks: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < message.content.len() {
        let part = &message.content[i];
        match part {
            ContentPart::Text { text } => {
                let mut block = json!({ "type": "text", "text": text });
                if let Some(ContentPart::Unknown { provider, raw }) = message.content.get(i + 1) {
                    if provider == "anthropic" && raw.get("cache_control").is_some() {
                        if let Some(cc) = raw.get("cache_control") {
                            block["cache_control"] = cc.clone();
                        }
                        i += 1;
                    }
                }
                blocks.push(block);
            }
            ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": arguments,
            })),
            ContentPart::ToolCallRaw {
                id,
                name,
                arguments_raw,
            } => blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": arguments_raw,
            })),
            ContentPart::ToolUse {
                id,
                name,
                input,
            } => blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            })),
            _ => {}
        }
        i += 1;
    }
    Value::Array(blocks)
}

fn encode_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect()
}

fn decode_anthropic_response(body: &Value) -> TranslateResult<UniversalResponse> {
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
    if let Some(blocks) = body.get("content").and_then(|v| v.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        if text.is_empty() {
                            // Skip but still iterate other blocks.
                        } else {
                            // Split inline reasoning tags out of text blocks so
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
                }
                Some("thinking") => {
                    // Anthropic extended-thinking blocks carry the
                    // chain-of-thought in a `thinking` field. Surface
                    // them as Reasoning so they are not silently
                    // dropped by the `_ => {}` catch-all.
                    if let Some(text) = block.get("thinking").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content.push(ContentPart::Reasoning {
                                text: text.to_string(),
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
                    let arguments = block.get("input").cloned().unwrap_or(Value::Null);
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
                _ => {}
            }
        }
    }
    let finish_reason = body
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .map(map_anthropic_stop_reason)
        .unwrap_or(FinishReason::Stop);
    let usage = body
        .get("usage")
        .and_then(decode_anthropic_usage)
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

fn decode_anthropic_usage(value: &Value) -> Option<autorouter_core::Usage> {
    let mut usage = autorouter_core::Usage::default();
    if let Some(input) = value.get("input_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.input = Some(input);
    }
    if let Some(output) = value.get("output_tokens").and_then(|v| v.as_u64()) {
        usage.tokens.output = Some(output);
    }
    if let Some(cache_read) = value
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
    {
        usage.tokens.cache_read = Some(cache_read);
    }
    if let Some(cache_write) = value
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
    {
        usage.tokens.cache_write = Some(cache_write);
    }
    if usage.tokens.input.is_some() || usage.tokens.output.is_some() {
        Some(usage)
    } else {
        None
    }
}
