//! End-to-end compatibility tests for the Anthropic Messages adapter.

use autorouter_core::{
    FinishReason, Message, ProviderKind, StreamChunk, StreamEvent, ToolDefinition, UniversalRequest,
};
use autorouter_translate::{AnthropicAdapter, ProviderAdapter};

#[test]
fn encodes_system_as_top_level() {
    let adapter = AnthropicAdapter::new();
    let request = UniversalRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![Message::system("be brief"), Message::user("hi")],
        max_output_tokens: Some(1024),
        ..empty_request()
    };
    let body = adapter.encode_request(&request).unwrap();
    assert_eq!(body["system"], "be brief");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["max_tokens"], 1024);
}

#[test]
fn encodes_tool_use() {
    let adapter = AnthropicAdapter::new();
    let request = UniversalRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![Message::user("search rust")],
        tools: vec![ToolDefinition {
            name: "search".into(),
            description: Some("search docs".into()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" } }
            }),
            strict: false,
        }],
        ..empty_request()
    };
    let body = adapter.encode_request(&request).unwrap();
    assert_eq!(body["tools"][0]["name"], "search");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
}

#[test]
fn decodes_text_response() {
    let adapter = AnthropicAdapter::new();
    let body = serde_json::json!({
        "id": "msg_1",
        "model": "claude-sonnet-4-5",
        "content": [{ "type": "text", "text": "hello" }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 3, "output_tokens": 4 }
    });
    let resp = adapter
        .decode_response(&empty_request(), &body, 200)
        .unwrap()
        .response;
    assert_eq!(resp.message.text(), "hello");
    assert_eq!(resp.finish_reason, FinishReason::Stop);
    assert_eq!(resp.usage.tokens.input, Some(3));
    assert_eq!(resp.usage.tokens.output, Some(4));
}

#[test]
fn decodes_sse_text_delta() {
    let adapter = AnthropicAdapter::new();
    let events_str = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-sonnet-4-5\"}}",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}",
    ].join("\n");
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), &events_str)
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
fn identity_provider() {
    assert_eq!(AnthropicAdapter::new().kind(), ProviderKind::Anthropic);
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
/// M3: ImageSource::Url must be rejected when targeting Anthropic
/// (which only accepts base64-encoded image bytes). The error must
/// be loud, not silent.
#[test]
fn rejects_image_url_for_anthropic() {
    use autorouter_core::{ContentPart, ImageSource};
    let adapter = AnthropicAdapter::new();
    let request = UniversalRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![
            Message::user("describe this"),
            Message {
                role: autorouter_core::MessageRole::User,
                content: vec![ContentPart::Image {
                    source: ImageSource::Url {
                        url: "https://example.com/cat.png".into(),
                    },
                    detail: None,
                }],
                name: None,
            },
        ],
        ..empty_request()
    };
    let err = adapter.encode_request(&request).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("image URL"), "got: {}", msg);
    assert!(msg.contains("Anthropic"), "got: {}", msg);
}

/// M3: ImageSource::FileId must be rejected when targeting Anthropic.
#[test]
fn rejects_image_file_id_for_anthropic() {
    use autorouter_core::{ContentPart, ImageSource};
    let adapter = AnthropicAdapter::new();
    let request = UniversalRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![Message {
            role: autorouter_core::MessageRole::User,
            content: vec![ContentPart::Image {
                source: ImageSource::FileId {
                    id: "file_abc".into(),
                },
                detail: None,
            }],
            name: None,
        }],
        ..empty_request()
    };
    let err = adapter.encode_request(&request).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("FileId"), "got: {}", msg);
}

// ---------------------------------------------------------------------------
// Inline `<!--reasoning-->...<!--/reasoning-->` tag handling
// ---------------------------------------------------------------------------

fn empty_request2() -> UniversalRequest {
    UniversalRequest {
        model: "claude-sonnet-4-5".into(),
        ..empty_request_min()
    }
}

