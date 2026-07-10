//! Streaming translation utilities.
//!
//! Phase 1 only exposes a few small helpers. The Phase 3 gateway uses
//! these to bridge the upstream SSE stream into the consumer wire format.

use autorouter_core::{FinishReason, StreamEvent, ToolCall};

use crate::error::TranslateResult;

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix seconds, used to timestamp loop-guard flags so they expire.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How many times the exact same command may repeat across turns before we
/// consider it a runaway loop and stop relaying further tool calls.
///
/// Set to 8: high enough that legitimate agentic iteration (a command run a
/// handful of times as the agent retries a transient failure or re-runs a
/// build) is not falsely suppressed, while a genuinely stuck command — one
/// that hangs or prompts interactively with no TTY and never produces useful
/// output — is stopped well before it burns meaningful quota. The counter is
/// only cleared for commands that produce a real completion, so a no-op or
/// yielded retry still accumulates toward this limit.
const LOOP_GUARD_MAX_REPEATS: usize = 8;

/// Sliding window: how recently the same normalized command must recur to be
/// considered a runaway loop. Agentic loops retry a hung/failing command on a
/// slow cadence (observed 12–40s apart, often with other commands interleaved).
/// Use a window well above that so legitimate interleaved work between retries
/// does not reset the streak, but short enough that a session which moves on to
/// different work is not suppressed forever.
const LOOP_GUARD_WINDOW_SECS: u64 = 120;

/// How long a "looping" flag stays authoritative after the last time the
/// looping command was seen, so a session that moves on to legitimate work (or
/// a fresh one-shot run) is not suppressed forever. While the same looping
/// command keeps re-appearing we refresh `looping_at`, which keeps a genuinely
/// stuck session suppressed for as long as it hammers the command.
const LOOP_GUARD_TTL_SECS: u64 = 120;

/// Per-run tool-round tracker (moon-bridge `max_tool_rounds` style).
///
/// WHY A GLOBAL MAP (not per-request state): Codex drives an agentic loop by
/// issuing a *separate* `/v1/responses` HTTP request for every tool turn, and
/// it sends NO stable anchor (no `previous_response_id`, no `conversation.id`,
/// no session header). So each turn arrives as a fresh request with a fresh
/// per-request `ResponsesSseState`, and any counter stored in that state resets
/// to 0 on every call — it can never accumulate. We therefore derive a stable
/// per-run key server-side (from the first user message text) and key this map
/// by it. Entries live for the agentic run plus `RUN_TTL_SECS`, then expire.
///
/// Unlike the identical-repeat guard (which only trips on 3+ of the SAME
/// command), this counts ANY round variant-agnostically, so it catches loops
/// where the model varies the command. The first call of a run is ALWAYS
/// allowed; we only nudge near the cap and hard-cut past it.
static RUN_TOOL_ROUNDS: LazyLock<Mutex<HashMap<String, RunRoundState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct RunRoundState {
    rounds: u32,
    last_seen: u64,
}

/// How long a run's round counter survives with no new tool call before it is
/// evicted (lets a paused run resume without losing its budget, but prevents
/// unbounded map growth). 10 minutes.
const RUN_TTL_SECS: u64 = 600;

/// Hard cap on tool rounds per agentic run (moon-bridge default is 50).
/// Set to 40: high enough for real multi-file coding sessions (edit, build,
/// test, edit, build, test, …) but low enough to catch runaway loops before
/// the user gives up and Ctrl-Cs. The previous value of 200 was so high that
/// users always killed the session manually before the cap triggered.
const MAX_TOOL_ROUNDS: u32 = 40;

/// Rounds before the cap at which we start nudging the model (mirrors
/// moon-bridge's `max-4` nudge). Nudge at 10 before the cap so the agent
/// has time to self-correct.
const MAX_TOOL_ROUNDS_REMINDER_AT: u32 = MAX_TOOL_ROUNDS - 10;

/// Look up (and increment if `bump`) the tool-round counter for a run.
/// Returns the new count. Evicts expired runs opportunistically.
///
/// IMPORTANT: `run_key` is only a hash of the first user message, so two
/// DISTINCT tasks that happen to share the same opening text would otherwise
/// collide and share one counter (a loop in task A would poison task B). To
/// avoid that, we RESET a run's counter whenever the previous round happened
/// more than `RUN_GAP_RESET_SECS` ago. A genuine agentic loop issues rounds
/// back-to-back (sub-second cadence, as seen with DeepSeek's `ls` spam), so it
/// keeps accumulating and still gets caught. A brand-new task that arrives after
/// a gap starts fresh — no cross-task poisoning.
fn run_tool_rounds(run_key: &str, bump: bool) -> u32 {
    let mut map = RUN_TOOL_ROUNDS.lock().unwrap();
    let now = now_secs();
    // Evict expired runs.
    map.retain(|_, s| now.saturating_sub(s.last_seen) < RUN_TTL_SECS);
    let entry = map.entry(run_key.to_string()).or_insert(RunRoundState {
        rounds: 0,
        last_seen: now,
    });
    if bump {
        // If the last round was a long time ago, treat this as a fresh run
        // (different task, same prompt text) and reset the budget.
        if now.saturating_sub(entry.last_seen) > RUN_GAP_RESET_SECS {
            entry.rounds = 0;
        }
        entry.rounds = entry.rounds.saturating_add(1);
        entry.last_seen = now;
    } else {
        // Read-only probe (ToolCallStart): also reset if stale, so the first
        // call of a genuinely new task is never cut by a prior run's counter.
        if now.saturating_sub(entry.last_seen) > RUN_GAP_RESET_SECS {
            entry.rounds = 0;
        }
        entry.last_seen = now;
    }
    entry.rounds
}

/// A run is considered "fresh" (counter reset) if its previous round happened
/// longer ago than this. 30s: comfortably longer than a real loop's cadence,
/// comfortably shorter than a human starting a new task.
const RUN_GAP_RESET_SECS: u64 = 30;

/// Cross-turn loop tracker. Each Codex agentic turn is a *separate*
/// `/v1/responses` request with a fresh `ResponsesSseState`, so an in-session
/// Runaway loop guard.
///
/// DeepSeek (via the Codex Responses API) sometimes gets stuck re-issuing the
/// SAME shell command across turns — often with tiny cosmetic variations
/// (e.g. `npm create vite@latest foo-a` → `foo-b` → `.` → `-y`)
/// and other commands interleaved (`ls`, `cd`). A per-turn signature list
/// cannot catch this, because each Codex agentic turn is a *separate* HTTP
/// request whose `previous_response_id` (the only per-conversation id Codex
/// reliably sends) ROTATES every turn. So we key the guard purely on the
/// NORMALIZED command signature (globally), using a sliding time window: when
/// the same normalized command recurs >= LOOP_GUARD_MAX_REPEATS within
/// LOOP_GUARD_WINDOW_SECS we mark it as looping and stop relaying further
/// calls of that SAME command family. Other, different commands proceed.
///
/// The flag auto-expires LOOP_GUARD_TTL_SECS after the last time the looping
/// command was seen, so a session that moves on to legitimate work (or a fresh
/// one-shot run) is not suppressed forever. While the same looping command
/// keeps re-appearing we refresh `looping_at`, which keeps a genuinely stuck
/// session suppressed for as long as it hammers the command.
///
/// NOTE: keying globally (not per-session) means a command family that loops in
/// one session is also suppressed in any other session within the window. This
/// is acceptable: a globally-failing identical command (e.g. `npm create vite`
/// with no network) should be stopped everywhere, and a single legitimate use
/// of the same command never trips the >=3 threshold.
#[derive(Default)]
struct CmdWindow {
    /// Wall-clock seconds of the most recent occurrences (newest last).
    /// Capped at `LOOP_GUARD_MAX_REPEATS + 1` entries.
    times: Vec<u64>,
    /// Human-readable last raw command (for an instructive message).
    last_command: String,
    /// `Instant` (secs since epoch) when this command last tripped the loop
    /// threshold; 0 = not looping. Suppression is active while
    /// `now - looping_at < LOOP_GUARD_TTL_SECS`.
    looping_at: u64,
}

