//! Pipeline-level tests: round-trip a request through two different
//! providers and verify the universal schema is the source of truth.

use std::sync::Arc;

use autorouter_core::{ProviderKind, UniversalRequest};
use autorouter_translate::{
    AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, OpenAiResponsesAdapter, TranslationPipeline,
};

fn pipeline() -> TranslationPipeline {
    TranslationPipeline::new()
        .register(Arc::new(OpenAiChatAdapter::new()))
        .register(Arc::new(OpenAiResponsesAdapter::new()))
        .register(Arc::new(AnthropicAdapter::new()))
        .register(Arc::new(GeminiAdapter::new()))
}

#[test]
fn pipeline_dispatches_by_provider_kind() {
    let p = pipeline();
    assert!(p.adapter_for(ProviderKind::OpenAI).is_ok());
    assert!(p.adapter_for(ProviderKind::Anthropic).is_ok());
    assert!(p.adapter_for(ProviderKind::Gemini).is_ok());
}

#[test]
fn pipeline_parses_openai_chat() {
    let p = pipeline();
    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [
            { "role": "system", "content": "be brief" },
            { "role": "user", "content": "hi" }
        ]
    });
    let req = p.parse_request(ProviderKind::OpenAI, &body).unwrap();
    assert_eq!(req.model, "gpt-5");
    assert_eq!(req.messages.len(), 2);
    assert!(matches!(
        req.messages[0].role,
        autorouter_core::MessageRole::System
    ));
}

#[test]
fn pipeline_serialises_to_anthropic() {
    let p = pipeline();
    let req = UniversalRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![
            autorouter_core::Message::system("be brief"),
            autorouter_core::Message::user("hi"),
        ],
        ..empty()
    };
    let body = p.serialise_request(ProviderKind::Anthropic, &req).unwrap();
    assert_eq!(body["system"], "be brief");
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn pipeline_rejects_unknown_provider() {
    let p = pipeline();
    let res = p.adapter_for(ProviderKind::Custom);
    assert!(res.is_err());
}

