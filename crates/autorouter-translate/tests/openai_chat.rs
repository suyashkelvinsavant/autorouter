//! End-to-end compatibility tests for the OpenAI Chat Completions adapter.

use autorouter_core::{
    ContentPart, FinishReason, Message, MessageRole, ProviderKind, StreamChunk, StreamEvent,
    ToolCall, ToolDefinition, UniversalRequest, UniversalResponse,
};
use autorouter_translate::{OpenAiChatAdapter, ProviderAdapter};

fn text_message(role: MessageRole, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentPart::Text {
            text: text.to_string(),
        }],
        name: None,
    }
}

#[test]
fn encodes_minimal_request() {
    let adapter = OpenAiChatAdapter::new();
    let request = UniversalRequest {
        model: "gpt-5".into(),
        messages: vec![text_message(MessageRole::User, "hi")],
        ..empty_request()
    };
    let body = adapter.encode_request(&request).unwrap();
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hi");
}

#[test]
fn decodes_minimal_response() {
    let adapter = OpenAiChatAdapter::new();
    let body = serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hello" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 7 }
    });
    let request = UniversalRequest {
        model: "gpt-5".into(),
        messages: vec![text_message(MessageRole::User, "hi")],
        ..empty_request()
    };
    let upstream = adapter
        .decode_response(&request, &body, 200)
        .expect("decode response");
    let response: UniversalResponse = upstream.response;
    assert_eq!(response.id, "chatcmpl-1");
    assert_eq!(response.message.text(), "hello");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert_eq!(response.usage.tokens.input, Some(5));
    assert_eq!(response.usage.tokens.output, Some(7));
}

#[test]
fn encodes_tool_call_request() {
    let adapter = OpenAiChatAdapter::new();
    let tool = ToolDefinition {
        name: "search".into(),
        description: Some("search docs".into()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
            "required": ["q"],
        }),
        strict: false,
    };
    let request = UniversalRequest {
        model: "gpt-5".into(),
        messages: vec![
            text_message(MessageRole::User, "search rust"),
            Message {
                role: MessageRole::Assistant,
                content: vec![ContentPart::ToolCall {
                    id: "call_1".into(),
                    name: "search".into(),
                    arguments: serde_json::json!({ "q": "rust" }),
                }],
                name: None,
            },
            Message {
                role: MessageRole::Tool,
                content: vec![ContentPart::ToolResult {
                    tool_call_id: "call_1".into(),
                    content: autorouter_core::ToolResultPayload::Text { text: "ok".into() },
                    is_error: false,
                }],
                name: None,
            },
        ],
        tools: vec![tool],
        ..empty_request()
    };
    let body = adapter.encode_request(&request).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "search");
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
    assert_eq!(body["messages"][2]["content"], "ok");
}

#[test]
fn decodes_tool_call_response() {
    let adapter = OpenAiChatAdapter::new();
    let body = serde_json::json!({
        "id": "x",
        "model": "gpt-5",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "search",
                        "arguments": "{\"q\":\"rust\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let request = empty_request();
    let upstream = adapter.decode_response(&request, &body, 200).unwrap();
    assert_eq!(upstream.response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(upstream.response.tool_calls.len(), 1);
    let ToolCall {
        id,
        name,
        arguments,
    } = &upstream.response.tool_calls[0];
    assert_eq!(id, "call_1");
    assert_eq!(name, "search");
    assert_eq!(arguments, &serde_json::json!({ "q": "rust" }));
}

#[test]
fn decodes_sse_chunk() {
    let adapter = OpenAiChatAdapter::new();
    let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}},{\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), chunk)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "Hel")));
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "lo")));
    assert!(events.iter().any(|e| matches!(
        e,
        StreamEvent::Finish {
            reason: FinishReason::Stop,
            ..
        }
    )));
}

#[test]
fn round_trip_text_response() {
    let adapter = OpenAiChatAdapter::new();
    let original = UniversalRequest {
        model: "gpt-5".into(),
        messages: vec![
            text_message(MessageRole::System, "be helpful"),
            text_message(MessageRole::User, "hi"),
        ],
        ..empty_request()
    };
    let body = adapter.encode_request(&original).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
}

fn empty_request() -> UniversalRequest {
    UniversalRequest {
        model: String::new(),
        system: None,
        messages: Vec::new(),
        tool_choice: None,
        metadata: serde_json::Value::Null,
        tools: Vec::new(),
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        stop: Vec::new(),
        stream: false,
        extra: serde_json::Value::Null,
        user: None,
        prior_usage: Default::default(),
        ..Default::default()
    }
}

