//! Streaming translation utilities.
//!
//! Phase 1 only exposes a few small helpers. The Phase 3 gateway uses
//! these to bridge the upstream SSE stream into the consumer wire format.

use autorouter_core::{FinishReason, StreamEvent};

use crate::error::TranslateResult;

/// Format a list of universal events as a `data: {...}` SSE chunk
/// suitable for sending to a consumer. The chunk ends with a
/// double newline as required by the SSE spec.
pub fn format_sse_chunk(events: &[StreamEvent]) -> TranslateResult<Option<String>> {
    if events.is_empty() {
        return Ok(None);
    }
    // Serialise as a JSON object (not a bare array), because some
    // SSE consumers reject top-level arrays.
    let payload = if events.len() == 1 {
        serde_json::to_string(&events[0])?
    } else {
        serde_json::to_string(&serde_json::json!({ "events": events }))?
    };
    Ok(Some(format!("data: {}\n\n", payload)))
}

/// Format the `[DONE]` sentinel that OpenAI-compatible consumers expect
/// at the end of a stream.
pub fn format_done_sentinel() -> String {
    "data: [DONE]\n\n".to_string()
}

/// Anthropic requires a trailing `event: message_stop` with empty
/// `data` to mark the end of a stream.
pub fn format_anthropic_stop_sentinel() -> String {
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()
}

/// Emit `data: [DONE]\n\n` and `event: ping\n\n` helpers that the
/// OpenAI Responses SSE format expects to optionally include.
pub fn format_openai_responses_done() -> String {
    "data: [DONE]\n\n".to_string()
}

/// OpenAI-compatible SSE: always `data: <json>\n\n` and trailing
/// `data: [DONE]\n\n`. Used by `OpenAiChatAdapter` and the Responses
/// adapter; the trailing `[DONE]` is the consumer`s responsibility.
pub fn encode_openai_sse(event: &StreamEvent) -> String {
    let payload = openai_payload(event);
    format!("data: {}\n\n", payload)
}

fn openai_payload(event: &StreamEvent) -> serde_json::Value {
    use serde_json::json;
    match event {
        StreamEvent::Start { id, model } => json!({
            "id": id,
            "object": "chat.completion.chunk",
            "model": model,
            "choices": [{ "index": 0, "delta": {}, "finish_reason": null }]
        }),
        StreamEvent::TextDelta { text } => json!({
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "content": text },
                "finish_reason": null,
            }]
        }),
        StreamEvent::ReasoningDelta { text } => json!({
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": text },
                "finish_reason": null,
            }]
        }),
        StreamEvent::ToolCallStart { call } => json!({
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments).unwrap_or_default(),
                        }
                    }]
                },
                "finish_reason": null,
            }]
        }),
        StreamEvent::ToolCallDelta {
            id,
            arguments_fragment,
        } => json!({
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "function": { "arguments": arguments_fragment }
                    }]
                },
                "finish_reason": null,
            }]
        }),
        StreamEvent::ToolCallEnd { .. } => json!({
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "tool_calls": [{ "index": 0 }] },
                "finish_reason": null,
            }]
        }),
        StreamEvent::Finish { reason, usage } => {
            let finish_reason = match reason {
                FinishReason::Stop => "stop",
                FinishReason::Length => "length",
                FinishReason::ToolCalls => "tool_calls",
                FinishReason::ContentFilter => "content_filter",
                // Safety and the catch-all `Other` were both being
                // collapsed to "stop", which made safety-triggered
                // terminations indistinguishable from a normal
                // completion. Map Safety to OpenAI's "content_filter"
                // (closest analog) and `Other` to "stop" only as a
                // last resort.
                FinishReason::Safety => "content_filter",
                FinishReason::Other => "stop",
                _ => "stop",
            };
            let mut body = json!({
                "object": "chat.completion.chunk",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": finish_reason,
                }]
            });
            if let Some(u) = usage {
                body["usage"] = json!({
                    "prompt_tokens": u.tokens.input.unwrap_or(0),
                    "completion_tokens": u.tokens.output.unwrap_or(0),
                    "total_tokens": u.total_tokens(),
                });
            }
            body
        }
        StreamEvent::UsageDelta { .. } => json!({}),
        StreamEvent::Error { message, code } => json!({
            "error": { "message": message, "code": code }
        }),
        _ => json!({}),
    }
}