/// Global sliding-window loop tracker keyed PURELY by normalized command
/// signature (NOT per-run). Codex drives each agentic turn as a separate
/// HTTP request with no stable conversation anchor and (in the OpenAI
/// Responses pattern) sends only the incremental `input` plus a rotating
/// `previous_response_id`, so a per-run `run_key` derived from the first
/// user message is UNSTABLE across turns and a per-run guard never
/// accumulates. Keying this detector globally is the documented intent
/// (see `CONV_LOOPS` above) and is what actually catches `npm create vite`
/// style loops regardless of how the client batches turns. The >=3 repeat
/// threshold within `LOOP_GUARD_WINDOW_SECS` means a single legitimate use
/// of any command never trips it.
static GLOBAL_SIG_LOOPS: LazyLock<Mutex<HashMap<String, CmdWindow>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per-conversation map of normalized-command -> sliding-window state. Keyed
/// by `state.conversation_key` (which Codex sends as the *stable*
/// `conversation.id` when present, falling back to the rotating
/// `previous_response_id`, then to the per-request UUID). When the key is
/// empty we use the literal string `"*fresh"` so that a brand-new session
/// never inherits state from a prior one.
static CONV_LOOPS: Lazy<Mutex<HashMap<String, HashMap<String, CmdWindow>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Maps a tool call's `id` to its loop-guard entries so that when a tool
/// RESULT arrives in a later request (proving the command completed — even
/// if it failed), the loop counter can be cleared. A genuinely hanging
/// command (interactive prompt, no TTY) never produces a result, so the
/// guard still catches those true runaways.
type CallMeta = (String, String, serde_json::Value);
static CALL_ID_MAP: LazyLock<Mutex<HashMap<String, CallMeta>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Tracks the most-recently-seen command signature per agentic run
/// (`run_key`). Used to implement the "moving on" reset: when the model
/// issues a DIFFERENT command family than the previous one in the same run,
/// the previous family's loop counters are cleared so a prior step's retries
/// don't poison the next step (e.g. `npm create vite` retried 3× must not
/// block the subsequent `npm install`).
static LAST_SIG_PER_RUN: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Determine whether a tool result's output indicates the command actually
/// completed (produced real output) vs. merely yielded with "Process running"
/// and empty output (which means the command is still in-flight and the model
/// got no useful feedback).
///
/// We clear loop-guard counters only for genuinely completed commands. A
/// "Process running" result with empty output is NOT a completion — it's a
/// timeout/yield that gives the model zero information, so the loop counter
/// must persist to catch the inevitable retry.
fn is_real_completion(output: &str) -> bool {
    // "Process running" means exec_command yielded at yield_time_ms without
    // the command finishing. The output is empty or near-empty.
    if output.contains("Process running with session ID") {
        return false;
    }
    // "aborted by user" is also not a real completion.
    if output.contains("aborted by user") {
        return false;
    }
    true
}

/// Clear the loop-guard counters for tool calls that produced results
/// (i.e. did NOT hang). Call this before streaming a new request when
/// the request body contains `function_call_output` items.
///
/// IMPORTANT: Only clears when the tool result indicates the command actually
/// COMPLETED (has real output, not "Process running" with empty output).
/// A yielded-but-still-running command gives the model no useful feedback,
/// so the loop counter must persist to catch the retry.

/// Look up the stored tool-call metadata for a `call_id`.
///
/// Returns `(signature, run_key, arguments)` if the call was previously
/// seen. This lets the request decoder (`pipeline.rs`) reconstruct the
/// assistant `tool_calls` message that MUST precede a `function_call_output`
/// tool result when relaying to an OpenAI-style chat upstream — Codex sends
/// the result as a *separate* `/v1/responses` request, so without this the
/// upstream would reject the orphan `tool` message.
pub fn get_call_args(call_id: &str) -> Option<(String, String, serde_json::Value)> {
    let map = CALL_ID_MAP.lock().unwrap();
    map.get(call_id)
        .map(|(sig, scope, args)| (sig.clone(), scope.clone(), args.clone()))
}

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
                "delta": { "tool_calls": [{ "index": 0, "function": {} }] },
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

/// State tracker for the OpenAI Responses SSE lifecycle.
///
/// The Responses API streaming protocol requires a strict event
/// sequence: `response.created` → `response.in_progress` →
/// `response.output_item.added` → `response.content_part.added`
/// BEFORE any `response.output_text.delta` events. Likewise,
/// `response.content_part.done` → `response.output_item.done` BEFORE
/// `response.completed`. This struct tracks that state across
/// successive calls to [`encode_openai_responses_sse`] so the wire
/// frames are emitted in the correct order.
#[derive(Debug, Clone, Default)]
pub struct ResponsesSseState {
    response_id: String,
    model: String,
    /// id of the active message output item (for text/reasoning).
    message_item_id: String,
    /// True after `response.output_item.added` + `response.content_part.added`
    /// have been emitted for the message item.
    setup_emitted: bool,
    /// True after `response.content_part.done` + `response.output_item.done`
    /// have been emitted (closed by ToolCallStart or Finish).
    text_closed: bool,
    /// Accumulated visible text for the `content_part.done` frame.
    accumulated_text: String,
    /// Tool call count — used to assign `output_index` values.
    tool_call_count: u32,
    /// Completed tool calls, accumulated as they stream in.
    tool_calls: Vec<ToolCall>,
    /// Currently in-progress tool call, if any.
    active_call: Option<ToolCall>,
    /// Accumulated raw argument string for the active call.
    active_args: String,
    /// Output index of the active tool call.
    active_call_output_index: u32,
    /// Conversation key (e.g. `previous_response_id`) used to look up the
    /// process-wide loop tracker so repeated calls across turns are caught.
    conversation_key: String,
    /// Set true when, on this turn, the loop guard has already decided to
    /// suppress the active tool call (so ToolCallDelta/End become no-ops).
    suppress_active: bool,
    /// For Codex 0.142.5, `exec_command` must be emitted as a `function_call`
    /// whose `output_item.added` carries the COMPLETE `{"cmd": "..."}` arguments.
    /// Because the command only arrives at `ToolCallEnd` (streamed via deltas),
    /// we buffer the `added` frame at `ToolCallStart` and emit it (with the full
    /// args) at `ToolCallEnd`. This holds the buffered frame until then.
    pending_shell_added: Option<String>,
}

impl ResponsesSseState {
    pub fn new(response_id: String, model: String, conversation_key: String) -> Self {
        let message_item_id = format!("{}__msg_0", &response_id);
        Self {
            response_id,
            model,
            message_item_id,
            setup_emitted: false,
            text_closed: false,
            accumulated_text: String::new(),
            tool_call_count: 0,
            tool_calls: Vec::new(),
            active_call: None,
            active_args: String::new(),
            active_call_output_index: 0,
            conversation_key,
            suppress_active: false,
            pending_shell_added: None,
        }
    }

    fn ensure_setup(&mut self, frames: &mut Vec<String>) {
        if self.setup_emitted {
            return;
        }
        let id = &self.response_id;
        let model = &self.model;
        let item_id = &self.message_item_id;
        use serde_json::json;
        frames.push(sse_responses_frame(
            "response.created",
            &json!({
                "type": "response.created",
                "response": { "id": id, "model": model, "status": "queued" }
            }),
        ));
        frames.push(sse_responses_frame(
            "response.in_progress",
            &json!({
                "type": "response.in_progress",
                "response": { "id": id, "model": model, "status": "in_progress" }
            }),
        ));
        frames.push(sse_responses_frame(
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }),
        ));
        frames.push(sse_responses_frame(
            "response.content_part.added",
            &json!({
                "type": "response.content_part.added",
                "output_index": 0,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": []
                }
            }),
        ));
        self.setup_emitted = true;
    }

    fn close_text(&mut self, frames: &mut Vec<String>) {
        if self.text_closed {
            return;
        }
        let item_id = &self.message_item_id;
        use serde_json::json;
        frames.push(sse_responses_frame(
            "response.content_part.done",
            &json!({
                "type": "response.content_part.done",
                "output_index": 0,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": self.accumulated_text,
                    "annotations": []
                }
            }),
        ));
        frames.push(sse_responses_frame(
            "response.output_item.done",
            &json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": self.accumulated_text,
                        "annotations": []
                    }]
                }
            }),
        ));
        self.text_closed = true;
    }
}

/// Format a single SSE frame with an optional event name.
fn sse_responses_frame(event_name: &str, payload: &serde_json::Value) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        event_name,
        serde_json::to_string(payload).unwrap_or_default()
    )
}

/// Determine the Responses API output item type for a Codex tool.
/// `exec_command`/`shell` must be a `function_call` whose `arguments` carry
/// `{"cmd": "..."}` (Codex 0.142.5 rejects `command` and rejects a server
/// `local_shell_call`). `apply_patch` stays a `custom_tool_call`.
fn codex_output_item_type(tool_name: &str) -> &str {
    match tool_name {
        "apply_patch" => "custom_tool_call",
        _ => "function_call",
    }
}

/// Canonicalize a tool name to what the Codex client can actually execute.
/// Codex's native router only knows `exec_command` and `local_shell_call`;
/// upstreams sometimes emit the alias `shell`, which Codex rejects with
/// "unsupported call: shell". Map any shell alias to `exec_command` so the
/// tool call round-trips instead of being dropped. This is name canonicaliza-
/// tion only — it never inspects or alters the task/content of the call.
fn canon_tool_name(tool_name: &str) -> &str {
    if tool_name == "shell" {
        "exec_command"
    } else {
        tool_name
    }
}

/// Normalize a tool call's arguments before emitting them to Codex.
/// Codex's `exec_command` tool expects `{"command": "..."}`; some upstreams
/// emit `{"cmd": "..."}` instead, so rename it.
fn normalize_tool_args(tool_name: &str, args: &serde_json::Value) -> serde_json::Value {
    if tool_name == "exec_command" || tool_name == "shell" {
        if let serde_json::Value::Object(obj) = args {
            if obj.contains_key("cmd") && !obj.contains_key("command") {
                let mut fixed = obj.clone();
                if let Some(cmd) = fixed.remove("cmd") {
                    fixed.insert("command".to_string(), cmd);
                }
                return serde_json::Value::Object(fixed);
            }
        }
    }
    args.clone()
}