#[test]
fn decode_openai_chat_streaming_extracts_reasoning_content() {
    let adapter = OpenAiChatAdapter::new();
    // Single SSE chunk carrying reasoning_content on the delta.
    let chunk = r#"data: {"choices":[{"delta":{"reasoning_content":"let me think..."}}]}"#;
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), chunk)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ReasoningDelta { text } => assert_eq!(text, "let me think..."),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn decode_openai_chat_streaming_ignores_empty_reasoning_content() {
    // An empty `reasoning_content` must NOT emit a reasoning delta —
    // some upstreams send `""` on every chunk and emitting an empty
    // stream event would confuse the SSE consumer.
    let adapter = OpenAiChatAdapter::new();
    let chunk = r#"data: {"choices":[{"delta":{"reasoning_content":""}}]}"#;
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), chunk)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert!(
        events.is_empty(),
        "empty reasoning_content must not emit any event, got {events:?}"
    );
}

#[test]
fn decode_openai_chat_response_extracts_reasoning_content() {
    let adapter = OpenAiChatAdapter::new();
    let body = serde_json::json!({
        "id": "chatcmpl-r1",
        "object": "chat.completion",
        "model": "o3-mini",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "the answer is 4",
                "reasoning_content": "thinking hard"
            },
            "finish_reason": "stop"
        }]
    });
    let upstream = adapter
        .decode_response(&empty_request(), &body, 200)
        .expect("decode response");
    let response = upstream.response;
    // Both content parts must be preserved. The decoder emits them
    // in JSON-field order (content first, reasoning_content second),
    // matching the source payload; we assert on presence rather
    // than a fixed position.
    assert_eq!(response.message.content.len(), 2);
    let has_reasoning = response
        .message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Reasoning { text } if text == "thinking hard"));
    let has_text = response
        .message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Text { text } if text == "the answer is 4"));
    assert!(
        has_reasoning,
        "expected Reasoning part, got {:?}",
        response.message.content
    );
    assert!(
        has_text,
        "expected Text part, got {:?}",
        response.message.content
    );
}

#[test]
fn identity_provider() {
    let adapter = OpenAiChatAdapter::new();
    assert_eq!(adapter.kind(), ProviderKind::OpenAI);
}

// ---------------------------------------------------------------------------
// Inline `<!--reasoning-->...<!--/reasoning-->` tag handling
// ---------------------------------------------------------------------------

fn empty_request2() -> UniversalRequest {
    empty_request()
}

#[test]
fn inline_reasoning_tag_in_non_streaming_content_becomes_reasoning_part() {
    let adapter = OpenAiChatAdapter::new();
    // Build the payload by concatenation to dodge a tooling quirk that
    // strips `/` from `<!--reasoning-->` when followed by a quote.
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let content = format!("Hello {opener}chain of thought{closer} world");
    let body = serde_json::json!({
        "id": "chatcmpl-1",
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    });
    let upstream = adapter
        .decode_response(&empty_request2(), &body, 200)
        .expect("decode response");
    let response = upstream.response;

    // Reasoning text moved out of `content`.
    let has_reasoning = response
        .message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Reasoning { text } if text == "chain of thought"));
    assert!(
        has_reasoning,
        "expected Reasoning part, got {:?}",
        response.message.content
    );

    // Surrounding text preserved.
    let text: String = response
        .message
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(
        text, "Hello  world",
        "expected stripped text, got {:?}",
        text
    );

    // No literal tag characters leaked into the response.
    let raw: String = response
        .message
        .content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Reasoning { text } => text.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(!raw.contains("!--"), "tag leak: {:?}", raw);
}

#[test]
fn inline_reasoning_tag_in_streaming_delta_becomes_reasoning_delta() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiChatAdapter::new();
    let request = empty_request2();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let payload = format!(
        r#"data: {{"id":"x","choices":[{{"index":0,"delta":{{"content":"hi {opener}thoughts{closer} bye"}}}}]}}"#
    );
    let chunks = adapter
        .decode_stream_chunk(&request, payload.as_str())
        .expect("decode");
    let mut texts: Vec<String> = Vec::new();
    let mut reasonings: Vec<String> = Vec::new();
    for c in chunks {
        for e in c.events {
            match e {
                StreamEvent::TextDelta { text } => texts.push(text),
                StreamEvent::ReasoningDelta { text } => reasonings.push(text),
                _ => {}
            }
        }
    }
    let text: String = texts.concat();
    let reasoning: String = reasonings.concat();
    assert_eq!(text, "hi  bye");
    assert_eq!(reasoning, "thoughts");
}

