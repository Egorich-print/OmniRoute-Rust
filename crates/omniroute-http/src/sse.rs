//! Minimal SSE event decoder.
//!
//! Splits a byte/text stream into `(event, data)` frames. Handles split
//! chunks, `\r\n` line endings, multi-line `data:` (joined with `\n`) and
//! comment/keepalive lines (`: ...`), which are dropped. A `data: [DONE]`
//! sentinel is surfaced as a normal event so the caller can terminate.

/// One decoded SSE frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` name, if the frame carried one.
    pub event: Option<String>,
    /// The concatenated `data:` payload.
    pub data: String,
}

/// Incremental decoder: feed text chunks, drain complete frames.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    /// Empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push text, returning every frame completed by this chunk.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(&chunk.replace("\r\n", "\n"));
        let mut events = Vec::new();
        while let Some(index) = self.buffer.find("\n\n") {
            let frame: String = self.buffer.drain(..index + 2).collect();
            if let Some(event) = parse_frame(&frame) {
                events.push(event);
            }
        }
        events
    }
}

fn parse_frame(frame: &str) -> Option<SseEvent> {
    let mut event: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in frame.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(name) = line.strip_prefix("event:") {
            event = Some(name.trim().to_string());
        } else if let Some(payload) = line.strip_prefix("data:") {
            // Per SSE, one leading space after the colon is stripped.
            data_lines.push(payload.strip_prefix(' ').unwrap_or(payload));
        }
    }
    if event.is_none() && data_lines.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_frames() {
        let mut decoder = SseDecoder::new();
        // Split mid-frame, mid-line and mid-`\n\n`.
        let first = decoder.push("event: response.output_text.del");
        assert!(first.is_empty());
        let second = decoder.push("ta\ndata: {\"delta\": \"Hi\"}\n");
        assert!(second.is_empty());
        let third = decoder.push("\ndata: {\"delta\": \"!\"}\n\n");
        assert_eq!(third.len(), 2);
        assert_eq!(
            third[0].event.as_deref(),
            Some("response.output_text.delta")
        );
        assert_eq!(third[0].data, "{\"delta\": \"Hi\"}");
        assert_eq!(third[1].event, None);
        assert_eq!(third[1].data, "{\"delta\": \"!\"}");
    }

    #[test]
    fn drops_comments_and_handles_crlf() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(": keepalive\r\nevent: ping\r\ndata: hi\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("ping"));
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn joins_multiline_data() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push("data: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }
}