fn empty_request_min() -> UniversalRequest {
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
fn inline_reasoning_tag_in_text_block_becomes_reasoning_part() {
    use autorouter_core::ContentPart;
    let adapter = AnthropicAdapter::new();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let text = format!("Lead {opener}thoughts{closer} tail");
    let body = serde_json::json!({
        "id": "msg_1",
        "model": "claude-sonnet-4-5",
        "stop_reason": "end_turn",
        "content": [{
            "type": "text",
            "text": text
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
        .any(|p| matches!(p, ContentPart::Reasoning { text } if text == "thoughts"));
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
    assert_eq!(visible, "Lead  tail");
}

/// Regression test for M11: Anthropic extended-thinking returns content
/// blocks of `type: "thinking"` (carrying the chain-of-thought in a
/// `thinking` field). These must surface as `ContentPart::Reasoning`
/// rather than being silently dropped by the catch-all arm.
#[test]
fn thinking_content_block_becomes_reasoning_part() {
    use autorouter_core::ContentPart;
    let adapter = AnthropicAdapter::new();
    let body = serde_json::json!({
        "id": "msg_1",
        "model": "claude-sonnet-4-5",
        "stop_reason": "end_turn",
        "content": [
            { "type": "thinking", "thinking": "deliberating...", "signature": "sig" },
            { "type": "text", "text": "answer" }
        ]
    });
    let upstream = adapter
        .decode_response(&empty_request2(), &body, 200)
        .expect("decode");
    let response = upstream.response;
    let has_reasoning = response
        .message
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::Reasoning { text } if text == "deliberating..."));
    assert!(
        has_reasoning,
        "expected thinking block to surface as Reasoning, got {:?}",
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
    assert_eq!(visible, "answer");
}

#[test]
fn inline_reasoning_tag_in_streaming_text_delta_becomes_reasoning_delta() {
    let adapter = AnthropicAdapter::new();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let payload = format!(
        "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"hi {opener}thoughts{closer} bye\"}}}}\n\n"
    );
    let chunks = adapter
        .decode_stream_chunk(&empty_request2(), payload.as_str())
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
    assert_eq!(texts.concat(), "hi  bye");
    assert_eq!(reasonings.concat(), "thoughts");
}

// ---------------------------------------------------------------------------
// Terminal-path reasoning streamer cleanup (regression tests).
// Some proxies send `message_stop` directly without a preceding
// `message_delta`; both sentinel paths must drain the per-request
// reasoning streamer so unclosed `<think>` blocks do not leak state.
// ---------------------------------------------------------------------------

#[test]
fn message_stop_drains_unclosed_reasoning_block() {
    // 1) Feed an unclosed `<think>`: the streamer holds the whole
    //    body in carry because it's waiting for `</think>`.
    let adapter = AnthropicAdapter::new();
    let req = empty_request2();
    let text_payload = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"<think>partial thoughts\"}}\n\n";
    let _ = adapter.decode_stream_chunk(&req, text_payload).unwrap();

    // 2) Send `message_stop` directly (no `message_delta` first —
    //    emulating a proxy that sends only the terminal sentinel).
    //    The streamer must be drained, emitting the held-back carry
    //    (the last `</think>`.len()-1 chars) as ReasoningDelta.
    let stop_payload = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let chunks = adapter.decode_stream_chunk(&req, stop_payload).unwrap();

    let mut all: Vec<StreamEvent> = Vec::new();
    for c in chunks {
        all.extend(c.events);
    }
    // The streamer holds back the trailing `</think>`.len()-1 chars of
    // the unclosed reasoning body in its carry buffer. On finish those
    // 7 chars ("houghts") surface as ReasoningDelta. The leak we guard
    // against would yield ZERO ReasoningDelta events because the
    // streamer entry would never be drained.
    let drained_reasoning = all
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(
        drained_reasoning.contains("houghts"),
        "message_stop must drain the held-back reasoning carry; got events: {all:?}"
    );
}
