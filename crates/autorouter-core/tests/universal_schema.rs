//! Round-trip tests for the universal schema.

use autorouter_core::{
    ContentPart, FinishReason, ImageSource, Message, MessageRole, ProviderKind, StreamChunk,
    StreamEvent, ToolCall, ToolDefinition, ToolResultBody, UniversalRequest, UniversalResponse,
    Usage,
};

#[test]
fn message_text_helper() {
    let m = Message::user("hello\nworld");
    assert_eq!(m.text(), "hello\nworld");
}

#[test]
fn message_concatenates_text_parts() {
    let m = Message {
        role: MessageRole::Assistant,
        content: vec![
            ContentPart::Text {
                text: "first".into(),
            },
            ContentPart::Text {
                text: "second".into(),
            },
        ],
        name: None,
    };
    assert_eq!(m.text(), "first\nsecond");
}

#[test]
fn usage_total() {
    let u = Usage {
        tokens: autorouter_core::TokenBreakdown {
            input: Some(10),
            output: Some(20),
            cache_read: Some(5),
            cache_write: None,
            reasoning: None,
        },
        cost_micro_cents: None,
    };
    assert_eq!(u.total_tokens(), 35);
}

#[test]
fn request_serialises_with_optional_fields_omitted() {
    let req = UniversalRequest {
        model: "gpt-5".into(),
        system: None,
        messages: vec![Message::user("hi")],
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
        prior_usage: Usage::default(),
        ..UniversalRequest::default()
    };
    let value = serde_json::to_value(&req).unwrap();
    let map = value.as_object().unwrap();
    for key in [
        "temperature",
        "top_p",
        "max_output_tokens",
        "stop",
        "user",
        "tools",
        "extra",
        "prior_usage",
    ] {
        assert!(!map.contains_key(key), "expected {key} to be skipped");
    }
    assert_eq!(map["model"], "gpt-5");
    assert!(map["messages"].is_array());
}

#[test]
fn stream_chunk_aggregates_events() {
    let chunk = StreamChunk::new(vec![
        StreamEvent::Start {
            id: "x".into(),
            model: "gpt-5".into(),
        },
        StreamEvent::TextDelta { text: "hi".into() },
        StreamEvent::Finish {
            reason: FinishReason::Stop,
            usage: None,
        },
    ]);
    assert_eq!(chunk.events.len(), 3);
}

#[test]
fn tool_definition_round_trip() {
    let t = ToolDefinition {
        name: "search".into(),
        description: Some("search".into()),
        parameters: serde_json::json!({ "type": "object" }),
        strict: true,
    };
    let v = serde_json::to_value(&t).unwrap();
    let back: ToolDefinition = serde_json::from_value(v).unwrap();
    assert_eq!(back.name, "search");
    assert!(back.strict);
}

#[test]
fn tool_call_round_trip() {
    let tc = ToolCall {
        id: "call_1".into(),
        name: "search".into(),
        arguments: serde_json::json!({ "q": "rust" }),
    };
    let v = serde_json::to_value(&tc).unwrap();
    let back: ToolCall = serde_json::from_value(v).unwrap();
    assert_eq!(back.arguments, serde_json::json!({ "q": "rust" }));
}

#[test]
fn image_source_preserved() {
    let src = ImageSource::Base64 {
        media_type: "image/png".into(),
        data: "BASE64".into(),
    };
    let v = serde_json::to_value(&src).unwrap();
    let back: ImageSource = serde_json::from_value(v).unwrap();
    assert_eq!(back, src);
}

#[test]
fn response_text_helper_concatenates_text_parts() {
    let resp = UniversalResponse {
        id: "r".into(),
        model: "gpt-5".into(),
        message: Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentPart::Text {
                    text: "line 1".into(),
                },
                ContentPart::ToolCall {
                    id: "c".into(),
                    name: "f".into(),
                    arguments: serde_json::json!({}),
                },
                ContentPart::Text {
                    text: "line 2".into(),
                },
            ],
            name: None,
        },
        tool_calls: vec![],
        finish_reason: FinishReason::ToolCalls,
        usage: Usage::default(),
        created_at: None,
    };
    assert_eq!(resp.text(), "line 1\nline 2");
    assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
}

#[test]
fn provider_kind_display() {
    assert_eq!(ProviderKind::OpenAI.to_string(), "openai");
    assert_eq!(ProviderKind::Anthropic.to_string(), "anthropic");
    assert_eq!(ProviderKind::Gemini.to_string(), "gemini");
    assert_eq!(ProviderKind::Custom.to_string(), "custom");
}

#[test]
fn tool_result_payload_text() {
    let p = ToolResultBody::Text { text: "ok".into() };
    let v = serde_json::to_value(&p).unwrap();
    let back: ToolResultBody = serde_json::from_value(v).unwrap();
    match back {
        ToolResultBody::Text { text } => assert_eq!(text, "ok"),
        _ => panic!("wrong variant"),
    }
}
