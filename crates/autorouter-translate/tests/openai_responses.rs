//! End-to-end compatibility tests for the OpenAI Responses adapter
//! specifically around the `reasoning` item type and the
//! `response.reasoning_text.delta` / `response.reasoning_summary_text.delta`
//! streaming events.

use autorouter_core::{ContentPart, FinishReason, StreamChunk, StreamEvent, UniversalRequest};
use autorouter_translate::{OpenAiResponsesAdapter, ProviderAdapter};

#[test]
fn decode_openai_responses_extracts_reasoning_item() {
    let adapter = OpenAiResponsesAdapter::new();
    // Per OpenAI Responses API:
    //   - `output[]` may contain both `reasoning` items and `message`
    //     items as siblings.
    //   - A `reasoning` item carries `summary: [{ type: "summary_text", text }]`
    //     OR `content: [{ type: "reasoning_text", text }]`.
    // Both shapes must end up as ContentPart::Reasoning on the universal
    // message; the visible answer must be ContentPart::Text.
    let body = serde_json::json!({
        "id": "resp_1",
        "model": "o3-mini",
        "status": "completed",
        "output": [
            {
                "type": "reasoning",
                "summary": [
                    { "type": "summary_text", "text": "step 1" },
                    { "type": "summary_text", "text": "step 2" }
                ]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "final answer" }]
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "reasoning_tokens": 7 }
    });
    let resp = adapter
        .decode_response(&empty_request(), &body, 200)
        .expect("decode response")
        .response;
    assert_eq!(resp.message.content.len(), 2);
    assert!(matches!(
        &resp.message.content[0],
        ContentPart::Reasoning { text } if text == "step 1\nstep 2"
    ));
    assert!(matches!(
        &resp.message.content[1],
        ContentPart::Text { text } if text == "final answer"
    ));
    assert_eq!(resp.usage.tokens.reasoning, Some(7));
    assert_eq!(resp.finish_reason, FinishReason::Stop);
}

#[test]
fn decode_openai_responses_extracts_reasoning_content_shape() {
    // Alternative reasoning item shape: `content: [{type:"reasoning_text",...}]`
    let adapter = OpenAiResponsesAdapter::new();
    let body = serde_json::json!({
        "id": "resp_2",
        "model": "o3-mini",
        "status": "completed",
        "output": [
            {
                "type": "reasoning",
                "content": [
                    { "type": "reasoning_text", "text": "deep thought" }
                ]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "ok" }]
            }
        ]
    });
    let resp = adapter
        .decode_response(&empty_request(), &body, 200)
        .unwrap()
        .response;
    assert!(matches!(
        &resp.message.content[0],
        ContentPart::Reasoning { text } if text == "deep thought"
    ));
    assert!(matches!(
        &resp.message.content[1],
        ContentPart::Text { text } if text == "ok"
    ));
}

#[test]
fn decode_openai_responses_streaming_extracts_reasoning_text_delta() {
    let adapter = OpenAiResponsesAdapter::new();
    // o-series models emit `response.reasoning_text.delta` events on
    // the SSE stream.
    let chunk = r#"data: {"type":"response.reasoning_text.delta","delta":"reasoning here"}"#;
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), chunk)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ReasoningDelta { text } => assert_eq!(text, "reasoning here"),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn decode_openai_responses_streaming_extracts_reasoning_summary_text_delta() {
    // Sanity check that the existing `reasoning_summary_text.delta`
    // event still works — this branch was already correct, but the
    // new `reasoning_text.delta` arm sits next to it in the match
    // and a regression here would indicate a regression there too.
    let adapter = OpenAiResponsesAdapter::new();
    let chunk = r#"data: {"type":"response.reasoning_summary_text.delta","delta":"summary chunk"}"#;
    let events: Vec<StreamEvent> = adapter
        .decode_stream_chunk(&empty_request(), chunk)
        .unwrap()
        .into_iter()
        .flat_map(|c: StreamChunk| c.events)
        .collect();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        StreamEvent::ReasoningDelta { text } if text == "summary chunk"
    ));
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

