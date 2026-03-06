//! Shared SSE (Server-Sent Events) parser with buffer limits.
//!
//! Provides a generic, incremental SSE frame parser used by all protocol
//! drivers that consume SSE streams (AG-UI, A2A client, MCP client).
//! Enforces maximum buffer and data sizes to prevent OOM from malicious
//! servers.
//!
//! See TJ-SPEC-016 §9.2, TJ-SPEC-017 §NFR-004, TJ-SPEC-018 F-001.

// ============================================================================
// Constants
// ============================================================================

/// Maximum line-accumulation buffer size (16 MiB).
///
/// If the buffer exceeds this limit, it is drained and a
/// `BufferOverflow` error is returned.
///
/// Implements: TJ-SPEC-016 F-001
const MAX_BUFFER_SIZE: usize = 16 * 1024 * 1024;

/// Maximum accumulated `data:` field size (4 MiB).
///
/// If the data exceeds this limit, the current event is discarded
/// and a `DataOverflow` error is returned.
///
/// Implements: TJ-SPEC-016 F-001
const MAX_DATA_SIZE: usize = 4 * 1024 * 1024;

// ============================================================================
// SseParseError
// ============================================================================

/// Errors that can occur during SSE parsing.
///
/// Implements: TJ-SPEC-016 F-001
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseParseError {
    /// The line-accumulation buffer exceeded `MAX_BUFFER_SIZE`.
    BufferOverflow,
    /// The accumulated `data:` field exceeded `MAX_DATA_SIZE`.
    DataOverflow,
}

impl std::fmt::Display for SseParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferOverflow => write!(
                f,
                "SSE buffer overflow: line buffer exceeded {MAX_BUFFER_SIZE} bytes"
            ),
            Self::DataOverflow => write!(
                f,
                "SSE data overflow: accumulated data exceeded {MAX_DATA_SIZE} bytes"
            ),
        }
    }
}

impl std::error::Error for SseParseError {}

// ============================================================================
// RawSseEvent
// ============================================================================

/// A raw SSE event with optional `event:` type and accumulated `data:` content.
///
/// Callers are responsible for JSON deserialization and type mapping.
///
/// Implements: TJ-SPEC-016 F-001
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSseEvent {
    /// The SSE `event:` field value, if present.
    pub event_type: Option<String>,
    /// The accumulated `data:` field content (joined with newlines).
    pub data: String,
}

// ============================================================================
// SseParser
// ============================================================================

/// Incremental SSE frame parser with buffer limits.
///
/// Reads bytes incrementally, accumulates lines, and yields complete
/// `RawSseEvent` values. Enforces maximum buffer and data sizes to
/// prevent unbounded memory growth from malicious servers.
///
/// Implements: TJ-SPEC-016 F-001
pub struct SseParser {
    /// Line accumulation buffer.
    buffer: String,
    /// Current SSE `event:` field value.
    current_event_type: Option<String>,
    /// Accumulated `data:` field content (may span multiple lines).
    current_data: String,
    /// Ignore all remaining lines in the current event after an overflow.
    discard_current_event: bool,
}