/// Anthropic Messages SSE: `event: <name>\ndata: <json>\n\n` per
/// event. The trailing `event: message_stop` is the consumer's
/// responsibility; emit it with [`format_anthropic_stop_sentinel`].
///
/// Uses the default content-block index 0. Prefer
/// [`encode_anthropic_sse_with_index`] when the decoder has tracked
/// the actual content-block position.
pub fn encode_anthropic_sse(event: &StreamEvent) -> String {
    encode_anthropic_sse_with_index(event, 0)
}

/// Like [`encode_anthropic_sse`] but with an explicit content-block
/// index. The adapter should set this from the upstream chunk so
/// multi-block messages (text + tool_use) have correct indices.
pub fn encode_anthropic_sse_with_index(event: &StreamEvent, index: u32) -> String {
    let (name, payload) = anthropic_event_with_index(event, index);
    format!("event: {}\ndata: {}\n\n", name, payload)
}

fn anthropic_event_with_index(
    event: &StreamEvent,
    index: u32,
) -> (&'static str, serde_json::Value) {
    use serde_json::json;
    match event {
        StreamEvent::Start { id, model } => (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                }
            }),
        ),
        StreamEvent::TextDelta { text } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        ),
        StreamEvent::ReasoningDelta { text } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": text }
            }),
        ),
        StreamEvent::ToolCallStart { call } => (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": call.arguments,
                }
            }),
        ),
        StreamEvent::ToolCallDelta {
            arguments_fragment, ..
        } => (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": arguments_fragment }
            }),
        ),
        StreamEvent::ToolCallEnd { .. } => (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        ),
        StreamEvent::Finish { reason, usage } => {
            let stop_reason = match reason {
                FinishReason::Stop => "end_turn",
                FinishReason::Length => "max_tokens",
                FinishReason::ToolCalls => "tool_use",
                FinishReason::Safety | FinishReason::ContentFilter => "refusal",
                _ => "end_turn",
            };
            let usage = usage.clone().unwrap_or_default();
            (
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop_reason },
                    "usage": {
                        "input_tokens": usage.tokens.input.unwrap_or(0),
                        "output_tokens": usage.tokens.output.unwrap_or(0),
                    }
                }),
            )
        }
        StreamEvent::UsageDelta { .. } => ("", json!({})),
        StreamEvent::Error { message, code } => (
            "error",
            json!({
                "type": "error",
                "error": { "type": "api_error", "message": message, "code": code }
            }),
        ),
        _ => ("", json!({})),
    }
}

/// Gemini `streamGenerateContent` SSE: `data: <json>\n\n` per event,
/// closed when the connection terminates (no sentinel).
pub fn encode_gemini_sse(event: &StreamEvent) -> String {
    let payload = gemini_payload(event);
    format!("data: {}\n\n", payload)
}

fn gemini_payload(event: &StreamEvent) -> serde_json::Value {
    use serde_json::json;
    match event {
        StreamEvent::TextDelta { text } => json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": text }] },
                "finishReason": null,
            }]
        }),
        StreamEvent::ReasoningDelta { text } => json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": text, "thought": true }] },
                "finishReason": null,
            }]
        }),
        StreamEvent::ToolCallStart { call } => json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": { "name": call.name, "args": call.arguments, "id": call.id }
                    }]
                },
                "finishReason": null,
            }]
        }),
        StreamEvent::ToolCallDelta {
            id,
            arguments_fragment,
        } => json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": { "args": arguments_fragment, "id": id }
                    }]
                },
                "finishReason": null,
            }]
        }),
        StreamEvent::ToolCallEnd { .. } => json!({
            "candidates": [{
                "content": { "role": "model", "parts": [] },
                "finishReason": null,
            }]
        }),
        StreamEvent::Finish { reason, usage } => {
            let finish_reason = match reason {
                FinishReason::Length => "MAX_TOKENS",
                FinishReason::Safety => "SAFETY",
                _ => "STOP",
            };
            json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [] },
                    "finishReason": finish_reason,
                }],
                "usageMetadata": {
                    "promptTokenCount": usage.as_ref().and_then(|u| u.tokens.input).unwrap_or(0),
                    "candidatesTokenCount": usage.as_ref().and_then(|u| u.tokens.output).unwrap_or(0),
                }
            })
        }
        StreamEvent::Start { id, model } => json!({
            "candidates": [{ "content": { "role": "model", "parts": [] }, "finishReason": null }],
            "responseId": id,
            "modelVersion": model,
        }),
        StreamEvent::UsageDelta { .. } => json!({}),
        StreamEvent::Error { message, code } => json!({
            "error": { "message": message, "code": code }
        }),
        _ => json!({}),
    }
}

