//! Reasoning-tag extraction helpers.
//!
//! Some upstreams embed a model's "thinking" / chain-of-thought
//! **inline** in the regular `content` field rather than in a
//! separate `reasoning_content` field. This module strips the tag
//! pairs (`<!--reasoning-->`, `<reasoning>`, `<thinking>`) and
//! classifies the enclosed content as `Reasoning`.

/// A single segment of a split text payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningSplit {
    /// Plain visible text.
    Text(String),
    /// Reasoning / chain-of-thought content. Maps to `ContentPart::Reasoning`
    /// on the universal schema and `StreamEvent::ReasoningDelta` on the
    /// streaming schema.
    Reasoning(String),
}

/// Split a single text payload into `(text | reasoning)` segments.
///
/// Recognised tags (case-insensitive):
/// - `<!--reasoning-->...<!--/reasoning-->`
/// - `<reasoning>...</reasoning>`
/// - `<!--thinking-->...<!--/thinking-->`
/// - `<thinking>...</thinking>`
/// - `<!--think-->...<!--/think-->`
/// - `<!--thought-->...<!--/thought-->`
/// - `<thought>...</thought>`
/// - `<!--reflect-->...<!--/reflect-->`
/// - `<reflect>...</reflect>`
///
/// Behaviour:
/// - The opening tag's exact spelling determines the close.
///   `<!--reasoning-->` matches `<!--/reasoning-->`, NOT `</reasoning>`.
/// - Unclosed opening tags are treated as reasoning until end-of-payload.
/// - Empty tags (`<!--reasoning--><!--/reasoning-->`) are skipped (no
///   empty `ReasoningSplit::Reasoning` is emitted, matching how the
///   non-streaming decoder skips empty `reasoning_content` strings).
/// - Whitespace and newlines inside the tag are preserved verbatim;
///   only the tag characters themselves are consumed.
pub fn split_reasoning(text: &str) -> Vec<ReasoningSplit> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut in_reasoning: Option<&'static str> = None; // the OPENING tag spelling
    while !rest.is_empty() {
        if let Some(open) = in_reasoning {
            // Inside reasoning — search for the matching close
            // (case-insensitive — the upstream may spell the close
            // tag in a different case than the open).
            let close = close_tag_for(open);
            if let Some(idx) = find_ci(rest.as_bytes(), close.as_bytes()) {
                let inside = &rest[..idx];
                push_reasoning(&mut out, inside);
                rest = &rest[idx + close.len()..];
                in_reasoning = None;
            } else {
                // Unclosed — treat the remainder as reasoning.
                push_reasoning(&mut out, rest);
                rest = "";
                in_reasoning = None;
            }
        } else {
            // Outside reasoning — find the next opening tag.
            match next_open_tag(rest) {
                Some((idx, open)) => {
                    if idx > 0 {
                        out.push(ReasoningSplit::Text(rest[..idx].to_string()));
                    }
                    rest = &rest[idx + open.len()..];
                    in_reasoning = Some(open);
                }
                None => {
                    if !rest.is_empty() {
                        out.push(ReasoningSplit::Text(rest.to_string()));
                    }
                    rest = "";
                }
            }
        }
    }
    out
}

fn push_reasoning(out: &mut Vec<ReasoningSplit>, text: &str) {
    if !text.is_empty() {
        out.push(ReasoningSplit::Reasoning(text.to_string()));
    }
}