/// Outbound argument form for Codex. Codex 0.142.5's `exec_command` tool
/// schema uses the field `cmd` (not `command`); it rejects
/// `{"command": "..."}` with "missing field `cmd`". Upstreams (DeepSeek,
/// OpenRouter, …) emit `command`, so rewrite the key on the way OUT to Codex.
/// The inbound side (`normalize_tool_args`) already maps `cmd`->`command`
/// when Codex's result round-trips back, keeping the upstream contract
/// unchanged. `apply_patch` and other tools are passed through untouched.
fn emit_tool_args(tool_name: &str, args: &serde_json::Value) -> serde_json::Value {
    if tool_name == "exec_command" || tool_name == "shell" {
        if let serde_json::Value::Object(obj) = args {
            if obj.contains_key("command") && !obj.contains_key("cmd") {
                let mut fixed = obj.clone();
                if let Some(command) = fixed.remove("command") {
                    fixed.insert("cmd".to_string(), command);
                }
                return serde_json::Value::Object(fixed);
            }
        }
    }
    args.clone()
}

/// Normalized signature for a tool call, used to detect identical repeated calls.
/// For shell/exec tools we deliberately COLLAPSE cosmetic variations of the same
/// intent so the loop guard catches "retry with a slightly different name/flag"
/// patterns — e.g. `npm create vite@latest foo-a -- --template react`,
/// `npm create vite@latest foo-b -- --template react`, and
/// `npm create vite@latest . -y` all normalize to `npm create vite`. We drop the
/// project/dir argument, trailing flags, and template variants, keeping only the
/// command "family" (the program + subcommand that actually does the work).
fn tool_call_signature(tool_name: &str, args: &serde_json::Value) -> String {
    if tool_name == "exec_command" || tool_name == "shell" {
        let norm = normalize_tool_args(tool_name, args);
        let cmd = norm.get("command").and_then(|v| v.as_str()).unwrap_or("");
        return format!("{}:cmd={}", tool_name, normalize_shell_command(cmd));
    }
    let norm = normalize_tool_args(tool_name, args);
    format!(
        "{}:{}",
        tool_name,
        serde_json::to_string(&norm).unwrap_or_default()
    )
}