/// OpenAI Responses SSE: uses `event:` lines for delta types and
/// `data:` for the payload. Trailing sentinel is `data: [DONE]\n\n`.
///
/// The OpenAI Responses wire format uses flat JSON objects
/// (`{"type": "response.output_text.delta", "delta": "..."}`),
/// not the Chat Completions `choices[0].delta` shape.
pub fn encode_openai_responses_sse(event: &StreamEvent) -> String {
    let payload = openai_responses_payload(event);
    // The `event:` line must agree with the payload's own `type` field
    // so SSE consumers that route by either signal stay consistent.
    let event_name = match event {
        StreamEvent::Start { .. } => "response.created",
        StreamEvent::TextDelta { .. } => "response.output_text.delta",
        StreamEvent::ReasoningDelta { .. } => "response.reasoning_text.delta",
        StreamEvent::ToolCallStart { .. } => "response.output_item.added",
        StreamEvent::ToolCallDelta { .. } => "response.function_call_arguments.delta",
        StreamEvent::ToolCallEnd { .. } => "response.output_item.done",
        StreamEvent::Finish { .. } => "response.completed",
        StreamEvent::Error { .. } => "error",
        _ => return format!("data: {}\n\n", payload),
    };
    format!("event: {}\ndata: {}\n\n", event_name, payload)
}