#[test]
fn inline_reasoning_tag_in_output_text_becomes_reasoning_part() {
    let adapter = OpenAiResponsesAdapter::new();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let text = format!("Lead {opener}thoughts{closer} tail");
    let body = serde_json::json!({
        "id": "resp_1",
        "model": "gpt-5",
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": text
            }]
        }]
    });
    let upstream = adapter
        .decode_response(&empty_request(), &body, 200)
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
fn inline_reasoning_tag_in_streaming_delta_becomes_reasoning_delta() {
    use autorouter_translate::ProviderAdapter;
    let adapter = OpenAiResponsesAdapter::new();
    let opener = String::from("<") + "!--reasoning-->";
    let closer = String::from("<") + "!--/reasoning-->";
    let payload = format!(
        r#"data: {{"type":"response.output_text.delta","delta":"hi {opener}thoughts{closer} bye"}}"#
    );
    let chunks = adapter
        .decode_stream_chunk(&empty_request(), payload.as_str())
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
// Terminal-path reasoning streamer cleanup (regression tests for the
// leak where a failed/incomplete Responses stream did not call
// streamer_finish and left a ReasoningStreamer entry in the static
// map until process restart).
// ---------------------------------------------------------------------------

#[test]
fn response_failed_drains_reasoning_and_emits_error() {
    // Set up a stream with an unclosed `<think>` inline tag so the
    // per-request streamer holds reasoning state. Then send
    // `response.failed`; the adapter must drain the reasoning
    // (emitting it as ReasoningDelta) AND emit the Error event,
    // leaving the streamer map clean.
    let adapter = OpenAiResponsesAdapter::new();
    let req = empty_request();

    // 1) Feed an unclosed `<think>`: the streamer holds the whole
    //    body in carry because it's waiting for `</think>`.
    let text_chunk =
        r#"data: {"type":"response.output_text.delta","delta":"<think>partial thoughts"}"#;
    let _ = adapter.decode_stream_chunk(&req, text_chunk).unwrap();

    // 2) Now send the terminal error. The reasoning must be drained
    //    as ReasoningDelta, and the Error event must be emitted after.
    let err_chunk = r#"data: {"type":"response.failed","response":{"error":{"message":"upstream blew up","code":"rate_limit_exceeded"}}}"#;
    let chunks = adapter.decode_stream_chunk(&req, err_chunk).unwrap();

    let mut all: Vec<StreamEvent> = Vec::new();
    for c in chunks {
        all.extend(c.events);
    }
    // There must be at least one ReasoningDelta (drained carry) and
    // the trailing Error event must be LAST (so the consumer stops).
    let last = all.last().expect("at least the Error event");
    assert!(
        matches!(last, StreamEvent::Error { message, .. } if message == "upstream blew up"),
        "terminal event must be Error, got {last:?}"
    );
    let has_drained_reasoning = all.iter().any(|e| {
        matches!(
            e,
            StreamEvent::ReasoningDelta { text } if !text.is_empty()
        )
    });
    assert!(
        has_drained_reasoning,
        "response.failed must drain pending reasoning state, got events: {all:?}"
    );

    // 3) Critical: the per-request streamer entry must be GONE so a
    //    subsequent stream with the same stream_id doesn't see stale
    //    state. Verify behaviorally: feed a new unclosed `<think>`
    //    to the SAME request_id and check it blocks again (carry
    //    re-populated). If the entry had leaked from the previous
    //    stream, the carry from the first call would surface here.
    let after = adapter
        .decode_stream_chunk(
            &req,
            r#"data: {"type":"response.output_text.delta","delta":"<think>fresh"}"#,
        )
        .unwrap();
    // No events should be emitted because the opener is incomplete
    // (carry holds it). If the previous stream's entry had leaked,
    // the first call would have emitted ReasoningDelta instead.
    let leaked: Vec<StreamEvent> = after.into_iter().flat_map(|c| c.events).collect();
    assert!(
        leaked.iter().all(|e| matches!(
            e,
            StreamEvent::TextDelta { .. } | StreamEvent::ReasoningDelta { .. }
        )) && !leaked
            .iter()
            .any(|e| matches!(e, StreamEvent::ReasoningDelta { text } if text.contains("partial"))),
        "stale carry from the previous failed stream leaked into a new stream: {leaked:?}"
    );
}

#[test]
fn response_incomplete_also_drains_reasoning() {
    // Same shape as response.failed but for the `response.incomplete`
    // arm — both share the cleanup code path.
    let adapter = OpenAiResponsesAdapter::new();
    let req = empty_request();

    let _ = adapter
        .decode_stream_chunk(
            &req,
            r#"data: {"type":"response.output_text.delta","delta":"<think>deep"}"#,
        )
        .unwrap();

    let err_chunk = r#"data: {"type":"response.incomplete","response":{"error":{"message":"max_output_tokens reached mid-think","code":"incomplete"}}}"#;
    let chunks = adapter.decode_stream_chunk(&req, err_chunk).unwrap();
    let mut all: Vec<StreamEvent> = Vec::new();
    for c in chunks {
        all.extend(c.events);
    }
    let last = all.last().expect("at least the Error event");
    assert!(matches!(last, StreamEvent::Error { .. }));
    assert!(
        all.iter().any(|e| matches!(
            e,
            StreamEvent::ReasoningDelta { text } if !text.is_empty()
        )),
        "response.incomplete must drain reasoning, got: {all:?}"
    );
}