/// Reduce a shell command to its stable "intent family" so that retries that
/// only differ by the target path/name, flags, redirections, or a leading
/// `cd <dir> &&` prefix still compare equal.
///
/// Examples that all collapse to `npm create vite`:
///   `cd /tmp && npm create vite@latest foo-a -- --template react 2>&1`
///   `npm create vite@latest foo-b -- --template react`
///   `npm create vite@latest . -y`
///   `npx create-vite@latest foo`
fn normalize_shell_command(cmd: &str) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return String::new();
    }
    // 1. Split into command segments on && | ; (keep the meaningful one).
    //    A pure `cd <dir>` prefix is not the "intent" — the real command
    //    follows it, so prefer the last non-trivial segment.
    let segments: Vec<&str> = cmd
        .split(['&', '|', ';'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let main = segments
        .iter()
        .find(|s| !s.starts_with("cd "))
        .copied()
        .or_else(|| segments.last().copied())
        .unwrap_or(cmd);

    // 2. Tokenize the main segment into argv (respecting single/double quotes).
    let tokens = split_shell_args(main);

    // 3. Drop flags and env-var assignments; keep positional args.
    let mut positional: Vec<String> = Vec::new();
    for t in &tokens {
        if t.starts_with('-') {
            continue; // --yes, -y, --template, --, --loglevel, ...
        }
        if t.contains('=') && !t.starts_with('\'') && !t.starts_with('"') {
            // FOO=bar assignment prefix — skip
            continue;
        }
        positional.push(t.clone());
    }
    if positional.is_empty() {
        return main.to_string();
    }

    // 4. Collapse well-known scaffolding families to a canonical family token.
    let prog = positional[0].as_str();
    let sub = positional.get(1).map(|s| s.as_str()).unwrap_or("");

    canonical_family(prog, sub, &positional)
}

/// Map a (program, subcommand, argv) triple to a small canonical "family"
/// string. The goal is that cosmetic retries collapse: same tool, same action,
/// regardless of project name / version tag / flags.
fn canonical_family(prog: &str, sub: &str, positional: &[String]) -> String {
    // npm create vite / npm create vite@latest / create-vite / npx create-vite
    let prog_lc = prog.to_lowercase();
    let sub_lc = sub.to_lowercase();
    if prog_lc == "npm" && sub_lc == "create" {
        // The next positional is the package (vite@latest / vite / . / name).
        // Collapse to "npm create vite" if it's a vite scaffold.
        if positional.len() >= 3 {
            let pkg = positional[2].to_lowercase();
            if pkg.contains("vite") {
                return "npm create vite".to_string();
            }
        }
        return "npm create".to_string();
    }
    if prog_lc == "npx" && sub_lc.contains("create-vite") {
        return "npm create vite".to_string();
    }
    if prog_lc == "create-vite" {
        return "npm create vite".to_string();
    }
    // git clone / pip install / yarn create / pnpm create / cargo new / etc.
    if prog_lc == "git" && sub_lc == "clone" {
        return "git clone".to_string();
    }
    if (prog_lc == "pip" || prog_lc == "pip3" || prog_lc == "uv") && sub_lc == "install" {
        return "pip install".to_string();
    }
    if (prog_lc == "yarn" || prog_lc == "pnpm") && sub_lc == "create" {
        return format!("{} create", prog_lc);
    }
    if prog_lc == "cargo" && sub_lc == "new" {
        return "cargo new".to_string();
    }
    // Generic: program + subcommand (or just program).
    if !sub.is_empty() {
        format!("{} {}", prog_lc, sub_lc)
    } else {
        prog_lc
    }
}

/// POSIX-ish argv splitter that respects single and double quotes.
fn split_shell_args(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_sq = false;
    let mut in_dq = false;
    for ch in s.chars() {
        match ch {
            '\'' if !in_dq => {
                in_sq = !in_sq;
            }
            '"' if !in_sq => {
                in_dq = !in_dq;
            }
            c if c.is_whitespace() && !in_sq && !in_dq => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn encode_openai_responses_sse(
    event: &StreamEvent,
    state: &mut ResponsesSseState,
    run_key: &str,
) -> Vec<String> {
    // After the loop guard fires (suppress_active=true) and emits
    // response.completed, consume ALL remaining events for this turn
    // silently.  The upstream stream may still produce ToolCallDelta,
    // ToolCallEnd, TextDelta, and Finish events after suppression, but
    // Codex already received its response.completed — emitting anything
    // afterward would leak garbage events after the completion and
    // corrupt Codex's SSE state tracking.
    if state.suppress_active && !matches!(event, StreamEvent::Start { .. }) {
        return vec![];
    }
    use serde_json::json;
    match event {
        StreamEvent::Start { id, model } => {
            state.response_id = id.clone();
            state.model = model.clone();
            state.message_item_id = format!("{}__msg_0", id);
            state.setup_emitted = false;
            state.text_closed = false;
            state.accumulated_text = String::new();
            state.tool_call_count = 0;
            state.tool_calls = Vec::new();
            state.active_call = None;
            state.active_args = String::new();
            let mut frames = Vec::new();
            state.ensure_setup(&mut frames);
            frames
        }
        StreamEvent::TextDelta { text } => {
            let mut frames = Vec::new();
            state.ensure_setup(&mut frames);
            state.accumulated_text.push_str(text);
            frames.push(sse_responses_frame(
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta",
                    "item_id": state.message_item_id,
                    "output_index": 0,
                    "content_index": 0,
                    "delta": text,
                }),
            ));
            frames
        }
        StreamEvent::ReasoningDelta { text } => {
            let mut frames = Vec::new();
            state.ensure_setup(&mut frames);
            frames.push(sse_responses_frame(
                "response.reasoning_summary_text.delta",
                &json!({
                    "type": "response.reasoning_summary_text.delta",
                    "item_id": state.message_item_id,
                    "output_index": 0,
                    "content_index": 0,
                    "delta": text,
                }),
            ));
            frames
        }
        StreamEvent::ToolCallStart { call } => {
            let mut frames = Vec::new();
            state.close_text(&mut frames);
            state.tool_call_count += 1;
            let output_index = state.tool_call_count;
            state.active_call = Some(call.clone());
            state.active_args = String::new();
            state.active_call_output_index = output_index;

            // --- Runaway loop guard (cross-turn) ---------------------------------
            // If this conversation has already been flagged as looping on the
            // SAME command family, suppress THIS call (and answer with a final
            // completion so Codex stops hammering the command). Other, different
            // commands are allowed to proceed.
            // IMPORTANT: the first turn of every Codex session arrives with an
            // EMPTY conversation key (no previous_response_id yet). An empty key
            // means "no history", NOT "looping" — never flag or suppress on it,
            // otherwise one looping session poisons the "" bucket and the guard
            // then kills the agentic loop for EVERY fresh session.
            let incoming_sig = tool_call_signature(&call.name, &call.arguments);
            // The cross-turn runaway suppression needs to compare this call
            // against the same command family that was flagged earlier in the
            // SAME agentic run (so we don't kill the first tool call of a
            // brand-new run with a stale loop flag from an earlier one).
            //
            // IMPORTANT: Codex sends NO stable anchor (no previous_response_id /
            // conversation.id / session header) — every tool turn is a separate
            // HTTP request and `conversation_key` is therefore always empty.
            // Keying on `conversation_key` would put ALL runs into one shared
            // "*fresh" bucket, so a loop in run A would poison run B's first
            // call. Instead we key on the server-derived `run_key` (stable hash
            // of the initial user message), which reliably isolates each run.
            //   1) If we can match the specific signature, suppress it.
            //   2) If the signature is empty (the tool arguments have not yet
            //      streamed in via ToolCallDelta), fall back to suppressing
            //      ONLY when THIS run has a previously-flagged exec_command
            //      family.
            let session = run_key.to_string();
            // Codex streams the `command` argument via ToolCallDelta, so at
            // ToolCallStart `call.arguments` is empty and `incoming_sig` is the
            // *placeholder* `exec_command:cmd=` (command unknown yet). The
            // specific signature (e.g. `exec_command:cmd=npm create vite`)
            // only becomes known at ToolCallEnd. We therefore split the
            // decision:
            //   * Known command (non-placeholder sig): check the GLOBAL map for
            //     that exact command family (catches loops where run_key is
            //     unstable across turns).
            //   * Placeholder sig (the Codex case): fall back to the
            //     run-scoped CONV_LOOPS — if THIS run already flagged an
            //     exec_command family, suppress. Run-scoping avoids
            //     cross-session false positives (a loop in one session must
            //     not kill another).
            let is_placeholder = call.name == "exec_command"
                && incoming_sig.strip_prefix("exec_command:cmd=") == Some("");
            let already_looping = if !incoming_sig.is_empty() && !is_placeholder {
                // Global keying by exact command family.
                let guard = GLOBAL_SIG_LOOPS.lock().unwrap();
                guard
                    .get(&incoming_sig)
                    .map(|w| {
                        w.looping_at != 0
                            && now_secs().saturating_sub(w.looping_at) < LOOP_GUARD_TTL_SECS
                    })
                    .unwrap_or(false)
            } else if is_placeholder && session != "*fresh" {
                // Placeholder exec_command (Codex-style): the command streams
                // in later (at ToolCallEnd), so we CANNOT match the exact
                // signature here. We used to suppress ANY exec_command in this
                // run that was flagged looping within 10s — but that killed
                // *legitimate different* commands (e.g. step 2 `npm install`
                // was suppressed because step 1 `npm create vite` had tripped
                // the guard). The precise suppression now happens at ToolCallEnd
                // where the real normalized signature is known. Here we only
                // log; we no longer suppress on a placeholder.
                tracing::debug!(
                    session = %session,
                    sig = %incoming_sig,
                    "loop-guard ToolCallStart: placeholder command — defer decision to ToolCallEnd"
                );
                false
            } else {
                false
            };
            tracing::debug!(
                session = %session,
                sig = %incoming_sig,
                already_looping = already_looping,
                "loop-guard ToolCallStart"
            );
            if already_looping {
                // Pull the last repeated command so the message is actionable.
                let last_cmd = {
                    let guard = GLOBAL_SIG_LOOPS.lock().unwrap();
                    guard
                        .get(&incoming_sig)
                        .map(|w| w.last_command.clone())
                        .unwrap_or_default()
                };
                tracing::warn!(
                    conversation = %state.conversation_key,
                    "loop guard: conversation already looping; suppressing tool call and completing turn"
                );
                state.suppress_active = true;
                // Emit setup if not already done, then a final answer + completion.
                // NOTE: the message is deliberately TRUTHFUL, not "completed
                // successfully". The repeated command usually hung (e.g. an
                // interactive prompt with no TTY) and returned no useful output,
                // which is WHY the model kept re-issuing it. Tell it that, so it
                // can self-correct instead of believing the work is done.
                let msg = if last_cmd.is_empty() {
                    "Stopping: the same tool command has now repeated several times with no useful result (it appears to hang or prompt interactively with no TTY). Do NOT re-issue the identical command. If the action needs a non-interactive form, use that (e.g. append ` --yes`, pipe `y | ...`, or set `--template` explicitly). Otherwise report what you were trying to achieve.".to_string()
                } else {
                    format!(
                        "Stopping: the command `{}` has now repeated several times with no useful result (it appears to hang or prompt interactively with no TTY available). Do NOT re-issue the identical command. Use a non-interactive variant if one exists (e.g. append ` --yes` or pipe `y | ...`), or report what you were trying to achieve.",
                        last_cmd
                    )
                };
                let fin = json!({
                    "type": "response.completed",
                    "response": {
                        "id": state.response_id,
                        "model": state.model,
                        "status": "completed",
                        "output": [{
                            "type": "message",
                            "id": state.message_item_id,
                            "role": "assistant",
                            "status": "completed",
                            "content": [{
                                "type": "output_text",
                                "text": msg,
                                "annotations": []
                            }]
                        }]
                    }
                });
                frames.push(sse_responses_frame("response.completed", &fin));
                return frames;
            }

            // --- Per-run runaway cap (moon-bridge `max_tool_rounds` style) ----
            // Counts EVERY tool round on THIS agentic run via the server-derived
            // stable `run_key` (Codex sends no per-run anchor, so we derive one
            // from the first user message). Variant-agnostic: catches loops where
            // the model varies the command (so the identical-repeat guard above
            // never trips). The FIRST call of a run is ALWAYS allowed — we only
            // start nudging near the cap and HARD-CUT once the cap is exceeded.
            let round = run_tool_rounds(run_key, true);
            if round > MAX_TOOL_ROUNDS {
                // Hard cap exceeded: suppress this call and answer with a final
                // completion that forces the model to stop looping and report
                // status honestly.
                tracing::warn!(
                    conversation = %state.conversation_key,
                    run_key = run_key,
                    round = round,
                    max = MAX_TOOL_ROUNDS,
                    "loop guard: per-run tool round cap exceeded; completing turn"
                );
                state.suppress_active = true;
                let msg = format!(
                    "Stopping: this task has now used {round} tool calls in a single run (hard cap {MAX_TOOL_ROUNDS}). \
                     This usually means the agent is stuck in a loop. Do NOT call another tool. \
                     Instead, summarize what you have accomplished so far and report the current status, \
                     or tell me explicitly what is blocking you so I can help.",
                );
                let fin = json!({
                    "type": "response.completed",
                    "response": {
                        "id": state.response_id,
                        "model": state.model,
                        "status": "completed",
                        "output": [{
                            "type": "message",
                            "id": state.message_item_id,
                            "role": "assistant",
                            "status": "completed",
                            "content": [{
                                "type": "output_text",
                                "text": msg,
                                "annotations": []
                            }]
                        }]
                    }
                });
                frames.push(sse_responses_frame("response.completed", &fin));
                return frames;
            } else if round >= MAX_TOOL_ROUNDS_REMINDER_AT {
                // Near the cap: let the call proceed, but append a nudge as a
                // system reminder so the model can self-correct before we cut.
                tracing::warn!(
                    conversation = %state.conversation_key,
                    run_key = run_key,
                    round = round,
                    max = MAX_TOOL_ROUNDS,
                    "loop guard: approaching per-run tool round cap"
                );
                let nudge = format!(
                    "[SystemReminder] You have issued {round} tool calls this run. The hard cap is {MAX_TOOL_ROUNDS}. \
                     If you are repeating the same failing action, STOP and report what you were trying to achieve, \
                     or adjust your approach. Do not loop indefinitely."
                );
                frames.push(sse_responses_frame(
                    "response.output_text.delta",
                    &json!({
                        "type": "response.output_text.delta",
                        "item_id": state.message_item_id,
                        "output_index": 0,
                        "content_index": 0,
                        "delta": format!("\n\n{nudge}\n\n")
                    }),
                ));
            }

            // Codex expects tool item IDs to have "fc_" prefix
            let item_id = if call.id.starts_with("fc_") {
                call.id.clone()
            } else {
                format!("fc_{}", call.id)
            };
            let output_type = codex_output_item_type(&call.name);
            let is_shell = call.name == "exec_command" || call.name == "shell";
            let norm_args = normalize_tool_args(&call.name, &call.arguments);
            // For Codex 0.142.5, `exec_command` must be emitted as a
            // `function_call` whose `output_item.added` carries the COMPLETE
            // `{"cmd": "..."}` arguments. Because the command only arrives at
            // `ToolCallEnd` (streamed via deltas), we DO NOT emit `added` here;
            // instead we buffer the call and emit `added` (with full args) at
            // `ToolCallEnd`. Non-shell tools emit `added` immediately as before.
            if is_shell {
                state.pending_shell_added = Some(item_id.clone());
                return frames; // emit added later, at ToolCallEnd
            }
            let item = if output_type == "local_shell_call" {
                build_local_shell_item(&item_id, &call.id, "in_progress", &norm_args)
            } else {
                json!({
                    "id": item_id,
                    "type": output_type,
                    "status": "in_progress",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": serde_json::to_string(&emit_tool_args(&call.name, &norm_args)).unwrap_or_default(),
                })
            };
            frames.push(sse_responses_frame(
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": item,
                }),
            ));
            frames
        }
        StreamEvent::ToolCallDelta {
            id,
            arguments_fragment,
        } => {
            let item_id = if id.starts_with("fc_") {
                id.clone()
            } else {
                format!("fc_{}", id)
            };
            // When the loop guard has suppressed this call, drop all further
            // fragments so nothing about the call leaks to Codex.
            if state.suppress_active {
                return vec![];
            }
            state.active_args.push_str(arguments_fragment);
            // `local_shell_call` and Codex's `exec_command` (a function_call)
            // both carry their command in `arguments`/`cmd`, NOT in streamed
            // `function_call_arguments.*` deltas. Streaming tiny JSON fragments
            // also prevents a reliable key rename, so for shell tools we skip
            // the deltas entirely and emit the complete `cmd` args at
            // `function_call_arguments.done` / `output_item.done` instead
            // (Codex assembles from those). Emitting deltas would both confuse
            // Codex and fail to parse.
            if state
                .active_call
                .as_ref()
                .map(|c| {
                    codex_output_item_type(&c.name) == "local_shell_call"
                        || c.name == "exec_command"
                        || c.name == "shell"
                })
                .unwrap_or(false)
            {
                return vec![];
            }
            vec![sse_responses_frame(
                "response.function_call_arguments.delta",
                &json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": item_id,
                    "output_index": state.active_call_output_index,
                    "content_index": 0,
                    "delta": arguments_fragment,
                }),
            )]
        }
        StreamEvent::ToolCallEnd { id } => {
            let item_id = if id.starts_with("fc_") {
                id.clone()
            } else {
                format!("fc_{}", id)
            };
            let mut frames = Vec::new();
            if let Some(mut call) = state.active_call.take() {
                // Canonicalize the tool name to what Codex's router can
                // execute ("shell" -> "exec_command"); see canon_tool_name().
                let call_name = canon_tool_name(&call.name).to_string();
                call.name = call_name.clone();
                let mut args = match serde_json::from_str::<serde_json::Value>(&state.active_args) {
                    Ok(v) => v,
                    Err(_) => call.arguments.clone(),
                };
                if call_name == "apply_patch" {
                    args = remap_apply_patch_args(args);
                }
                call.arguments = args.clone();
                state.tool_calls.push(call);
                // --- Update cross-turn loop tracker (sliding window) -------------
                // Record this call's NORMALIZED command signature. Counts use a
                // sliding time window, not a per-turn bucket, because:
                //   * Codex's `previous_response_id` ROTATES every turn, so a
                //     per-turn key never accumulates repeats across turns.
                //   * The model often retries with a slightly different command
                //     (e.g. `create-next-app foo-a` -> `foo-b` -> `.` -> `-y`);
                //     normalization collapses those into one signature so the
                //     loop is still caught.
                // When the same normalized command recurs >= LOOP_GUARD_MAX_REPEATS
                // within LOOP_GUARD_WINDOW_SECS, flag the conversation as looping
                // on that signature; ToolCallStart then suppresses further calls
                // of THAT command family (other work still proceeds).
                let sig = tool_call_signature(&call_name, &args);
                let raw_cmd = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // --- Precise per-signature loop suppression --------------------
                // At ToolCallEnd the REAL normalized command signature is known
                // (unlike ToolCallStart, where Codex streams the command later as
                // a placeholder). If THIS exact command family was already flagged
                // looping, suppress THIS call only — never a different command.
                // This is the correct place to gate: it stops a genuinely
                // repeating identical command (e.g. `npm create vite` retried 3×)
                // without collateral damage to legitimate subsequent steps
                // (e.g. `cd fb-probe && npm install`).
                {
                    let guard = GLOBAL_SIG_LOOPS.lock().unwrap();
                    if let Some(w) = guard.get(&sig) {
                        if w.looping_at != 0
                            && now_secs().saturating_sub(w.looping_at) < LOOP_GUARD_TTL_SECS
                        {
                            drop(guard);
                            tracing::warn!(
                                conversation = %state.conversation_key,
                                sig = %sig,
                                "loop guard: identical command family flagged looping; suppressing this specific call"
                            );
                            state.suppress_active = true;
                            // Emit a truthful completion that stops the loop.
                            let msg = format!(
                                "Stopping: the command `{}` has now repeated several times with no useful result (it appears to hang or prompt interactively with no TTY available). Do NOT re-issue the identical command. Use a non-interactive variant if one exists (e.g. append ` --yes` or pipe `y | ...`), or report what you were trying to achieve.",
                                raw_cmd
                            );
                            let fin = json!({
                                "type": "response.completed",
                                "response": {
                                    "id": state.response_id,
                                    "model": state.model,
                                    "status": "completed",
                                    "output": [{
                                        "type": "message",
                                        "id": state.message_item_id,
                                        "role": "assistant",
                                        "status": "completed",
                                        "content": [{
                                            "type": "output_text",
                                            "text": msg,
                                            "annotations": []
                                        }]
                                    }]
                                }
                            });
                            return vec![sse_responses_frame("response.completed", &fin)];
                        }
                    }
                }
                // Record call_id → (signature, run_key) so that when a tool
                // RESULT arrives (proving the command completed), the loop
                // counter can be cleared. A hanging command never produces a
                // result, so true runaways are still caught.
                {
                    let mut map = CALL_ID_MAP.lock().unwrap();
                    map.insert(id.clone(), (sig.clone(), run_key.to_string(), args.clone()));
                }
                {
                    // --- "Moving on" reset ---------------------------------------
                    // If this run previously flagged a DIFFERENT command family
                    // as looping, the model has now moved on to a new step
                    // (e.g. `npm create vite` → `npm install`). Clear the prior
                    // family's counters so a legitimately different subsequent
                    // command is never collateral-damaged, and so a previous
                    // step's retries don't poison the next step. A genuine
                    // runaway (same command repeated 3× consecutively) is still
                    // caught because its sig never changes.
                    let prev = {
                        let last = LAST_SIG_PER_RUN.lock().unwrap();
                        last.get(run_key).cloned()
                    };
                    if let Some(prev) = prev {
                        if prev != sig {
                            if let Some(w) = CONV_LOOPS
                                .lock()
                                .unwrap()
                                .get_mut(run_key)
                                .and_then(|p| p.get_mut(&prev))
                            {
                                w.looping_at = 0;
                                w.times.clear();
                            }
                            if let Some(w) = GLOBAL_SIG_LOOPS.lock().unwrap().get_mut(&prev) {
                                w.looping_at = 0;
                                w.times.clear();
                            }
                        }
                    }
                    LAST_SIG_PER_RUN
                        .lock()
                        .unwrap()
                        .insert(run_key.to_string(), sig.clone());
                }
                {
                    // Scope key = this run's run_key (stable per agentic run).
                    // Codex sends no conversation anchor, so we MUST key on the
                    // server-derived run_key (hash of the initial user message)
                    // rather than the always-empty conversation_key — otherwise
                    // every run lands in one shared "*fresh" bucket and a loop in
                    let scope = run_key.to_string();
                    {
                        let mut guard = CONV_LOOPS.lock().unwrap();
                        let per = guard.entry(scope).or_default();
                        let win = per.entry(sig.clone()).or_default();
                        win.last_command = raw_cmd.clone();
                        let now = now_secs();
                        win.times
                            .retain(|t| now.saturating_sub(*t) < LOOP_GUARD_WINDOW_SECS);
                        win.times.push(now);
                        if win.times.len() > LOOP_GUARD_MAX_REPEATS + 1 {
                            win.times.remove(0);
                        }
                        if win.times.len() >= LOOP_GUARD_MAX_REPEATS && win.looping_at == 0 {
                            tracing::warn!(
                                count = win.times.len(),
                                sig = %sig,
                                "loop guard: same command family repeated across turns; flagging as looping"
                            );
                        }
                        if win.times.len() >= LOOP_GUARD_MAX_REPEATS {
                            win.looping_at = now;
                        }
                    }
                    // ALSO record into the GLOBAL signature tracker so the
                    // identical-repeat detector (which keys globally, not by
                    // run_key) can catch Codex-style loops where each turn's
                    // run_key is unique (see GLOBAL_SIG_LOOPS).
                    {
                        let mut guard = GLOBAL_SIG_LOOPS.lock().unwrap();
                        let win = guard.entry(sig.clone()).or_default();
                        win.last_command = raw_cmd;
                        let now = now_secs();
                        win.times
                            .retain(|t| now.saturating_sub(*t) < LOOP_GUARD_WINDOW_SECS);
                        win.times.push(now);
                        if win.times.len() > LOOP_GUARD_MAX_REPEATS + 1 {
                            win.times.remove(0);
                        }
                        if win.times.len() >= LOOP_GUARD_MAX_REPEATS && win.looping_at == 0 {
                            tracing::warn!(
                                count = win.times.len(),
                                sig = %sig,
                                "loop guard (global): same command family repeated; flagging as looping"
                            );
                        }
                        if win.times.len() >= LOOP_GUARD_MAX_REPEATS {
                            win.looping_at = now;
                        }
                    }
                }
                let output_index = state.active_call_output_index;
                let output_type = codex_output_item_type(&call_name);
                let is_local_shell = output_type == "local_shell_call";
                let emit_args = emit_tool_args(&call_name, &args);
                // For shell tools we deferred `output_item.added` to here so it
                // can carry the COMPLETE `{"cmd": "..."}` arguments (Codex
                // 0.142.5 rejects an `added` frame whose `arguments` lacks `cmd`).
                if state.pending_shell_added.take().is_some() {
                    let added_item = json!({
                        "id": item_id,
                        "type": "function_call",
                        "status": "in_progress",
                        "call_id": id,
                        "name": call_name,
                        "arguments": serde_json::to_string(&emit_args).unwrap_or_default(),
                    });
                    frames.push(sse_responses_frame(
                        "response.output_item.added",
                        &json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": added_item,
                        }),
                    ));
                }
                // For non-shell function calls Codex expects
                // function_call_arguments.done before output_item.done.
                if !is_local_shell {
                    frames.push(sse_responses_frame(
                        "response.function_call_arguments.done",
                        &json!({
                            "type": "response.function_call_arguments.done",
                            "output_index": output_index,
                            "item_id": item_id,
                            "arguments": serde_json::to_string(&emit_args).unwrap_or_default(),
                        }),
                    ));
                }
                let output_type = codex_output_item_type(&call_name);
                let item = if output_type == "local_shell_call" {
                    build_local_shell_item(&item_id, id, "completed", &args)
                } else {
                    json!({
                        "id": item_id,
                        "type": output_type,
                        "status": "completed",
                        "call_id": id,
                        "name": call_name,
                        "arguments": serde_json::to_string(&emit_args).unwrap_or_default(),
                    })
                };
                frames.push(sse_responses_frame(
                    "response.output_item.done",
                    &json!({
                        "type": "response.output_item.done",
                        "output_index": output_index,
                        "item": item,
                    }),
                ));
            }
            state.active_args = String::new();
            frames
        }
        StreamEvent::Finish { reason, usage } => {
            let mut frames = Vec::new();
            // Flush any pending active tool call that never got a ToolCallEnd.
            if let Some(mut call) = state.active_call.take() {
                if let Ok(args) = serde_json::from_str::<serde_json::Value>(&state.active_args) {
                    call.arguments = if call.name == "apply_patch" {
                        remap_apply_patch_args(args)
                    } else {
                        args
                    };
                }
                state.tool_calls.push(call);
            }
            state.active_args = String::new();
            state.close_text(&mut frames);
            let status = match reason {
                FinishReason::Stop => "completed",
                FinishReason::Length => "incomplete",
                _ => "completed",
            };
            let mut body = json!({
                "type": "response.completed",
                "response": {
                    "id": state.response_id,
                    "model": state.model,
                    "status": status,
                    "output": []
                }
            });
            if !state.text_closed && state.accumulated_text.is_empty() && state.tool_call_count == 0
            {
                // Never emitted setup: just send the minimal response.created
                // and completed in one go.
                body["type"] = json!("response.completed");
            } else {
                let mut output = Vec::new();
                if !state.accumulated_text.is_empty() {
                    output.push(json!({
                        "type": "message",
                        "id": state.message_item_id,
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": state.accumulated_text,
                            "annotations": []
                        }]
                    }));
                }
                for call in &state.tool_calls {
                    let output_type = codex_output_item_type(&call.name);
                    let item = if output_type == "local_shell_call" {
                        // call.id here is the raw upstream id; mirror the fc_ prefix
                        // used elsewhere so Codex can correlate the item.
                        let item_id = if call.id.starts_with("fc_") {
                            call.id.clone()
                        } else {
                            format!("fc_{}", call.id)
                        };
                        build_local_shell_item(&item_id, &call.id, "completed", &call.arguments)
                    } else {
                        // Codex 0.142.5's exec_command expects `cmd` (not
                        // `command`); remap before emitting the completed item.
                        let emit_args = emit_tool_args(&call.name, &call.arguments);
                        json!({
                            "type": output_type,
                            "id": call.id,
                            "name": call.name,
                            "arguments": serde_json::to_string(&emit_args).unwrap_or_default(),
                            "status": "completed",
                        })
                    };
                    output.push(item);
                }
                body["response"]["output"] = json!(output);
            }
            if let Some(u) = usage {
                body["response"]["usage"] = json!({
                    "input_tokens": u.tokens.input.unwrap_or(0),
                    "output_tokens": u.tokens.output.unwrap_or(0),
                    "total_tokens": u.tokens.input.unwrap_or(0) + u.tokens.output.unwrap_or(0),
                });
            }
            frames.push(sse_responses_frame("response.completed", &body));
            frames
        }
        StreamEvent::Error { message, code } => {
            let mut frames = Vec::new();
            state.close_text(&mut frames);
            frames.push(format!(
                "event: error\ndata: {}\n\n",
                serde_json::to_string(&json!({
                    "type": "error",
                    "error": { "message": message, "code": code }
                }))
                .unwrap_or_default()
            ));
            frames
        }
        _ => {
            vec![]
        }
    }
}