fn openai_responses_payload(event: &StreamEvent) -> serde_json::Value {
    use serde_json::json;
    match event {
        StreamEvent::Start { id, model } => json!({
            "type": "response.created",
            "response": { "id": id, "model": model }
        }),
        StreamEvent::TextDelta { text } => json!({
            "type": "response.output_text.delta",
            "delta": text
        }),
        StreamEvent::ReasoningDelta { text } => json!({
            "type": "response.reasoning_text.delta",
            "delta": text
        }),
        StreamEvent::ToolCallStart { call } => json!({
            "type": "response.output_item.added",
            "item": {
                "type": "function_call",
                "id": call.id,
                "name": call.name,
                "arguments": serde_json::to_string(&call.arguments).unwrap_or_default(),
            }
        }),
        StreamEvent::ToolCallDelta {
            id,
            arguments_fragment,
        } => json!({
            "type": "response.function_call_arguments.delta",
            "item_id": id,
            "delta": arguments_fragment,
        }),
        StreamEvent::ToolCallEnd { id } => json!({
            "type": "response.output_item.done",
            "item": { "type": "function_call", "id": id }
        }),
        StreamEvent::Finish { reason, usage } => {
            let status = match reason {
                FinishReason::Stop => "completed",
                FinishReason::Length => "incomplete",
                _ => "completed",
            };
            let mut body = json!({
                "type": "response.completed",
                "response": { "status": status }
            });
            if let Some(u) = usage {
                body["response"]["usage"] = json!({
                    "input_tokens": u.tokens.input.unwrap_or(0),
                    "output_tokens": u.tokens.output.unwrap_or(0),
                });
            }
            body
        }
        StreamEvent::UsageDelta { .. } => json!({}),
        StreamEvent::Error { message, code } => json!({
            "type": "error",
            "error": { "message": message, "code": code }
        }),
        _ => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autorouter_core::{FinishReason, TokenBreakdown, ToolCall, Usage};

    fn text_event(s: &str) -> StreamEvent {
        StreamEvent::TextDelta {
            text: s.to_string(),
        }
    }

    fn finish_event(reason: FinishReason, usage: Option<Usage>) -> StreamEvent {
        StreamEvent::Finish { reason, usage }
    }

    fn reasoning_event(s: &str) -> StreamEvent {
        StreamEvent::ReasoningDelta {
            text: s.to_string(),
        }
    }

    // ── format_sse_chunk ──────────────────────────────────────────

    #[test]
    fn format_sse_chunk_empty_returns_none() {
        assert!(format_sse_chunk(&[]).unwrap().is_none());
    }

    #[test]
    fn format_sse_chunk_single_event() {
        let events = vec![text_event("hello")];
        let result = format_sse_chunk(&events).unwrap().unwrap();
        assert!(result.starts_with("data: "));
        assert!(result.ends_with("\n\n"));
        // Single events are serialised as the event itself (not wrapped in {"events":…})
        let parsed: serde_json::Value = serde_json::from_str(&result[6..].trim()).unwrap();
        assert_eq!(parsed["kind"], "text_delta");
    }

    #[test]
    fn format_sse_chunk_multi_event_batch() {
        let events = vec![text_event("a"), text_event("b")];
        let result = format_sse_chunk(&events).unwrap().unwrap();
        assert!(result.starts_with("data: "));
        assert!(result.ends_with("\n\n"));
        let parsed: serde_json::Value = serde_json::from_str(&result[6..].trim()).unwrap();
        // Multi-event batches are wrapped in {"events":[…]}
        assert!(parsed.get("events").is_some());
    }

    // ── Sentinel helpers ─────────────────────────────────────────

    #[test]
    fn done_sentinel_format() {
        assert_eq!(format_done_sentinel(), "data: [DONE]\n\n");
    }

    #[test]
    fn anthropic_stop_sentinel_format() {
        assert_eq!(
            format_anthropic_stop_sentinel(),
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
    }

    #[test]
    fn openai_responses_done_format() {
        assert_eq!(format_openai_responses_done(), "data: [DONE]\n\n");
    }

    // ── encode_openai_sse — all event shapes ────────────────────────

    #[test]
    fn openai_sse_start() {
        let event = StreamEvent::Start {
            id: "chatcmpl-abc".into(),
            model: "gpt-4".into(),
        };
        let s = encode_openai_sse(&event);
        assert!(s.starts_with("data: "));
        assert!(s.ends_with("\n\n"));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["id"], "chatcmpl-abc");
        assert_eq!(v["model"], "gpt-4");
        assert_eq!(v["object"], "chat.completion.chunk");
    }

    #[test]
    fn openai_sse_text_delta() {
        let s = encode_openai_sse(&text_event("Hello"));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["choices"][0]["delta"]["content"], "Hello");
    }

    #[test]
    fn openai_sse_reasoning_delta() {
        let event = StreamEvent::ReasoningDelta {
            text: "thinking...".into(),
        };
        let s = encode_openai_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["choices"][0]["delta"]["reasoning_content"], "thinking...");
    }

    #[test]
    fn openai_sse_tool_call_start() {
        let event = StreamEvent::ToolCallStart {
            call: ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                arguments: serde_json::json!({"city": "NYC"}),
            },
        };
        let s = encode_openai_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        let tc = &v["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["function"]["name"], "get_weather");
    }

    #[test]
    fn openai_sse_tool_call_delta() {
        let event = StreamEvent::ToolCallDelta {
            id: "call_1".into(),
            arguments_fragment: "{\"city\":".into(),
        };
        let s = encode_openai_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(
            v["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":"
        );
    }

    #[test]
    fn openai_sse_tool_call_end() {
        let event = StreamEvent::ToolCallEnd {
            id: "call_1".into(),
        };
        let s = encode_openai_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert!(v["choices"][0]["delta"]["tool_calls"][0]["index"].is_number());
    }

    #[test]
    fn openai_sse_finish_stop() {
        let s = encode_openai_sse(&finish_event(FinishReason::Stop, None));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn openai_sse_finish_with_usage() {
        let usage = Usage {
            tokens: TokenBreakdown {
                input: Some(10),
                output: Some(20),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = encode_openai_sse(&finish_event(FinishReason::Length, Some(usage)));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "length");
        assert_eq!(v["usage"]["prompt_tokens"], 10);
        assert_eq!(v["usage"]["completion_tokens"], 20);
        assert_eq!(v["usage"]["total_tokens"], 30);
    }

    #[test]
    fn openai_sse_error() {
        let event = StreamEvent::Error {
            message: "bad key".into(),
            code: Some("invalid_api_key".into()),
        };
        let s = encode_openai_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["error"]["message"], "bad key");
        assert_eq!(v["error"]["code"], "invalid_api_key");
    }

    #[test]
    fn openai_sse_usage_delta() {
        let event = StreamEvent::UsageDelta {
            usage: Usage::default(),
        };
        let s = encode_openai_sse(&event);
        // UsageDelta emits an empty JSON object
        assert_eq!(s, "data: {}\n\n");
    }

    // ── encode_anthropic_sse — all event shapes ─────────────────────

    #[test]
    fn anthropic_sse_start() {
        let event = StreamEvent::Start {
            id: "msg_abc".into(),
            model: "claude-3".into(),
        };
        let s = encode_anthropic_sse(&event);
        assert!(s.starts_with("event: message_start\ndata: "));
        assert!(s.ends_with("\n\n"));
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "message_start");
        assert_eq!(v["message"]["id"], "msg_abc");
    }

    #[test]
    fn anthropic_sse_text_delta() {
        let s = encode_anthropic_sse(&text_event("Hi"));
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "content_block_delta");
        assert_eq!(v["delta"]["type"], "text_delta");
        assert_eq!(v["delta"]["text"], "Hi");
    }

    #[test]
    fn anthropic_sse_reasoning_delta() {
        let event = StreamEvent::ReasoningDelta {
            text: "thinking".into(),
        };
        let s = encode_anthropic_sse(&event);
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["delta"]["type"], "thinking_delta");
        assert_eq!(v["delta"]["thinking"], "thinking");
    }

    #[test]
    fn anthropic_sse_tool_call_start() {
        let event = StreamEvent::ToolCallStart {
            call: ToolCall {
                id: "toolu_1".into(),
                name: "search".into(),
                arguments: serde_json::json!({"q": "test"}),
            },
        };
        let s = encode_anthropic_sse(&event);
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "content_block_start");
        assert_eq!(v["content_block"]["type"], "tool_use");
        assert_eq!(v["content_block"]["id"], "toolu_1");
    }

    #[test]
    fn anthropic_sse_tool_call_delta() {
        let event = StreamEvent::ToolCallDelta {
            id: "toolu_1".into(),
            arguments_fragment: "{\"q\":".into(),
        };
        let s = encode_anthropic_sse(&event);
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["delta"]["type"], "input_json_delta");
        assert_eq!(v["delta"]["partial_json"], "{\"q\":");
    }

    #[test]
    fn anthropic_sse_tool_call_end() {
        let event = StreamEvent::ToolCallEnd {
            id: "toolu_1".into(),
        };
        let s = encode_anthropic_sse(&event);
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "content_block_stop");
    }

    #[test]
    fn anthropic_sse_finish_stop() {
        let s = encode_anthropic_sse(&finish_event(FinishReason::Stop, None));
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "message_delta");
        assert_eq!(v["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn anthropic_sse_finish_tool_calls() {
        let s = encode_anthropic_sse(&finish_event(FinishReason::ToolCalls, None));
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn anthropic_sse_error() {
        let event = StreamEvent::Error {
            message: "rate limit".into(),
            code: None,
        };
        let s = encode_anthropic_sse(&event);
        assert!(s.starts_with("event: error\ndata: "));
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["error"]["message"], "rate limit");
    }

    #[test]
    fn anthropic_sse_with_index() {
        let s = encode_anthropic_sse_with_index(&text_event("idx-test"), 2);
        let v: serde_json::Value = serde_json::from_str(extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["index"], 2);
    }

    // ── encode_gemini_sse — all event shapes ────────────────────────

    #[test]
    fn gemini_sse_start() {
        let event = StreamEvent::Start {
            id: "g-id".into(),
            model: "gemini-2".into(),
        };
        let s = encode_gemini_sse(&event);
        assert!(s.starts_with("data: "));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["responseId"], "g-id");
    }

    #[test]
    fn gemini_sse_text_delta() {
        let s = encode_gemini_sse(&text_event("Hello"));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["candidates"][0]["content"]["parts"][0]["text"], "Hello");
        assert!(v["candidates"][0]["content"]["parts"][0]
            .get("thought")
            .is_none());
    }

    #[test]
    fn gemini_sse_reasoning_delta() {
        let event = StreamEvent::ReasoningDelta {
            text: "thinking".into(),
        };
        let s = encode_gemini_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["text"],
            "thinking"
        );
        assert_eq!(v["candidates"][0]["content"]["parts"][0]["thought"], true);
    }

    #[test]
    fn gemini_sse_tool_call_start() {
        let event = StreamEvent::ToolCallStart {
            call: ToolCall {
                id: "gc_1".into(),
                name: "fn".into(),
                arguments: serde_json::json!({"x": 1}),
            },
        };
        let s = encode_gemini_sse(&event);
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        let fc = &v["candidates"][0]["content"]["parts"][0]["functionCall"];
        assert_eq!(fc["name"], "fn");
        assert_eq!(fc["id"], "gc_1");
    }

    #[test]
    fn gemini_sse_finish_with_usage() {
        let usage = Usage {
            tokens: TokenBreakdown {
                input: Some(5),
                output: Some(15),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = encode_gemini_sse(&finish_event(FinishReason::Stop, Some(usage)));
        let v: serde_json::Value = serde_json::from_str(&s[6..].trim()).unwrap();
        assert_eq!(v["candidates"][0]["finishReason"], "STOP");
        assert_eq!(v["usageMetadata"]["promptTokenCount"], 5);
        assert_eq!(v["usageMetadata"]["candidatesTokenCount"], 15);
    }

    #[test]
    fn gemini_sse_usage_delta() {
        let event = StreamEvent::UsageDelta {
            usage: Usage::default(),
        };
        assert_eq!(encode_gemini_sse(&event), "data: {}\n\n");
    }

    #[test]
    fn gemini_sse_error() {
        let event = StreamEvent::Error {
            message: "err".into(),
            code: None,
        };
        let v: serde_json::Value =
            serde_json::from_str(&encode_gemini_sse(&event)[6..].trim()).unwrap();
        assert_eq!(v["error"]["message"], "err");
    }

    // ── encode_openai_responses_sse ─────────────────────────────────

    #[test]
    fn openai_responses_sse_text_delta() {
        let s = encode_openai_responses_sse(&text_event("hi"));
        assert_eq!(&s[..7], "event: ");
        assert!(s.contains("response.output_text.delta"));
        let v: serde_json::Value = serde_json::from_str(&extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "response.output_text.delta");
        assert_eq!(v["delta"], "hi");
    }

    #[test]
    fn openai_responses_sse_finish() {
        let s = encode_openai_responses_sse(&finish_event(FinishReason::Stop, None));
        assert!(s.contains("response.completed"));
    }

    #[test]
    fn openai_responses_sse_reasoning_delta() {
        let s = encode_openai_responses_sse(&reasoning_event("thinking..."));
        assert!(
            s.contains("response.reasoning_text.delta"),
            "ReasoningDelta must use response.reasoning_text.delta, got: {s}"
        );
        assert!(
            !s.contains("response.output_text.delta"),
            "ReasoningDelta must not be routed to the output channel"
        );
    }

    #[test]
    fn openai_responses_sse_start_event() {
        let event = StreamEvent::Start {
            id: "rsp_1".into(),
            model: "gpt-4o".into(),
        };
        let s = encode_openai_responses_sse(&event);
        // Start carries a `response.created` event line that matches its
        // payload `type`, so SSE consumers routing by either signal agree.
        assert!(s.contains("event: response.created\n"), "{}", s);
        let v: serde_json::Value = serde_json::from_str(&extract_data(&s).unwrap()).unwrap();
        assert_eq!(v["type"], "response.created");
        assert_eq!(v["response"]["id"], "rsp_1");
    }

    /// Extract the `data:` portion from an SSE frame.
    /// Works for both `data: <json>\n\n` and `event: X\ndata: <json>\n\n`.
    fn extract_data(sse: &str) -> Option<&str> {
        let data_start = sse.find("data: ")?;
        let after_data = &sse[data_start + 6..];
        after_data.strip_suffix("\n\n")
    }
}
