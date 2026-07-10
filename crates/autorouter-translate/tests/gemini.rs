//! End-to-end compatibility tests for the Gemini generateContent adapter.

use autorouter_core::{
    ContentPart, FinishReason, Message, ProviderKind, StreamChunk, StreamEvent, UniversalRequest,
};
use autorouter_translate::{GeminiAdapter, ProviderAdapter};

#[test]
fn encodes_contents() {
    let adapter = GeminiAdapter::new();
    let request = UniversalRequest {
        model: "gemini-2.5-pro".into(),
        messages: vec![Message::system("be terse"), Message::user("hi")],
        ..empty_request()
    };
    let body = adapter.encode_request(&request).unwrap();
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be terse");
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
}

#[test]
fn decodes_text_response() {
    let adapter = GeminiAdapter::new();
    let body = serde_json::json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "hi back" }] },
            "finishReason": "STOP"
        }],
        "modelVersion": "gemini-2.5-pro",
        "responseId": "resp_1",
        "usageMetadata": { "promptTokenCount": 2, "candidatesTokenCount": 3 }
    });
    let resp = adapter
        .decode_response(&empty_request(), &body, 200)
        .unwrap()
        .response;
    assert_eq!(resp.message.text(), "hi back");
    assert_eq!(resp.finish_reason, FinishReason::Stop);
    assert_eq!(resp.usage.tokens.input, Some(2));
    assert_eq!(resp.usage.tokens.output, Some(3));
}

#[test]
fn decodes_streaming_json_array() {
    let adapter = GeminiAdapter::new();
    // Gemini SSE returns JSON arrays; the adapter accepts both `data: {...}`
    // and raw JSON objects.
    let body = serde_json::json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "streamed" }] }
        }]
    });
    let line = format!("data: {}", body);
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), &line)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "streamed")));
}

#[test]
fn identity_provider() {
    assert_eq!(GeminiAdapter::new().kind(), ProviderKind::Gemini);
}

#[test]
fn decode_gemini_streaming_extracts_thought_part() {
    let adapter = GeminiAdapter::new();
    // Gemini SSE chunk with a single `thought: true` text part.
    // Must decode into StreamEvent::ReasoningDelta (NOT TextDelta)
    // so the SSE encoders can route it correctly.
    let body = serde_json::json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "deep thought", "thought": true }] }
        }]
    });
    let line = format!("data: {}", body);
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), &line)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert_eq!(
        events.len(),
        1,
        "expected exactly one event, got {events:?}"
    );
    match &events[0] {
        StreamEvent::ReasoningDelta { text } => assert_eq!(text, "deep thought"),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn decode_gemini_streaming_thought_false_is_text_delta() {
    // Sanity: a regular part (no `thought`) still emits TextDelta.
    let adapter = GeminiAdapter::new();
    let body = serde_json::json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": "plain" }] }
        }]
    });
    let line = format!("data: {}", body);
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), &line)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert!(matches!(
        &events[0],
        StreamEvent::TextDelta { text } if text == "plain"
    ));
}

#[test]
fn decode_gemini_response_extracts_thought_part() {
    let adapter = GeminiAdapter::new();
    // Gemini response with both a thought part and a plain text part
    // in the same candidate. Both must surface as ContentPart on the
    // universal message.
    let body = serde_json::json!({
        "responseId": "resp_g1",
        "modelVersion": "gemini-2.5-pro",
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    { "text": "let me think...", "thought": true },
                    { "text": "the answer is 42" }
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 20,
            "thoughtsTokenCount": 30
        }
    });
    let resp = adapter
        .decode_response(&empty_request(), &body, 200)
        .expect("decode response")
        .response;
    assert_eq!(resp.message.content.len(), 2);
    assert!(matches!(
        &resp.message.content[0],
        ContentPart::Reasoning { text } if text == "let me think..."
    ));
    assert!(matches!(
        &resp.message.content[1],
        ContentPart::Text { text } if text == "the answer is 42"
    ));
    // Reasoning tokens must be preserved in the usage block.
    assert_eq!(resp.usage.tokens.reasoning, Some(30));
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

// ---------------------------------------------------------------------------
// Inline `<!--reasoning-->...<!--/reasoning-->` tag handling
// ---------------------------------------------------------------------------

fn empty_request2() -> UniversalRequest {
    empty_request()
}

#[test]
fn inline_reasoning_tag_in_text_part_becomes_reasoning_part() {
    let adapter = GeminiAdapter::new();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let text = format!("Lead {opener}thoughts{closer} tail");
    let body = serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [{ "text": text }]
            },
            "finishReason": "STOP"
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

#[test]
fn inline_reasoning_tag_in_streaming_text_part_becomes_reasoning_delta() {
    let adapter = GeminiAdapter::new();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let payload = format!(
        r#"data: {{"candidates":[{{"content":{{"parts":[{{"text":"hi {opener}thoughts{closer} bye"}}]}}}}]}}"#
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