/// Remap DeepSeek `apply_patch` arguments (`mode`, `path`, `content`)
/// to the shape Codex expects (`filepath`, `patch`).
/// Build the `action` object Codex expects for a `local_shell_call` item.
/// Codex's `LocalShellAction` is a RootModel around `ExecLocalShellAction`,
/// which requires `command` to be a **list of strings** (argv) and `type: "exec"`.
fn local_shell_call_action(args: &serde_json::Value) -> serde_json::Value {
    let command_str = match args {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(o) => o
            .get("command")
            .and_then(|v| v.as_str())
            .or_else(|| o.get("cmd").and_then(|v| v.as_str()))
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    // Codex 0.142.5's local_shell_call `action` schema uses a single `cmd`
    // string (not an argv `command` array); emitting `command` as a list is
    // rejected with "missing field `cmd`".
    serde_json::json!({ "type": "exec", "cmd": command_str })
}

/// Build a `local_shell_call` output item exactly as Codex's
/// `LocalShellCallResponseItem` schema expects: only `id`, `type`, `status`,
/// `call_id`, and `action` — NO `name`/`arguments` (those cause Codex to reject
/// the item). `action` is the `ExecLocalShellAction` described above.
fn build_local_shell_item(
    item_id: &str,
    call_id: &str,
    status: &str,
    args: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": item_id,
        "type": "local_shell_call",
        "status": status,
        "call_id": call_id,
        "action": local_shell_call_action(args),
    })
}