fn empty() -> UniversalRequest {
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
fn pipeline_custom_request_passthrough() {
    // Custom is intentionally a best-effort pass-through. The decoder
    // accepts a generic OpenAI-like body and preserves unknown fields
    // in `extra` so downstream serializers can re-emit them.
    let p = pipeline();
    let body = serde_json::json!({
        "model": "my-custom-model",
        "messages": [
            { "role": "system", "content": "be brief" },
            { "role": "user", "content": "hi" }
        ],
        "vendor_specific": { "trace_id": "abc-123" }
    });
    let req = p.parse_request(ProviderKind::Custom, &body).unwrap();
    assert_eq!(req.model, "my-custom-model");
    assert_eq!(req.messages.len(), 2);
    assert!(req.extra.get("vendor_specific").is_some());
}

/// M2: `/openai/v1/responses` uses a different wire format from
/// Chat Completions (`input` + `instructions` instead of
/// `messages`). Verify the pipeline decodes the Responses body into
/// the same universal schema the Chat path produces.
#[test]
fn pipeline_parses_openai_responses() {
    use autorouter_translate::OpenAiWireFormat;
    let p = pipeline();
    let body = serde_json::json!({
        "model": "gpt-5",
        "instructions": "be brief",
        "input": [
            { "role": "user", "content": [
                { "type": "input_text", "text": "hi" }
            ]}
        ],
        "max_output_tokens": 256,
        "stream": false
    });
    let req = p
        .parse_request_with_format(ProviderKind::OpenAI, OpenAiWireFormat::Responses, &body)
        .unwrap();
    assert_eq!(req.model, "gpt-5");
    assert_eq!(req.messages.len(), 2, "instructions + user message");
    assert!(matches!(
        req.messages[0].role,
        autorouter_core::MessageRole::System
    ));
    assert!(matches!(
        req.messages[1].role,
        autorouter_core::MessageRole::User
    ));
    assert_eq!(req.max_output_tokens, Some(256));
}

#[test]
fn pipeline_responses_and_chat_share_universal_shape() {
    use autorouter_translate::OpenAiWireFormat;
    let p = pipeline();
    let chat_body = serde_json::json!({
        "model": "gpt-5",
        "messages": [
            { "role": "user", "content": "hi" }
        ]
    });
    let responses_body = serde_json::json!({
        "model": "gpt-5",
        "input": [
            { "role": "user", "content": "hi" }
        ]
    });
    let chat_req = p
        .parse_request_with_format(
            ProviderKind::OpenAI,
            OpenAiWireFormat::ChatCompletions,
            &chat_body,
        )
        .unwrap();
    let responses_req = p
        .parse_request_with_format(
            ProviderKind::OpenAI,
            OpenAiWireFormat::Responses,
            &responses_body,
        )
        .unwrap();
    assert_eq!(chat_req.model, responses_req.model);
    assert_eq!(chat_req.messages.len(), responses_req.messages.len());
    assert!(matches!(
        chat_req.messages[0].role,
        autorouter_core::MessageRole::User
    ));
    assert!(matches!(
        responses_req.messages[0].role,
        autorouter_core::MessageRole::User
    ));
}

#[test]
fn anthropic_system_block_list_extracts_text_segments() {
    // gap #4: the Anthropic system parameter can be an array of
    // content blocks. The plain-string decoder dropped those
    // requests silently. Verify the decoder now extracts every text
    // segment and preserves non-text blocks as Unknown.
    use autorouter_core::{ContentPart, MessageRole};
    use serde_json::json;
    let body = json!({
        "model": "claude-3-5-sonnet",
        "system": [
            { "type": "text", "text": "You are a helpful assistant." },
            { "type": "text", "text": "Be concise." },
            {
                "type": "text",
                "text": "Pinned block.",
                "cache_control": { "type": "ephemeral" }
            }
        ],
        "messages": [
            { "role": "user", "content": "hello" }
        ]
    });
    let req = pipeline()
        .parse_request(ProviderKind::Anthropic, &body)
        .expect("decode_anthropic succeeds");
    // The first system message must be the concatenated text.
    let system_msgs: Vec<_> = req
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::System)
        .collect();
    assert_eq!(system_msgs.len(), 1);
    let combined: String = system_msgs[0]
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(combined.contains("helpful assistant"));
    assert!(combined.contains("Be concise"));
    assert!(combined.contains("Pinned block"));
    // Pinning hint remains inside the Unknown part so the encoder
    // can re-emit it.
    let has_cache_control = system_msgs[0].content.iter().any(|p| match p {
        ContentPart::Unknown { raw, .. } => {
            raw.get("cache_control")
                .and_then(|c| c.get("type"))
                .and_then(|t| t.as_str())
                == Some("ephemeral")
        }
        _ => false,
    });
    assert!(
        has_cache_control,
        "cache_control metadata must reach the upstream encoder"
    );
}