/// Returns the closing tag spelling for a given opening tag.
/// `<!--reasoning-->` → `<!--/reasoning-->`, `<reasoning>` → `</reasoning>`,
/// `<!--thinking-->` → `<!--/thinking-->`, `<thinking>` → `</thinking>`,
/// `<!--think-->` → `<!--/think-->`, `<!--thought-->` → `<!--/thought-->`,
/// `<thought>` → `</thought>`, `<!--reflect-->` → `<!--/reflect-->`,
/// `<reflect>` → `</reflect>`.
fn close_tag_for(open: &str) -> &'static str {
    match open {
        "<!--reasoning-->" => "<!--/reasoning-->",
        "<!--thinking-->" => "<!--/thinking-->",
        "<!--think-->" => "<!--/think-->",
        "<!--thought-->" => "<!--/thought-->",
        "<!--reflect-->" => "<!--/reflect-->",
        "<!--antthinking-->" => "<!--/antthinking-->",
        "<!--analysis-->" => "<!--/analysis-->",
        "<reasoning>" => "</reasoning>",
        "<thinking>" => "</thinking>",
        "<thought>" => "</thought>",
        "<reflect>" => "</reflect>",
        "<antthinking>" => "</antthinking>",
        "<analysis>" => "</analysis>",
        // Canonical DeepSeek-shape opener (7 bytes: `think`)
        // MUST close with `think>` so the streamer matches the
        // upstream close tag exactly.
        "<think>" => "</think>",
        // Canonical openers used when the streamer sees a partial
        // opener split across chunks (e.g. `<think` mid-chunk).
        // They map to the matching close the rest of the pipeline
        // expects to see at the end of the reasoning block.
        "<think" => "</think>",
        "<thinking" => "</thinking>",
        _ => "</reasoning>",
    }
}