fn remap_apply_patch_args(mut args: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = args.as_object_mut() {
        obj.remove("mode");
        if let Some(path) = obj.remove("path") {
            obj.insert("filepath".into(), path);
        }
        if let Some(content) = obj.remove("content") {
            obj.insert("patch".into(), content);
        }
    }
    args
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
        let parsed: serde_json::Value = serde_json::from_str(result[6..].trim()).unwrap();
        assert_eq!(parsed["kind"], "text_delta");
    }

    #[test]
    fn format_sse_chunk_multi_event_batch() {
        let events = vec![text_event("a"), text_event("b")];
        let result = format_sse_chunk(&events).unwrap().unwrap();
        assert!(result.starts_with("data: "));
        assert!(result.ends_with("\n\n"));
        let parsed: serde_json::Value = serde_json::from_str(result[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
        assert_eq!(v["id"], "chatcmpl-abc");
        assert_eq!(v["model"], "gpt-4");
        assert_eq!(v["object"], "chat.completion.chunk");
    }

    #[test]
    fn openai_sse_text_delta() {
        let s = encode_openai_sse(&text_event("Hello"));
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
        assert_eq!(v["choices"][0]["delta"]["content"], "Hello");
    }

    #[test]
    fn openai_sse_reasoning_delta() {
        let event = StreamEvent::ReasoningDelta {
            text: "thinking...".into(),
        };
        let s = encode_openai_sse(&event);
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
        assert!(v["choices"][0]["delta"]["tool_calls"][0]["index"].is_number());
        assert!(
            v["choices"][0]["delta"]["tool_calls"][0]["function"].is_object(),
            "ToolCallEnd must include function object for OpenAI-compatible clients"
        );
    }

    #[test]
    fn openai_sse_finish_stop() {
        let s = encode_openai_sse(&finish_event(FinishReason::Stop, None));
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
        assert_eq!(v["responseId"], "g-id");
    }

    #[test]
    fn gemini_sse_text_delta() {
        let s = encode_gemini_sse(&text_event("Hello"));
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
        let v: serde_json::Value = serde_json::from_str(s[6..].trim()).unwrap();
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
            serde_json::from_str(encode_gemini_sse(&event)[6..].trim()).unwrap();
        assert_eq!(v["error"]["message"], "err");
    }

    // ── encode_openai_responses_sse ─────────────────────────────────

    #[test]
    fn openai_responses_sse_start_event() {
        let event = StreamEvent::Start {
            id: "rsp_1".into(),
            model: "gpt-4o".into(),
        };
        let mut state = ResponsesSseState::default();
        let frames = encode_openai_responses_sse(&event, &mut state, "test");
        // Start must emit 4 frames: response.created, response.in_progress,
        // response.output_item.added, response.content_part.added
        assert_eq!(frames.len(), 4, "expected 4 setup frames, got {frames:?}");
        assert!(
            frames[0].contains("event: response.created\n"),
            "frame[0]: {}",
            frames[0]
        );
        let v: serde_json::Value = serde_json::from_str(extract_data(&frames[0]).unwrap()).unwrap();
        assert_eq!(v["type"], "response.created");
        assert_eq!(v["response"]["id"], "rsp_1");
    }

    #[test]
    fn openai_responses_sse_full_lifecycle() {
        let start = StreamEvent::Start {
            id: "rsp_1".into(),
            model: "gpt-4o".into(),
        };
        let mut state = ResponsesSseState::default();
        let setup = encode_openai_responses_sse(&start, &mut state, "test");
        assert_eq!(setup.len(), 4);

        // Subsequent TextDelta should not re-emit setup frames.
        let delta1 = encode_openai_responses_sse(&text_event("hello"), &mut state, "test");
        assert_eq!(delta1.len(), 1, "delta should not re-emit setup");
        let v: serde_json::Value = serde_json::from_str(extract_data(&delta1[0]).unwrap()).unwrap();
        assert_eq!(v["type"], "response.output_text.delta");
        assert_eq!(v["delta"], "hello");

        // Finish should close with content_part.done + output_item.done + response.completed
        let finish = encode_openai_responses_sse(
            &finish_event(FinishReason::Stop, None),
            &mut state,
            "test",
        );
        assert!(
            finish.len() >= 2,
            "expected at least {finish:?} closing frames, got {finish:?}"
        );
        let last = &finish[finish.len() - 1];
        assert!(
            last.contains("response.completed"),
            "last frame must be response.completed, got {last}"
        );
    }

    #[test]
    fn openai_responses_sse_text_delta_idempotent_setup() {
        // Calling TextDelta before Start must still emit setup, using
        // whatever id is in the state (empty strings if nothing was set).
        let mut state = ResponsesSseState::default();
        let frames = encode_openai_responses_sse(&text_event("hi"), &mut state, "test");
        assert!(!frames.is_empty());
        // First frames are setup, last is the delta
        let delta_frame = &frames[frames.len() - 1];
        let v: serde_json::Value =
            serde_json::from_str(extract_data(delta_frame).unwrap()).unwrap();
        assert_eq!(v["type"], "response.output_text.delta");
        assert_eq!(v["delta"], "hi");
    }

    #[test]
    fn openai_responses_sse_reasoning_delta() {
        let start = StreamEvent::Start {
            id: "rsp_1".into(),
            model: "gpt-4o".into(),
        };
        let mut state = ResponsesSseState::default();
        encode_openai_responses_sse(&start, &mut state, "test");

        let frames =
            encode_openai_responses_sse(&reasoning_event("thinking..."), &mut state, "test");
        assert_eq!(frames.len(), 1);
        assert!(
            frames[0].contains("response.reasoning_summary_text.delta"),
            "ReasoningDelta must use response.reasoning_summary_text.delta, got: {}",
            frames[0]
        );
    }

    /// Extract the `data:` portion from an SSE frame.
    /// Works for both `data: <json>\n\n` and `event: X\ndata: <json>\n\n`.
    fn extract_data(sse: &str) -> Option<&str> {
        let data_start = sse.find("data: ")?;
        let after_data = &sse[data_start + 6..];
        after_data.strip_suffix("\n\n")
    }

    /// Simulate a Codex-style agentic loop: the SAME `npm create vite`
    /// tool call repeated across consecutive `/v1/responses` turns.
    /// Codex sends each turn as a separate request with no stable
    /// conversation anchor and a rotating `previous_response_id`, so the
    /// per-run `run_key` is (in the real bug) UNIQUE per turn. This test
    /// pins a fixed `run_key` to model a correctly-correlated run, and
    /// verifies the GLOBAL identical-repeat guard (keyed by normalized
    /// command signature, independent of run_key) catches the loop after
    /// `LOOP_GUARD_MAX_REPEATS` repeats and suppresses further calls with a
    /// truthful `response.completed` instead of relaying the tool call.
    /// Serializes loop-guard tests. The guards key on process-global statics
    /// (`GLOBAL_SIG_LOOPS`, `CONV_LOOPS`, `LAST_SIG_PER_RUN`, `CALL_ID_MAP`),
    /// and `reset_loop_guards` clears the *entire* maps. Without this lock,
    /// parallel sibling tests wipe each other's in-flight counters mid-loop,
    /// making the suppression turn nondeterministic. Each loop-guard test
    /// acquires this lock before resetting state.
    static LOOP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Reset ALL loop-guard state so tests are hermetic. The guard keys on
    /// three statics (GLOBAL_SIG_LOOPS, CONV_LOOPS, LAST_SIG_PER_RUN); a test
    /// that resets only one leaks state into the next and makes the suite
    /// order-dependent. Call this at the start of every loop-guard test.
    fn reset_loop_guards() {
        use crate::streaming::{CONV_LOOPS, GLOBAL_SIG_LOOPS, LAST_SIG_PER_RUN};
        GLOBAL_SIG_LOOPS.lock().unwrap().clear();
        CONV_LOOPS.lock().unwrap().clear();
        LAST_SIG_PER_RUN.lock().unwrap().clear();
    }

    #[test]
    fn loop_guard_suppresses_repeated_npm_create_vite() {
        let _lock = LOOP_TEST_LOCK.lock().unwrap();
        // Reset global state so the test is hermetic.
        reset_loop_guards();

        let run_key = "run-fixed-codex-test";
        let cmd =
            serde_json::json!({ "command": "npm create vite@latest hippo -- --template react" });

        // Drive enough turns to exceed LOOP_GUARD_MAX_REPEATS (8).
        let mut suppressed_on_turn: Option<usize> = None;
        for turn in 1..=12 {
            let mut state = ResponsesSseState::default();
            let _setup = encode_openai_responses_sse(
                &StreamEvent::Start {
                    id: format!("rsp_{turn}"),
                    model: "mock".into(),
                },
                &mut state,
                run_key,
            );
            let call = ToolCall {
                id: format!("fc_{turn}"),
                name: "exec_command".into(),
                arguments: cmd.clone(),
            };
            let start_frames = encode_openai_responses_sse(
                &StreamEvent::ToolCallStart { call: call.clone() },
                &mut state,
                run_key,
            );
            // A suppress_active guard returns a single response.completed
            // frame with no function_call. Otherwise it relays the call.
            let relayed = start_frames.iter().any(|f| {
                f.contains("function_call") || f.contains("response.function_call_arguments")
            });
            let completed = start_frames
                .iter()
                .any(|f| f.contains("response.completed"));
            if completed && !relayed {
                suppressed_on_turn = Some(turn);
                // The suppression completion must carry a truthful loop msg.
                let joined = start_frames.concat();
                assert!(
                    joined.contains("repeated") || joined.contains("Stopping"),
                    "suppression frame must explain the loop, got: {joined}"
                );
                break;
            }
            // Normal relay: emit delta + end so the guard records the call.
            encode_openai_responses_sse(
                &StreamEvent::ToolCallDelta {
                    id: call.id.clone(),
                    arguments_fragment: serde_json::to_string(&cmd).unwrap(),
                },
                &mut state,
                run_key,
            );
            encode_openai_responses_sse(
                &StreamEvent::ToolCallEnd {
                    id: call.id.clone(),
                },
                &mut state,
                run_key,
            );
            encode_openai_responses_sse(
                &finish_event(FinishReason::Stop, None),
                &mut state,
                run_key,
            );
        }

        assert!(
            suppressed_on_turn.is_some(),
            "expected the loop guard to suppress the repeated `npm create vite` call"
        );
        assert!(
            suppressed_on_turn.unwrap() <= 9,
            "loop should be caught by the 9th repeat, got turn {}",
            suppressed_on_turn.unwrap()
        );
    }

    /// The REAL Codex regression: Codex drives each agentic turn as a
    /// SEPARATE `/v1/responses` request with a rotating `previous_response_id`
    /// and (in the OpenAI Responses pattern) sends only the incremental
    /// `input`, so the bridge-derived `run_key` (hash of first user message)
    /// is UNIQUE per turn. The OLD per-run guard keyed on `run_key` and
    /// therefore never accumulated repeats -> `npm create vite` looped
    /// forever. This test reproduces that exact condition (a fresh
    /// `run_key` every turn) and verifies the GLOBAL signature guard still
    /// catches the loop.
    #[test]
    fn loop_guard_suppresses_npm_create_vite_with_unstable_run_key() {
        let _lock = LOOP_TEST_LOCK.lock().unwrap();
        use crate::streaming::GLOBAL_SIG_LOOPS;
        {
            let mut g = GLOBAL_SIG_LOOPS.lock().unwrap();
            g.clear();
        }
        reset_loop_guards();

        let cmd = serde_json::json!({ "command": "npm create vite@latest hippo -y" });

        let mut suppressed_on_turn: Option<usize> = None;
        for turn in 1..=12 {
            // Fresh, unique run_key every turn — mirrors Codex's real behavior.
            let run_key = format!("run-unstable-{turn}");
            let mut state = ResponsesSseState::default();
            encode_openai_responses_sse(
                &StreamEvent::Start {
                    id: format!("rsp_{turn}"),
                    model: "mock".into(),
                },
                &mut state,
                &run_key,
            );
            let call = ToolCall {
                id: format!("fc_{turn}"),
                name: "exec_command".into(),
                arguments: cmd.clone(),
            };
            let start_frames = encode_openai_responses_sse(
                &StreamEvent::ToolCallStart { call: call.clone() },
                &mut state,
                &run_key,
            );
            let relayed = start_frames.iter().any(|f| {
                f.contains("function_call") || f.contains("response.function_call_arguments")
            });
            let completed = start_frames
                .iter()
                .any(|f| f.contains("response.completed"));
            if completed && !relayed {
                suppressed_on_turn = Some(turn);
                break;
            }
            encode_openai_responses_sse(
                &StreamEvent::ToolCallDelta {
                    id: call.id.clone(),
                    arguments_fragment: serde_json::to_string(&cmd).unwrap(),
                },
                &mut state,
                &run_key,
            );
            encode_openai_responses_sse(
                &StreamEvent::ToolCallEnd {
                    id: call.id.clone(),
                },
                &mut state,
                &run_key,
            );
            encode_openai_responses_sse(
                &finish_event(FinishReason::Stop, None),
                &mut state,
                &run_key,
            );
        }

        assert!(
            suppressed_on_turn.is_some(),
            "GLOBAL guard must catch the loop even when run_key is unique per turn (the real Codex bug)"
        );
        assert!(
            suppressed_on_turn.unwrap() <= 9,
            "loop should be caught by the 9th repeat, got turn {}",
            suppressed_on_turn.unwrap()
        );
    }

    /// Mirrors the EXACT live Codex behavior observed in the bridge logs:
    /// Codex streams the `command` argument via `ToolCallDelta`, so at
    /// `ToolCallStart` `arguments` is EMPTY (`exec_command:cmd=` placeholder)
    /// and the real signature only becomes known at `ToolCallEnd`. The
    /// `run_key` (hash of the first user message) is STABLE across turns.
    /// This test verifies the placeholder-exec_command path still suppresses
    /// the loop via the run-scoped `CONV_LOOPS` guard.
    #[test]
    fn loop_guard_suppresses_placeholder_exec_command_stable_run_key() {
        let _lock = LOOP_TEST_LOCK.lock().unwrap();
        use crate::streaming::GLOBAL_SIG_LOOPS;
        {
            let mut g = GLOBAL_SIG_LOOPS.lock().unwrap();
            g.clear();
        }
        reset_loop_guards();

        let run_key = "run-live-codex-stable";
        let _cmd = serde_json::json!({ "command": "npm create vite@latest hippo" });

        let mut suppressed_on_turn: Option<usize> = None;
        for turn in 1..=12 {
            let mut state = ResponsesSseState::default();
            encode_openai_responses_sse(
                &StreamEvent::Start {
                    id: format!("rsp_{turn}"),
                    model: "mock".into(),
                },
                &mut state,
                run_key,
            );
            // Placeholder: empty arguments at ToolCallStart (real Codex).
            let call = ToolCall {
                id: format!("fc_{turn}"),
                name: "exec_command".into(),
                arguments: serde_json::Value::Object(Default::default()),
            };
            let start_frames = encode_openai_responses_sse(
                &StreamEvent::ToolCallStart { call: call.clone() },
                &mut state,
                run_key,
            );
            let relayed = start_frames.iter().any(|f| {
                f.contains("function_call") || f.contains("response.function_call_arguments")
            });
            let completed = start_frames
                .iter()
                .any(|f| f.contains("response.completed"));
            if completed && !relayed {
                suppressed_on_turn = Some(turn);
                let joined = start_frames.concat();
                assert!(
                    joined.contains("repeated") || joined.contains("Stopping"),
                    "suppression frame must explain the loop, got: {joined}"
                );
                break;
            }
            // Command arrives at ToolCallEnd (real Codex streaming). The
            // placeholder command's REAL signature becomes known here, so
            // the precise per-signature guard suppresses THIS specific
            // repeating command at ToolCallEnd (not blindly at ToolCallStart).
            let end_frames = encode_openai_responses_sse(
                &StreamEvent::ToolCallEnd {
                    id: call.id.clone(),
                },
                &mut state,
                run_key,
            );
            let end_completed = end_frames.iter().any(|f| f.contains("response.completed"));
            let end_relayed = end_frames.iter().any(|f| {
                f.contains("function_call") || f.contains("response.function_call_arguments")
            });
            if end_completed && !end_relayed {
                suppressed_on_turn = Some(turn);
                let joined = end_frames.concat();
                assert!(
                    joined.contains("repeated") || joined.contains("Stopping"),
                    "suppression frame must explain the loop, got: {joined}"
                );
                break;
            }
            encode_openai_responses_sse(
                &finish_event(FinishReason::Stop, None),
                &mut state,
                run_key,
            );
        }

        assert!(
            suppressed_on_turn.is_some(),
            "placeholder exec_command loop must be suppressed via run-scoped CONV_LOOPS (real Codex case)"
        );
        assert!(
            suppressed_on_turn.unwrap() <= 9,
            "loop should be caught by the 9th repeat, got turn {}",
            suppressed_on_turn.unwrap()
        );
    }

    /// A FAILING command that Codex retries (e.g. transient DNS EAI_AGAIN)
    /// WILL be suppressed after LOOP_GUARD_MAX_REPEATS retries. The previous
    /// design cleared the counter on every tool result, but that defeated the
    /// guard entirely for any command that completed (even with "Operation
    /// cancelled"). Now the counter is never cleared — 8 failures in 120s
    /// is a real problem worth stopping.
    ///
    /// Uses an entirely different command (`python -c "raise ..."`) so the
    /// normalized signature does NOT collide with the `npm create vite` sig
    /// of the other loop-guard tests under parallel execution.
    #[test]
    fn loop_guard_suppresses_failing_retry_after_max() {
        let _lock = LOOP_TEST_LOCK.lock().unwrap();
        use crate::streaming::{CALL_ID_MAP, GLOBAL_SIG_LOOPS};
        {
            let mut g = GLOBAL_SIG_LOOPS.lock().unwrap();
            g.clear();
        }
        reset_loop_guards();
        {
            let mut m = CALL_ID_MAP.lock().unwrap();
            m.clear();
        }

        let run_key = "run-failing-retry";
        let cmd = serde_json::json!({ "command": "python -c 'raise SystemExit(1)'" });

        let call_id = |turn: usize| format!("fc_failing_retry_{}", turn);

        let mut suppressed_on_turn: Option<usize> = None;
        for turn in 1..=15 {
            let mut state = ResponsesSseState::default();
            let _setup = encode_openai_responses_sse(
                &StreamEvent::Start {
                    id: format!("rsp_{turn}"),
                    model: "mock".into(),
                },
                &mut state,
                run_key,
            );
            let call = ToolCall {
                id: call_id(turn),
                name: "exec_command".into(),
                arguments: cmd.clone(),
            };
            let start_frames = encode_openai_responses_sse(
                &StreamEvent::ToolCallStart { call: call.clone() },
                &mut state,
                run_key,
            );
            let relayed = start_frames.iter().any(|f| {
                f.contains("function_call") || f.contains("response.function_call_arguments")
            });
            let completed = start_frames
                .iter()
                .any(|f| f.contains("response.completed"));
            if completed && !relayed {
                suppressed_on_turn = Some(turn);
                break;
            }
            encode_openai_responses_sse(
                &StreamEvent::ToolCallDelta {
                    id: call.id.clone(),
                    arguments_fragment: serde_json::to_string(&cmd).unwrap(),
                },
                &mut state,
                run_key,
            );
            encode_openai_responses_sse(
                &StreamEvent::ToolCallEnd {
                    id: call.id.clone(),
                },
                &mut state,
                run_key,
            );
            encode_openai_responses_sse(
                &finish_event(FinishReason::Stop, None),
                &mut state,
                run_key,
            );
        }

        assert!(
            suppressed_on_turn.is_some(),
            "a failing command retried 8+ times SHOULD be suppressed"
        );
        assert!(
            suppressed_on_turn.unwrap() >= 3,
            "should not suppress before LOOP_GUARD_MAX_REPEATS (8), got turn {}",
            suppressed_on_turn.unwrap()
        );
    }
}