impl SseParser {
    /// Creates a new SSE parser with default buffer limits.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: String::new(),
            current_event_type: None,
            current_data: String::new(),
            discard_current_event: false,
        }
    }

    /// Feed raw bytes into the parser and extract any complete events.
    ///
    /// Returns a `Vec` of results — each is either a successfully parsed
    /// `RawSseEvent` or an `SseParseError` if a buffer limit was exceeded.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Result<RawSseEvent, SseParseError>> {
        let text = String::from_utf8_lossy(bytes);

        // Check buffer overflow before appending
        if self.buffer.len() + text.len() > MAX_BUFFER_SIZE {
            self.reset_current_event();
            self.discard_current_event = true;
            self.buffer.clear();
            return vec![Err(SseParseError::BufferOverflow)];
        }

        self.buffer.push_str(&text);

        let mut events = Vec::new();

        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos]
                .trim_end_matches('\r')
                .to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();
            self.process_line(&line, &mut events);
        }

        events
    }

    /// Flush any partially buffered event when the byte stream ends.
    pub fn finish(&mut self) -> Vec<Result<RawSseEvent, SseParseError>> {
        let mut events = Vec::new();

        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.process_line(line.trim_end_matches('\r'), &mut events);
        }

        if self.discard_current_event {
            self.discard_current_event = false;
            self.reset_current_event();
            return events;
        }

        if let Some(event) = self.dispatch_event() {
            events.push(event);
        }

        events
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<Result<RawSseEvent, SseParseError>>) {
        if line.is_empty() {
            if self.discard_current_event {
                self.discard_current_event = false;
                self.reset_current_event();
            } else if let Some(event) = self.dispatch_event() {
                events.push(event);
            }
            return;
        }

        if self.discard_current_event {
            return;
        }

        if let Some(value) = line.strip_prefix("event:") {
            self.current_event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            let trimmed = value.trim_start();
            let new_len = self.current_data.len()
                + usize::from(!self.current_data.is_empty())
                + trimmed.len();
            if new_len > MAX_DATA_SIZE {
                self.discard_current_event = true;
                self.reset_current_event();
                events.push(Err(SseParseError::DataOverflow));
            } else {
                if !self.current_data.is_empty() {
                    self.current_data.push('\n');
                }
                self.current_data.push_str(trimmed);
            }
        } else if line.starts_with(':') {
            // SSE comment — ignore
        }
    }

    fn reset_current_event(&mut self) {
        self.current_data.clear();
        self.current_event_type = None;
    }

    /// Dispatch an accumulated event (called on blank line).
    fn dispatch_event(&mut self) -> Option<Result<RawSseEvent, SseParseError>> {
        if self.current_data.is_empty() && self.current_event_type.is_none() {
            return None;
        }

        let event_type = self.current_event_type.take();
        let data = std::mem::take(&mut self.current_data);

        if data.is_empty() {
            return None;
        }

        Some(Ok(RawSseEvent { event_type, data }))
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_data_event() {
        let mut parser = SseParser::new();
        let input = b"data: {\"key\":\"value\"}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert!(event.event_type.is_none());
        assert_eq!(event.data, "{\"key\":\"value\"}");
    }

    #[test]
    fn parse_event_with_type() {
        let mut parser = SseParser::new();
        let input = b"event: RUN_STARTED\ndata: {\"ok\":true}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.event_type.as_deref(), Some("RUN_STARTED"));
        assert_eq!(event.data, "{\"ok\":true}");
    }

    #[test]
    fn parse_multiline_data() {
        let mut parser = SseParser::new();
        let input = b"data: {\"key\":\ndata: \"value\"}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.data, "{\"key\":\n\"value\"}");
    }

    #[test]
    fn parse_multiple_events() {
        let mut parser = SseParser::new();
        let input = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap().data, "{\"a\":1}");
        assert_eq!(events[1].as_ref().unwrap().data, "{\"b\":2}");
    }

    #[test]
    fn parse_incremental_chunks() {
        let mut parser = SseParser::new();

        let events1 = parser.feed(b"data: {\"res");
        assert!(events1.is_empty());

        let events2 = parser.feed(b"ult\":true}\n\n");
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].as_ref().unwrap().data, "{\"result\":true}");
    }

    #[test]
    fn parse_comment_ignored() {
        let mut parser = SseParser::new();
        let input = b": keepalive\ndata: {\"ok\":true}\n\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
    }

    #[test]
    fn parse_empty_lines_no_event() {
        let mut parser = SseParser::new();
        let input = b"\n\n";
        let events = parser.feed(input);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_cr_lf_line_endings() {
        let mut parser = SseParser::new();
        let input = b"data: {\"ok\":true}\r\n\r\n";
        let events = parser.feed(input);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap().data, "{\"ok\":true}");
    }

    #[test]
    fn buffer_overflow_returns_error() {
        let mut parser = SseParser::new();
        // Feed a huge chunk that exceeds MAX_BUFFER_SIZE
        let huge = vec![b'x'; MAX_BUFFER_SIZE + 1];
        let events = parser.feed(&huge);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], Err(SseParseError::BufferOverflow));
        // Buffer should be cleared
        assert!(parser.buffer.is_empty());
    }

    #[test]
    fn data_overflow_returns_error() {
        let mut parser = SseParser::new();
        // Build data that exceeds MAX_DATA_SIZE across multiple data: lines
        let chunk_size = MAX_DATA_SIZE / 2 + 1;
        let big_data = "x".repeat(chunk_size);
        let input = format!("data: {big_data}\ndata: {big_data}\n\n");
        let events = parser.feed(input.as_bytes());

        // Should contain a DataOverflow error
        assert!(
            events
                .iter()
                .any(|e| e == &Err(SseParseError::DataOverflow))
        );
    }

    #[test]
    fn recovery_after_buffer_overflow() {
        let mut parser = SseParser::new();
        let huge = vec![b'x'; MAX_BUFFER_SIZE + 1];
        let _ = parser.feed(&huge);

        // Buffer overflow discards the rest of the current event until a blank line.
        let events = parser.feed(b"data: {\"ignored\":true}\n\n");
        assert!(events.is_empty());

        // After the record boundary, subsequent events parse normally.
        let events = parser.feed(b"data: {\"recovered\":true}\n\n");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
    }

    #[test]
    fn recovery_after_data_overflow() {
        let mut parser = SseParser::new();
        let big_data = "x".repeat(MAX_DATA_SIZE + 1);
        let input = format!("data: {big_data}\n\n");
        let _ = parser.feed(input.as_bytes());

        // Parser should recover
        let events = parser.feed(b"data: {\"recovered\":true}\n\n");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
    }

    #[test]
    fn default_creates_empty_parser() {
        let parser = SseParser::default();
        assert!(parser.buffer.is_empty());
        assert!(parser.current_event_type.is_none());
        assert!(parser.current_data.is_empty());
    }

    // ---- Property Tests ----

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_sse_frame() -> impl Strategy<Value = Vec<u8>> {
            (1..=100_i64).prop_map(|id| format!("data: {{\"id\":{id}}}\n\n").into_bytes())
        }

        fn arb_sse_stream_with_splits() -> impl Strategy<Value = (Vec<u8>, Vec<usize>)> {
            prop::collection::vec(arb_sse_frame(), 1..6).prop_flat_map(|frames| {
                let stream: Vec<u8> = frames.into_iter().flatten().collect();
                let len = stream.len();
                let splits = prop::collection::vec(0..len, 1..8).prop_map(|mut pts| {
                    pts.sort_unstable();
                    pts.dedup();
                    pts
                });
                (Just(stream), splits)
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn prop_chunk_independence(
                (stream, splits) in arb_sse_stream_with_splits()
            ) {
                let mut one_shot = SseParser::new();
                let one_shot_ok: Vec<_> = one_shot
                    .feed(&stream)
                    .into_iter()
                    .filter_map(Result::ok)
                    .collect();

                let mut chunked = SseParser::new();
                let mut chunked_ok: Vec<_> = Vec::new();
                let mut prev = 0;
                for &split in &splits {
                    if split > prev {
                        chunked_ok.extend(
                            chunked.feed(&stream[prev..split]).into_iter().filter_map(Result::ok),
                        );
                        prev = split;
                    }
                }
                chunked_ok.extend(
                    chunked.feed(&stream[prev..]).into_iter().filter_map(Result::ok),
                );

                prop_assert_eq!(one_shot_ok.len(), chunked_ok.len(),
                    "chunk independence: one-shot={}, chunked={}",
                    one_shot_ok.len(), chunked_ok.len());
            }

            #[test]
            fn prop_no_panic(data in prop::collection::vec(any::<u8>(), 0..512)) {
                let mut parser = SseParser::new();
                let _ = parser.feed(&data);
            }
        }
    }
}