#[test]
fn anthropic_system_string_still_works() {
    // Back-compat: the plain-string form must still parse exactly
    // the same way it always did.
    use autorouter_core::{ContentPart, MessageRole};
    use serde_json::json;
    let body = json!({
        "model": "claude-3-5-sonnet",
        "system": "You are concise.",
        "messages": [
            { "role": "user", "content": "hi" }
        ]
    });
    let req = pipeline()
        .parse_request(ProviderKind::Anthropic, &body)
        .expect("decode_anthropic succeeds");
    let sys = req
        .messages
        .iter()
        .find(|m| m.role == MessageRole::System)
        .expect("system message present");
    assert_eq!(sys.content.len(), 1);
    match &sys.content[0] {
        ContentPart::Text { text } => assert_eq!(text, "You are concise."),
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn anthropic_round_trip_preserves_cache_control_on_system() {
    // gap #5: an Anthropic request with cache_control blocks on the
    // system prompt must survive a parse -> serialise round-trip
    // back to Anthropic. The decode step must keep the cache hint
    // and the encode step must emit system as the documented
    // array-of-blocks form (not a plain string).
    use autorouter_core::{ContentPart, MessageRole};
    use serde_json::json;
    let body = json!({
        "model": "claude-3-5-sonnet",
        "system": [
            { "type": "text", "text": "Pinned preamble." },
            {
                "type": "text",
                "text": "Long reference doc...",
                "cache_control": { "type": "ephemeral" }
            }
        ],
        "messages": [
            { "role": "user", "content": "summarise" }
        ]
    });
    let req = pipeline()
        .parse_request(ProviderKind::Anthropic, &body)
        .expect("decode_anthropic succeeds");
    let encoded = pipeline()
        .serialise_request(ProviderKind::Anthropic, &req)
        .expect("encode_request succeeds");
    let sys = encoded
        .get("system")
        .and_then(|v| v.as_array())
        .expect("system must be a block array, not a string");
    assert_eq!(sys.len(), 2, "both system blocks must round-trip");
    let pinned = &sys[1];
    assert_eq!(
        pinned.get("text").and_then(|v| v.as_str()),
        Some("Long reference doc...")
    );
    assert_eq!(
        pinned
            .get("cache_control")
            .and_then(|c| c.get("type"))
            .and_then(|v| v.as_str()),
        Some("ephemeral"),
        "cache_control metadata must be re-emitted to the upstream"
    );
    // Sanity: the decoder kept an Unknown part alongside the Text so
    // the encoder could merge them.
    let sys_msg = req
        .messages
        .iter()
        .find(|m| m.role == MessageRole::System)
        .expect("system message");
    let has_unknown_with_cache = sys_msg.content.iter().any(|p| match p {
        ContentPart::Unknown { raw, .. } => raw.get("cache_control").is_some(),
        _ => false,
    });
    assert!(has_unknown_with_cache);
}

#[test]
fn anthropic_round_trip_preserves_cache_control_on_user_message() {
    // gap #5: cache_control on a user-text block must round-trip
    // through the encoder so the upstream honours the cache pin
    // even though it came in on a non-system message.
    use autorouter_core::{ContentPart, MessageRole};
    use serde_json::json;
    let body = json!({
        "model": "claude-3-5-sonnet",
        "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "Pinned user context." }
            ]},
            { "role": "user", "content": [
                {
                    "type": "text",
                    "text": "big document...",
                    "cache_control": { "type": "ephemeral" }
                }
            ]}
        ]
    });
    let req = pipeline()
        .parse_request(ProviderKind::Anthropic, &body)
        .expect("decode_anthropic succeeds");
    let encoded = pipeline()
        .serialise_request(ProviderKind::Anthropic, &req)
        .expect("encode_request succeeds");
    let msgs = encoded.get("messages").and_then(|v| v.as_array()).unwrap();
    // Second user message must carry the cache_control on its text block.
    let last = &msgs[msgs.len() - 1];
    let content = last
        .get("content")
        .and_then(|v| v.as_array())
        .expect("user message content is an array because it has a non-text part");
    let text_block = content
        .iter()
        .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
        .expect("text block present");
    assert_eq!(
        text_block.get("text").and_then(|v| v.as_str()),
        Some("big document...")
    );
    assert_eq!(
        text_block
            .get("cache_control")
            .and_then(|c| c.get("type"))
            .and_then(|v| v.as_str()),
        Some("ephemeral"),
        "cache_control must be re-emitted on the user-text block"
    );
    // Decoder sanity: a User-role Text was followed by an Unknown
    // pin marker.
    let last_user_msg = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .expect("user message");
    let has_pin = last_user_msg.content.iter().any(|p| match p {
        ContentPart::Unknown { raw, .. } => raw.get("cache_control").is_some(),
        _ => false,
    });
    assert!(has_pin);
}