#[test]
fn inline_reasoning_tag_split_across_stream_chunks() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiChatAdapter::new();
    let request = empty_request2();
    // The opener is split across two chunks; the closer is in the third;
    // trailing text is in the fourth.
    let opener_full = String::from("<") + "!--reasoning-->";
    let closer_full = String::from("<") + "!--/reasoning-->";
    let mid_idx = 7; // "<!- -reasoning-->"
    let opener_a: String = opener_full.chars().take(mid_idx).collect();
    let opener_b: String = opener_full.chars().skip(mid_idx).collect();
    let body_a =
        format!(r#"data: {{"choices":[{{"index":0,"delta":{{"content":"x{opener_a}"}}}}]}}"#);
    let body_b = format!(
        r#"data: {{"choices":[{{"index":0,"delta":{{"content":"{opener_b}thinking"}}}}]}}"#
    );
    let body_c =
        format!(r#"data: {{"choices":[{{"index":0,"delta":{{"content":"{closer_full}"}}}}]}}"#);
    let body_d = r#"data: {"choices":[{"index":0,"delta":{"content":" final"}}]}"#.to_string();
    let body_e = r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#.to_string();

    let mut texts: Vec<String> = Vec::new();
    let mut reasonings: Vec<String> = Vec::new();
    for raw in [body_a, body_b, body_c, body_d, body_e] {
        let chunks = adapter
            .decode_stream_chunk(&request, raw.as_str())
            .expect("decode");
        for c in chunks {
            for e in c.events {
                match e {
                    StreamEvent::TextDelta { text } => texts.push(text),
                    StreamEvent::ReasoningDelta { text } => reasonings.push(text),
                    _ => {}
                }
            }
        }
    }
    let text: String = texts.concat();
    let reasoning: String = reasonings.concat();
    assert_eq!(text, "x final");
    assert_eq!(reasoning, "thinking");
}

/// When the upstream truncates mid-thought (max_tokens hit while
/// the model is still inside `<think>...</think>`), the opener is
/// never closed. The streamer's flush on `[DONE]` must classify
/// the dangling body as Reasoning so the user can see what the
/// model was trying to say. Mirrors the user's 2026-06-23
/// symptom (response cut off mid-thinking, finish_reason: length).
#[test]
fn think_tag_unclosed_at_done_flushes_as_reasoning() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiChatAdapter::new();
    let request = empty_request2();
    let body = r#"data: {"choices":[{"index":0,"delta":{"content":"<think>The user asked me to reply with exactly PONG"}}]}"#;
    // Collect events from BOTH the content chunk AND the [DONE] frame.
    // The streamer holds back 7 chars (close tag - 1) as carry to
    // detect a possible mid-chunk close tag, so the body is split
    // between the two frames: first frame emits the first 37 chars
    // of the body, [DONE] flushes the remaining 7 ("ly PONG") and
    // the Finish event.
    let mut reasonings: Vec<String> = Vec::new();
    let mut saw_finish = false;

    let first = adapter.decode_stream_chunk(&request, body).unwrap();
    for c in first {
        for e in c.events {
            if let StreamEvent::ReasoningDelta { text } = e {
                reasonings.push(text);
            }
        }
    }

    let done = adapter
        .decode_stream_chunk(&request, "data: [DONE]\n\n")
        .expect("decode done");
    for c in done {
        for e in c.events {
            match e {
                StreamEvent::ReasoningDelta { text } => reasonings.push(text),
                StreamEvent::Finish { .. } => saw_finish = true,
                _ => {}
            }
        }
    }
    let reasoning: String = reasonings.concat();
    assert_eq!(
        reasoning, "The user asked me to reply with exactly PONG",
        "unclosed think block should be flushed as reasoning on [DONE]"
    );
    assert!(saw_finish, "expected Finish after [DONE]");
}