/// Locate the next FULL opening tag in `text` (the earliest of any
/// recognised reasoning-tag opener, case-insensitive). Returns
/// `(byte_index, canonical_tag_spelling)` so the matching close
/// tag can be looked up via [`close_tag_for`].
///
/// Only complete openers are matched here. Partial prefixes
/// (e.g. `<think` mid-chunk) are handled by [`is_tag_prefix`] and
/// held back in the streamer's carry buffer; including them here
/// would consume too few bytes and leave a stray trailing byte at
/// the start of the reasoning payload.
///
/// Recognised full openers:
/// `<!--reasoning-->`, `<!--thinking-->`, `<!--thought-->`,
/// `<!--reflect-->`, `<!--antthinking-->`, `<!--analysis-->`,
/// `<!--think-->`, `<reasoning>`, `<thinking>`, `<thought>`,
/// `<reflect>`, `<antthinking>`, `<analysis>`, `think`.
fn next_open_tag(text: &str) -> Option<(usize, &'static str)> {
    let bytes = text.as_bytes();
    let needles: &[(&[u8], &'static str)] = &[
        (b"<!--reasoning-->", "<!--reasoning-->"),
        (b"<!--thinking-->", "<!--thinking-->"),
        (b"<!--thought-->", "<!--thought-->"),
        (b"<!--reflect-->", "<!--reflect-->"),
        (b"<!--antthinking-->", "<!--antthinking-->"),
        (b"<!--analysis-->", "<!--analysis-->"),
        (b"<reasoning>", "<reasoning>"),
        (b"<thinking>", "<thinking>"),
        (b"<thought>", "<thought>"),
        (b"<reflect>", "<reflect>"),
        (b"<antthinking>", "<antthinking>"),
        (b"<analysis>", "<analysis>"),
        (b"<!--think-->", "<!--think-->"),
        // `think` (7 bytes — `<`, `t`, `h`, `i`, `n`, `k`, `>`) is the
        // original DeepSeek-shape opener. Listed last so longer
        // matchers (e.g. `<!--think-->`) win on tie. MUST include the
        // leading `<` and trailing `>` or the matcher will leave a
        // stray `<` at the start of the payload.
        (b"<think>", "<think>"),
    ];
    // Pick the earliest match; on ties, pick the longest match
    // (more specific opener wins).
    let mut best: Option<(usize, usize, &'static str)> = None;
    for (needle, canon) in needles {
        if let Some(idx) = find_ci(bytes, needle) {
            best = match best {
                Some((b, n, c)) if b < idx => Some((b, n, c)),
                Some((b, n, c)) if b == idx && n >= needle.len() => Some((b, n, c)),
                _ => Some((idx, needle.len(), *canon)),
            };
        }
    }
    best.map(|(idx, _, canon)| (idx, canon))
}

fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if eq_ci(&haystack[i..i + needle.len()], needle) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn eq_ci(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// ---------------------------------------------------------------------------
// Streaming variant
// ---------------------------------------------------------------------------

/// Internal state for [`ReasoningStreamer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamerState {
    /// Currently outside any reasoning tag.
    Normal,
    /// Inside a reasoning tag. `open` is the canonical opening-tag
    /// spelling (so we can look up the matching close).
    InReasoning { open: &'static str },
}

/// Stateful reasoning-tag streamer. Created once per upstream SSE
/// stream; call [`ReasoningStreamer::feed`] for each chunk of text
/// received from the upstream, and [`ReasoningStreamer::flush`] at
/// end-of-stream to drain any unclosed opening tag.
#[derive(Debug, Clone)]
pub struct ReasoningStreamer {
    state: StreamerState,
    /// Carry buffer: text that might still be the prefix of a tag.
    /// We hold it back from emission until we can decide whether it
    /// is a tag or just visible text.
    carry: String,
    /// Maximum carry-buffer length before we force-clear. Guards
    /// against pathological upstreams that never close a tag —
    /// without this cap, the carry could grow without bound.
    carry_cap: usize,
}

impl Default for ReasoningStreamer {
    fn default() -> Self {
        Self::new()
    }
}

impl ReasoningStreamer {
    /// Create a new streamer. The default carry cap is 64 KiB, which
    /// is generous enough to absorb any sane tag-prefix split but
    /// small enough to prevent runaway memory if the upstream never
    /// closes the tag.
    pub fn new() -> Self {
        Self::with_carry_cap(64 * 1024)
    }

    /// Constructor with a custom carry-buffer cap. Used by tests.
    pub fn with_carry_cap(carry_cap: usize) -> Self {
        Self {
            state: StreamerState::Normal,
            carry: String::new(),
            carry_cap,
        }
    }

    /// Feed a chunk of upstream text. Returns the splits to emit
    /// for THIS chunk. Empty carries, half-tags, and tag boundaries
    /// are handled internally.
    pub fn feed(&mut self, text: &str) -> Vec<ReasoningSplit> {
        let mut out = Vec::new();
        if text.is_empty() && self.carry.is_empty() {
            return out;
        }
        let mut buf = std::mem::take(&mut self.carry);
        buf.push_str(text);

        loop {
            match self.state {
                StreamerState::Normal => {
                    // Find the next opening tag.
                    match next_open_tag(&buf) {
                        Some((idx, open)) => {
                            // Emit text up to the tag opening.
                            if idx > 0 {
                                out.push(ReasoningSplit::Text(buf[..idx].to_string()));
                            }
                            // Check if the OPENING tag itself is complete in
                            // this chunk. If not, hold the suffix and wait.
                            if idx + open.len() > buf.len() {
                                self.carry = buf.split_off(idx);
                                out.extend(self.enforce_cap(self.carry.len()));
                                return out;
                            }
                            // Opening tag is complete — switch state.
                            let after_open = idx + open.len();
                            buf = buf.split_off(after_open);
                            self.state = StreamerState::InReasoning { open };
                        }
                        None => {
                            // No tag found. Hold back a suffix that
                            // *might* be the prefix of a tag so we
                            // don't emit it just to learn it was a
                            // tag in the next chunk.
                            let hold = hold_back(&buf);
                            let cut = buf.len() - hold;
                            if cut > 0 {
                                out.push(ReasoningSplit::Text(buf[..cut].to_string()));
                            }
                            self.carry = buf[cut..].to_string();
                            out.extend(self.enforce_cap(self.carry.len()));
                            return out;
                        }
                    }
                }
                StreamerState::InReasoning { open } => {
                    let close = close_tag_for(open);
                    match find_ci(buf.as_bytes(), close.as_bytes()) {
                        Some(idx) => {
                            let inside = buf[..idx].to_string();
                            push_reasoning(&mut out, &inside);
                            let after_close = idx + close.len();
                            buf = buf.split_off(after_close);
                            self.state = StreamerState::Normal;
                        }
                        None => {
                            let hold = (close.len() - 1).min(buf.len());
                            let cut = buf.len() - hold;
                            let inside = &buf[..cut];
                            push_reasoning(&mut out, inside);
                            self.carry = buf[cut..].to_string();
                            out.extend(self.enforce_cap(self.carry.len()));
                            return out;
                        }
                    }
                }
            }
        }
    }

    /// Drain any buffered text and close out an unclosed opening
    /// tag. Call this exactly once when the upstream stream ends.
    /// If the carry ends inside an open tag, the remaining text is
    /// emitted as `Reasoning` (mirroring `split_reasoning`'s
    /// unclosed-tag behaviour).
    pub fn flush(&mut self) -> Vec<ReasoningSplit> {
        let mut out = Vec::new();
        let tail = std::mem::take(&mut self.carry);
        if tail.is_empty() {
            return out;
        }
        match self.state {
            StreamerState::Normal => {
                out.push(ReasoningSplit::Text(tail));
            }
            StreamerState::InReasoning { .. } => {
                push_reasoning(&mut out, &tail);
            }
        }
        out
    }

    /// Flush the carry buffer as plain text or reasoning (depending
    /// on the current state) when it exceeds the cap, then clear it.
    /// This bounds memory without silently dropping data. The carry
    /// is emitted through the returned splits so the caller can
    /// extend its output vector.
    fn enforce_cap(&mut self, current_carry_len: usize) -> Vec<ReasoningSplit> {
        if current_carry_len > self.carry_cap {
            let tail = std::mem::take(&mut self.carry);
            let mut out = Vec::new();
            match self.state {
                StreamerState::Normal => {
                    out.push(ReasoningSplit::Text(tail));
                }
                StreamerState::InReasoning { .. } => {
                    push_reasoning(&mut out, &tail);
                }
            }
            out
        } else {
            Vec::new()
        }
    }
}

/// Maximum byte length of any tag we recognise. Used as the
/// hold-back window so a half-tag doesn't leak out as visible text
/// while we wait for the next chunk.
const TAG_MAX_LEN: usize = {
    let a = "<reasoning>".len();
    let b = "<!--reasoning-->".len();
    let c = "<think>".len();
    let d = "<!--antthinking-->".len();
    let e = "<!--analysis-->".len();
    let mut m = if a > b { a } else { b };
    if c > m {
        m = c;
    }
    if d > m {
        m = d;
    }
    if e > m {
        m = e;
    }
    m
};

/// Return how many trailing bytes of `buf` should be withheld
/// because they *might* be the prefix of a tag. The check is
/// case-insensitive and operates on bytes.
fn hold_back(buf: &str) -> usize {
    let bytes = buf.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let max = TAG_MAX_LEN.saturating_sub(1);
    let n = bytes.len().min(max);
    // Largest first: if the last `k` bytes match a needle prefix,
    // return that k. Stop scanning as soon as we find a match.
    for k in (1..=n).rev() {
        if is_tag_prefix(&bytes[bytes.len() - k..]) {
            return k;
        }
    }
    0
}

/// Does `tail` form a prefix of any recognised OPENER (case-insensitive)?
///
/// We require the tail to start with `<` because every recognised
/// opener does (`<!--...-->`, `<reasoning>`, `<thinking>`, etc).
/// Without this guard a single-character tail like `"t"` would match
/// any needle that starts with `t` (e.g. `think>`), causing the
/// streamer to withhold one byte of trailing visible text from
/// every chunk — observed as a one-character truncation bug in
/// `streamer_partial_opening_tag_at_end`.
fn is_tag_prefix(tail: &[u8]) -> bool {
    if tail.is_empty() || tail[0] != b'<' {
        return false;
    }
    for needle in [
        b"<!--reasoning-->" as &[u8],
        b"<!--thinking-->",
        b"<!--thought-->",
        b"<!--reflect-->",
        b"<!--antthinking-->",
        b"<!--analysis-->",
        b"<reasoning>",
        b"<thinking>",
        b"<thought>",
        b"<reflect>",
        b"<antthinking>",
        b"<analysis>",
        b"<!--think-->",
    ] {
        if tail.len() <= needle.len() && eq_ci(tail, &needle[..tail.len()]) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Per-stream state holder
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Per-stream `ReasoningStreamer` state, keyed by the request
/// pointer (`*const UniversalRequest as usize`). Stored in a
/// process-wide `Mutex` (rather than a `thread_local!`) because the
/// gateway's `async_stream!` may resume on a different Tokio worker
/// thread between SSE frames — a thread-local cell would lose state
/// across that thread hop. Each entry is removed when the stream
/// sees a `Finish` event so memory does not leak across long-running
/// servers.
static STREAMERS: OnceLock<Mutex<HashMap<usize, ReasoningStreamer>>> = OnceLock::new();

fn streamers() -> &'static Mutex<HashMap<usize, ReasoningStreamer>> {
    STREAMERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Feed a chunk of text into the per-request streamer and return
/// the splits to emit. Creates a new streamer on first call for
/// a given request.
///
/// Use [`streamer_finish`] when a `Finish` event is observed to
/// drain any pending state and remove the per-request entry.
pub(crate) fn streamer_feed(request_ptr: usize, text: &str) -> Vec<ReasoningSplit> {
    let mut map = streamers().lock().unwrap_or_else(|e| e.into_inner());
    let streamer = map.entry(request_ptr).or_default();
    streamer.feed(text)
}

/// Drain the per-request streamer's pending state. Returns the
/// splits to emit at end-of-stream (handles the unclosed-tag
/// case where the carry should be classified as reasoning).
/// The per-request entry is removed so the map does not leak
/// entries across streams.
pub(crate) fn streamer_finish(request_ptr: usize) -> Vec<ReasoningSplit> {
    let mut map = streamers().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut streamer) = map.remove(&request_ptr) {
        streamer.flush()
    } else {
        Vec::new()
    }
}

/// Drop the per-request streamer entry without emitting anything.
/// Used when the adapter sees a non-`Finish` reason that ends the
/// stream (e.g. an upstream error) so the map does not accumulate
/// dead entries. Called from `autorouter-server::upstream` when the
/// upstream byte stream errors out or completes without producing a
/// `Finish` event.
pub fn streamer_drop(request_ptr: usize) {
    let mut map = streamers().lock().unwrap_or_else(|e| e.into_inner());
    map.remove(&request_ptr);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> ReasoningSplit {
        ReasoningSplit::Text(s.to_string())
    }
    fn r(s: &str) -> ReasoningSplit {
        ReasoningSplit::Reasoning(s.to_string())
    }

    #[test]
    fn split_empty() {
        assert!(split_reasoning("").is_empty());
    }

    #[test]
    fn split_plain_text() {
        assert_eq!(split_reasoning("hello world"), vec![text("hello world")]);
    }

    #[test]
    fn split_html_comment_tag() {
        let out = split_reasoning("<!--reasoning-->foo<!--/reasoning-->bar");
        assert_eq!(out, vec![r("foo"), text("bar")]);
    }

    #[test]
    fn split_tag_with_leading_and_trailing_text() {
        let out = split_reasoning("before <!--reasoning-->middle<!--/reasoning--> after");
        assert_eq!(out, vec![text("before "), r("middle"), text(" after")]);
    }

    #[test]
    fn split_simple_tag() {
        let out = split_reasoning("<reasoning>foo</reasoning>bar");
        assert_eq!(out, vec![r("foo"), text("bar")]);
    }

    #[test]
    fn split_think_tag() {
        // The `<think>...</think>` shape is emitted by DeepSeek and
        // several open-source models; treat it as a reasoning tag.
        let out = split_reasoning("<think>chain of thought</think>reply");
        assert_eq!(out, vec![r("chain of thought"), text("reply")]);
    }

    #[test]
    fn split_think_tag_with_surrounding_text() {
        let out = split_reasoning("hi <think>deep thought</think> bye");
        assert_eq!(out, vec![text("hi "), r("deep thought"), text(" bye")]);
    }

    #[test]
    fn split_think_tag_unclosed_is_reasoning() {
        let out = split_reasoning("<think>dangling");
        assert_eq!(out, vec![r("dangling")]);
    }

    #[test]
    fn streamer_think_tag_split_across_chunks() {
        let mut s = ReasoningStreamer::new();
        // Build the test string in a way that doesn't double the leading `<`.
        // (Earlier versions used `String::from("<") + "<think>..."`, which
        // produced `<think>...` because the literal already started with `<`.)
        let full = String::from("<think>deep") + "</think>tail";
        let pre: String = full.chars().take(4).collect();
        let mid: String = full.chars().take(13).skip(4).collect();
        let suf: String = full.chars().skip(13).collect();
        let mut all = Vec::new();
        all.extend(s.feed(&pre));
        all.extend(s.feed(&mid));
        all.extend(s.feed(&suf));
        all.extend(s.flush());
        assert_eq!(all, vec![r("deep"), text("tail")]);
    }

    #[test]
    fn split_multiple_blocks() {
        let out = split_reasoning(
            "<!--reasoning-->a<!--/reasoning--> mid <!--reasoning-->b<!--/reasoning--> end",
        );
        assert_eq!(out, vec![r("a"), text(" mid "), r("b"), text(" end")]);
    }

    #[test]
    fn split_case_insensitive() {
        let out = split_reasoning("<!--REASONING-->x<!--/REASONING-->");
        assert_eq!(out, vec![r("x")]);
    }

    #[test]
    fn split_unclosed_is_reasoning() {
        let out = split_reasoning("<!--reasoning-->never closed");
        assert_eq!(out, vec![r("never closed")]);
    }

    #[test]
    fn split_empty_tag_skipped() {
        let out = split_reasoning("<!--reasoning--><!--/reasoning-->after");
        assert_eq!(out, vec![text("after")]);
    }

    #[test]
    fn split_preserves_whitespace() {
        let out = split_reasoning("<!--reasoning-->\n\n hi \n<!--/reasoning-->");
        assert_eq!(out, vec![r("\n\n hi \n")]);
    }

    #[test]
    fn split_mismatched_tags_do_not_cross_close() {
        // `<!--reasoning-->` MUST close on `<!--/reasoning-->`, NOT on
        // `</reasoning>`.
        let out = split_reasoning("<!--reasoning-->inside</reasoning>trailing");
        // Because `<!--reasoning-->` never finds its actual close, the
        // entire remainder (including `</reasoning>`) is treated as
        // unclosed reasoning.
        assert_eq!(out, vec![r("inside</reasoning>trailing")]);
    }

    #[test]
    fn split_text_mentioning_reasoning_word_passthrough() {
        let out = split_reasoning("the <reasoning tag is not closed");
        assert_eq!(out, vec![text("the <reasoning tag is not closed")]);
    }

    // -- Streaming variant --

    #[test]
    fn streamer_simple() {
        let mut s = ReasoningStreamer::new();
        let out = s.feed("<!--reasoning-->foo<!--/reasoning-->bar");
        assert_eq!(out, vec![r("foo"), text("bar")]);
        assert!(s.flush().is_empty());
    }

    #[test]
    fn streamer_split_across_chunks() {
        let mut s = ReasoningStreamer::new();
        // The full tag is `<!--reasoning-->` (17 bytes). We split it
        // across three chunks to exercise the carry / partial-opener
        // logic. We construct the partial strings via slicing from a
        // literal that already contains the correct bytes so the
        // leading tag never appears as a contiguous incomplete
        // substring in the source (which would be confusing).
        let full = String::from("<") + "!--reasoning-->foo<!--/reasoning-->bar";
        let pre: String = full.chars().take(12).collect();
        let mid: String = full.chars().take(30).skip(12).collect();
        let suf: String = full.chars().take(48).skip(30).collect();
        let last: String = full.chars().skip(48).collect();
        let mut all = Vec::new();
        all.extend(s.feed(&pre));
        all.extend(s.feed(&mid));
        all.extend(s.feed(&suf));
        all.extend(s.feed(&last));
        all.extend(s.flush());
        assert_eq!(all, vec![r("foo"), text("bar")]);
    }

    #[test]
    fn streamer_partial_opening_tag_at_end() {
        let mut s = ReasoningStreamer::new();
        // Feed the prefix that ends mid-tag. The hold-back logic
        // should withhold the last `<r` (a case-insensitive prefix of
        // `<reasoning>`), so the visible chunk emits `hello ` and the
        // carry keeps `<r`.
        let first = s.feed("hello <r");
        assert_eq!(first, vec![text("hello ")]);
        // When more arrives and it turns out to be plain text
        // (`andom text`), the entire carry + new text must be
        // emitted as visible text (no tag was completed).
        let rest = s.feed("andom text");
        assert_eq!(rest, vec![text("<random text")]);
        assert!(s.flush().is_empty());
    }

    #[test]
    fn streamer_unclosed_at_flush_emits_reasoning() {
        let mut s = ReasoningStreamer::new();
        let mut all = Vec::new();
        all.extend(s.feed("<!--reasoning-->the user"));
        all.extend(s.feed(" wants hi"));
        all.extend(s.flush());
        // The unclosed reasoning block must be flushed as Reasoning.
        // The streamer may split the reasoning text across chunks
        // because the close tag is held back as a possible prefix;
        // both pieces must round-trip into `Reasoning` events so the
        // final concatenation equals the original reasoning text.
        let combined_reasoning: String = all
            .iter()
            .filter_map(|s| match s {
                ReasoningSplit::Reasoning(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(combined_reasoning, "the user wants hi");
    }

    #[test]
    fn streamer_no_tags_passes_through() {
        let mut s = ReasoningStreamer::new();
        let out = s.feed("just a normal response, nothing fancy");
        assert_eq!(out, vec![text("just a normal response, nothing fancy")]);
        assert!(s.flush().is_empty());
    }

    #[test]
    fn streamer_cap_forces_clear_on_pathological_input() {
        // Build a streamer with a tiny carry cap so we can verify
        // the cap triggers without generating megabytes of input.
        let mut s = ReasoningStreamer::with_carry_cap(8);
        let mut buf = String::from("<reasoning>");
        // Push enough text (no closing tag ever) to overflow
        // the carry cap. The exact emitted shape isn't asserted —
        // we only verify the streamer survives and `flush` returns.
        for _ in 0..32 {
            buf.push_str("abcdefghij");
        }
        let _ = s.feed(&buf);
        let _ = s.flush();
    }

    #[test]
    fn streamer_multiple_blocks_across_chunks() {
        let mut s = ReasoningStreamer::new();
        let mut all = Vec::new();
        all.extend(s.feed("<!--reasoning-->a<!"));
        all.extend(s.feed("<!--/reasoning--> mid <reasoning"));
        all.extend(s.feed(">b</reasoning> end"));
        all.extend(s.flush());
        // The first reasoning block contains `a<!` because the
        // streamer's hold-back logic kept the trailing `<!` while
        // waiting for the next chunk. When the next chunk arrived
        // it merged into `<!--/reasoning-->`, completing the close.
        // The second block is the bare `b`. The final emitted stream
        // is therefore `a<!`, ` mid `, `b`, ` end` — note the
        // reasoning chunks concatenate to the original payload text.
        let combined_reasoning: String = all
            .iter()
            .filter_map(|s| match s {
                ReasoningSplit::Reasoning(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(combined_reasoning, "a<!b");
        assert_eq!(all, vec![r("a<!"), text(" mid "), r("b"), text(" end")]);
    }

    #[test]
    fn streamer_open_tag_only_split_within_opener() {
        // The opening tag itself can split: `<!--reason` then `ing-->`.
        // The carry holds the opener until it completes. We build the
        // partial string from a fully-spelled literal so the leading
        // tag never appears as a contiguous incomplete substring.
        let full = String::from("before <") + "!--reasoning-->foo<!--/reasoning-->bar";
        let pre: String = full.chars().take(19).collect();
        let post: String = full.chars().skip(19).collect();
        let mut s = ReasoningStreamer::new();
        let mut all = Vec::new();
        let first = s.feed(&pre);
        // The opener is incomplete: `before ` (7 chars) is visible
        // text, `<!--reason` (12 chars) is held in the carry.
        assert_eq!(first, vec![text("before ")]);
        all.extend(first);
        all.extend(s.feed(&post));
        all.extend(s.flush());
        assert_eq!(all, vec![text("before "), r("foo"), text("bar")]);
    }

    // ====================================================================
    // Inline reasoning tag tests. These cover the case where an upstream
    // provider embeds reasoning content inline in the content field
    // instead of using a separate reasoning_content field. The
    // split-reasoning helper must lift the tagged region into Reasoning
    // and leave the surrounding text as visible Text.
    // ====================================================================

    #[test]
    fn split_reasoning_handles_simple_think_block() {
        // <think> and </think> are the canonical DeepSeek / Qwen
        // QwQ reasoning tag pair. The inside of the block is
        // classified as Reasoning; surrounding text remains Text.
        let input = std::str::from_utf8(b"<think>foo</think>bar").unwrap();
        let out = split_reasoning(input);
        assert_eq!(out, vec![r("foo"), text("bar")]);
    }

    #[test]
    fn split_reasoning_handles_thinking_tag_variant() {
        // <thinking>...</thinking> is the same shape with a longer
        // tag name. Must close only on </thinking>, never on
        // </think> or </reasoning>.
        let input = std::str::from_utf8(b"<thinking>foo</thinking>bar").unwrap();
        let out = split_reasoning(input);
        assert_eq!(out, vec![r("foo"), text("bar")]);
    }

    #[test]
    fn split_reasoning_handles_multiple_blocks() {
        // Two <think>...</think> blocks separated by visible text.
        let input = std::str::from_utf8(b"<think>a</think> middle <think>b</think> end").unwrap();
        let out = split_reasoning(input);
        assert_eq!(out, vec![r("a"), text(" middle "), r("b"), text(" end")]);
    }

    #[test]
    fn split_reasoning_handles_case_insensitive() {
        // Mixed-case opener. Must match (the upstream may emit
        // uppercase tags by accident).
        let out = split_reasoning("<THINK>x");
        assert_eq!(out, vec![r("x")]);
    }

    #[test]
    fn split_reasoning_unclosed_remainder_is_reasoning() {
        // If the upstream truncates the response mid-thought
        // (common with token-limit-exceeded errors), the remainder
        // is still treated as reasoning so the user can see what
        // the model was trying to say.
        let input = std::str::from_utf8(b"<think>foo").unwrap();
        let out = split_reasoning(input);
        assert_eq!(out, vec![r("foo")]);
    }

    #[test]
    fn split_reasoning_no_tags_passthrough() {
        // A response with no reasoning tags is returned verbatim
        // as a single Text segment. No false-positive tag matches.
        let out = split_reasoning("plain answer");
        assert_eq!(out, vec![text("plain answer")]);
    }
}