/// Companion test using the HTML-comment variant `<!--reasoning-->`.
#[test]
fn streaming_done_drains_pending_reasoning_before_finish() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiChatAdapter::new();
    let request = empty_request2();
    let opener = String::from("<") + "!--reasoning-->";
    // Send an unclosed reasoning block, then [DONE].
    let body = format!(
        r#"data: {{"choices":[{{"index":0,"delta":{{"content":"{opener}dangling thought"}}}}]}}"#
    );
    let _ = adapter
        .decode_stream_chunk(&request, body.as_str())
        .unwrap();
    let mut texts: Vec<String> = Vec::new();
    let mut reasonings: Vec<String> = Vec::new();
    let mut saw_finish = false;
    let done = adapter
        .decode_stream_chunk(&request, "data: [DONE]\n\n")
        .expect("decode done");
    for c in done {
        for e in c.events {
            match e {
                StreamEvent::TextDelta { text } => texts.push(text),
                StreamEvent::ReasoningDelta { text } => reasonings.push(text),
                StreamEvent::Finish { .. } => saw_finish = true,
                _ => {}
            }
        }
    }
    let reasoning: String = reasonings.concat();
    assert_eq!(reasoning, "dangling thought");
    assert!(saw_finish, "expected Finish after [DONE]");
}

#[test]
fn think_tag_in_non_streaming_content_becomes_reasoning_part() {
    let adapter = OpenAiChatAdapter::new();
    // The <think>...</think> shape (DeepSeek / OSS models).
    let body = serde_json::json!({
        "id": "chatcmpl-2",
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "<think>chain of thought</think>PONG"
            },
            "finish_reason": "stop"
        }]
    });
    let upstream = adapter
        .decode_response(&empty_request2(), &body, 200)
        .expect("decode");
    let response = upstream.response;
    let has_reasoning = response
        .message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Reasoning { text } if text == "chain of thought"));
    assert!(
        has_reasoning,
        "expected Reasoning part, got {:?}",
        response.message.content
    );
    let visible: String = response
        .message
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(visible, "PONG");
}

#[test]
fn think_tag_in_streaming_delta_becomes_reasoning_delta() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiChatAdapter::new();
    let payload = r#"data: {"id":"x","choices":[{"index":0,"delta":{"content":"<think>thoughts</think>bye"}}]}"#;
    let chunks = adapter
        .decode_stream_chunk(&empty_request2(), payload)
        .expect("decode");
    let mut texts: Vec<String> = Vec::new();
    let mut reasonings: Vec<String> = Vec::new();
    for c in chunks {
        for e in c.events {
            match e {
                StreamEvent::TextDelta { text } => texts.push(text),
                StreamEvent::ReasoningDelta { text } => reasonings.push(text),
                _ => {}
            }
        }
    }
    assert_eq!(texts.concat(), "bye");
    assert_eq!(reasonings.concat(), "thoughts");
}

#[test]
fn think_tag_split_across_stream_chunks_reassembles_correctly() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiChatAdapter::new();
    let request = empty_request2();
    // Chunk A ends mid-opener (`<th`). Chunk B carries the rest of
    // the opener + the first half of the body. Chunk C finishes the
    // body + closes the tag. Chunk D is the visible answer.
    let chunk_a = r#"data: {"choices":[{"index":0,"delta":{"content":"<th"}}]}"#;
    let chunk_b = r#"data: {"choices":[{"index":0,"delta":{"content":"ink>The user just"}}]}"#;
    let chunk_c = r#"data: {"choices":[{"index":0,"delta":{"content":" wants a friendly reply.\n</think>\nHi there!"}}]}"#;
    let chunk_d = r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;

    let mut texts: Vec<String> = Vec::new();
    let mut reasonings: Vec<String> = Vec::new();
    for raw in [chunk_a, chunk_b, chunk_c, chunk_d] {
        let chunks = adapter.decode_stream_chunk(&request, raw).expect("decode");
        for c in chunks {
            for e in c.events {
                match e {
                    StreamEvent::TextDelta { text } => texts.push(text),
                    StreamEvent::ReasoningDelta { text } => reasonings.push(text),
                    _ => {}
                }
            }
        }
    }
    let visible: String = texts.concat();
    let reasoning: String = reasonings.concat();
    // The streamer should absorb the partial opener from chunk A
    // (no visible text until the opener is complete), then emit
    // the body as reasoning (including the trailing newline before
    // `</think>`), then the trailing visible text (including the
    // newline between `</think>` and the answer).
    assert_eq!(reasoning, "The user just wants a friendly reply.\n");
    assert_eq!(visible, "\nHi there!");
    // No literal tag characters should leak into either channel.
    assert!(
        !reasoning.contains("<"),
        "reasoning leaked opener: {reasoning:?}"
    );
    assert!(
        !visible.contains("<think>"),
        "visible leaked opener: {visible:?}"
    );
    assert!(
        !visible.contains("</think>"),
        "visible leaked closer: {visible:?}"
    );
}
