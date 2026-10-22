//! Framing for streamed response bodies: turning the bytes of a
//! `text/event-stream` or newline-delimited JSON body into a sequence of
//! events.
//!
//! The decoders are incremental, so they can serve a response that is read to
//! completion as well as one consumed as it arrives.

use mime::Mime;
use serde_json_bytes::Value;

/// How a streamed response body is divided into events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFraming {
    /// `text/event-stream`, per the WHATWG Server-Sent Events specification.
    ServerSentEvents,
    /// One JSON value per line: `application/x-ndjson`, `application/jsonl`
    /// and their relatives.
    JsonLines,
}

impl StreamFraming {
    /// Picks the framing for a response's `Content-Type`, or `None` when the
    /// body is not a stream.
    pub fn from_content_type(content_type: &Mime) -> Option<Self> {
        match (
            content_type.type_().as_str(),
            content_type.subtype().as_str(),
        ) {
            ("text", "event-stream") => Some(Self::ServerSentEvents),
            ("application", "x-ndjson" | "ndjson" | "jsonl" | "x-jsonlines" | "jsonlines") => {
                Some(Self::JsonLines)
            }
            _ => None,
        }
    }

    pub fn decoder(self) -> StreamDecoder {
        match self {
            Self::ServerSentEvents => StreamDecoder::Sse(SseDecoder::default()),
            Self::JsonLines => StreamDecoder::JsonLines(LineSplitter::default()),
        }
    }
}

/// One event from a streamed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    /// The SSE `event:` field. Always `None` for JSON Lines.
    pub event: Option<String>,
    /// The SSE `id:` field. Always `None` for JSON Lines.
    pub id: Option<String>,
    /// The event's payload: the joined `data:` lines for SSE, or the line
    /// itself for JSON Lines.
    pub data: String,
}

impl StreamEvent {
    /// The payload parsed as JSON, or `None` if it isn't JSON (an SSE
    /// sentinel like `[DONE]`, for example).
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.data).ok()
    }
}

#[derive(Debug)]
pub enum StreamDecoder {
    Sse(SseDecoder),
    JsonLines(LineSplitter),
}

impl StreamDecoder {
    /// Feeds the next chunk of the body, returning every event it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<StreamEvent> {
        match self {
            Self::Sse(decoder) => decoder.push(chunk),
            Self::JsonLines(lines) => lines
                .push(chunk)
                .into_iter()
                .filter_map(json_line_event)
                .collect(),
        }
    }

    /// Signals the end of the body, returning any event left unterminated.
    ///
    /// For SSE that is always empty: the specification discards an event that
    /// the stream ends before dispatching. A final JSON line, on the other
    /// hand, needs no trailing newline.
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        match self {
            Self::Sse(_) => Vec::new(),
            Self::JsonLines(lines) => lines
                .finish()
                .and_then(json_line_event)
                .into_iter()
                .collect(),
        }
    }
}

fn json_line_event(line: String) -> Option<StreamEvent> {
    (!line.trim().is_empty()).then_some(StreamEvent {
        event: None,
        id: None,
        data: line,
    })
}

/// The JSON payloads of every event in a complete streamed body, in order.
///
/// Events whose payload isn't JSON (an SSE `[DONE]` sentinel, a ping) are
/// dropped, so a selection like `$->last` lands on the last real event.
pub fn json_events(framing: StreamFraming, body: &[u8]) -> Vec<Value> {
    let mut decoder = framing.decoder();
    let mut events = decoder.push(body);
    events.extend(decoder.finish());
    events.iter().filter_map(StreamEvent::json).collect()
}

/// Splits a byte stream into lines, accepting `\n`, `\r\n` and a lone `\r`
/// as line endings, as the SSE specification requires.
#[derive(Debug, Default)]
pub struct LineSplitter {
    buffer: Vec<u8>,
    /// A `\r` ended the previous chunk, so a `\n` opening the next one belongs
    /// to the same line ending.
    pending_cr: bool,
}

impl LineSplitter {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in chunk {
            if self.pending_cr {
                self.pending_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' => lines.push(self.take_line()),
                b'\r' => {
                    lines.push(self.take_line());
                    self.pending_cr = true;
                }
                _ => self.buffer.push(byte),
            }
        }
        lines
    }

    fn finish(&mut self) -> Option<String> {
        (!self.buffer.is_empty()).then(|| self.take_line())
    }

    fn take_line(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned()
    }
}

/// An incremental Server-Sent Events decoder, following the "interpret an
/// event stream" algorithm of the WHATWG HTML specification.
#[derive(Debug, Default)]
pub struct SseDecoder {
    lines: LineSplitter,
    event: Option<String>,
    data: Option<String>,
    /// The last event ID seen, which persists across events until reset.
    last_id: Option<String>,
    /// Whether the leading byte-order mark, if any, has been dealt with.
    started: bool,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<StreamEvent> {
        let chunk = if self.started {
            chunk
        } else {
            self.started = !chunk.is_empty();
            chunk.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(chunk)
        };
        self.lines
            .push(chunk)
            .into_iter()
            .filter_map(|line| self.process_line(&line))
            .collect()
    }

    fn process_line(&mut self, line: &str) -> Option<StreamEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            "id" if !value.contains('\0') => {
                self.last_id = (!value.is_empty()).then(|| value.to_string());
            }
            // `retry` only matters to a client that reconnects, and unknown
            // fields are ignored by the specification.
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<StreamEvent> {
        let event = self.event.take().filter(|event| !event.is_empty());
        let data = self.data.take()?;
        Some(StreamEvent {
            event,
            id: self.last_id.clone(),
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    fn decode_sse(chunks: &[&[u8]]) -> Vec<StreamEvent> {
        let mut decoder = StreamFraming::ServerSentEvents.decoder();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(chunk));
        }
        events.extend(decoder.finish());
        events
    }

    fn data_of(events: &[StreamEvent]) -> Vec<&str> {
        events.iter().map(|event| event.data.as_str()).collect()
    }

    #[test]
    fn content_types_select_a_framing() {
        let framing = |ct: &str| StreamFraming::from_content_type(&ct.parse().unwrap());
        assert_eq!(
            framing("text/event-stream; charset=utf-8"),
            Some(StreamFraming::ServerSentEvents)
        );
        assert_eq!(
            framing("application/x-ndjson"),
            Some(StreamFraming::JsonLines)
        );
        assert_eq!(framing("application/jsonl"), Some(StreamFraming::JsonLines));
        assert_eq!(framing("application/json"), None);
        assert_eq!(framing("text/plain"), None);
    }

    #[test]
    fn sse_events_carry_their_type_id_and_joined_data() {
        let events = decode_sse(&[
            b": a comment\n",
            b"event: price\nid: 7\ndata: {\"a\":\ndata: 1}\n\n",
            b"data: second\n\n",
        ]);
        assert_eq!(
            events,
            vec![
                StreamEvent {
                    event: Some("price".into()),
                    id: Some("7".into()),
                    data: "{\"a\":\n1}".into(),
                },
                // The ID persists, and the event type does not.
                StreamEvent {
                    event: None,
                    id: Some("7".into()),
                    data: "second".into(),
                },
            ]
        );
    }

    #[test]
    fn sse_survives_events_and_line_endings_split_across_chunks() {
        let events = decode_sse(&[b"da", b"ta: one\r", b"\n\r", b"\ndata:two\r\r"]);
        assert_eq!(data_of(&events), vec!["one", "two"]);
    }

    #[test]
    fn sse_drops_an_event_the_stream_ends_before_dispatching() {
        let events = decode_sse(&[b"data: done\n\ndata: cut off\n"]);
        assert_eq!(data_of(&events), vec!["done"]);
    }

    #[test]
    fn sse_strips_a_leading_byte_order_mark() {
        let events = decode_sse(&[b"\xEF\xBB\xBFdata: x\n\n"]);
        assert_eq!(data_of(&events), vec!["x"]);
    }

    #[test]
    fn json_lines_needs_no_trailing_newline_and_skips_blank_lines() {
        let mut decoder = StreamFraming::JsonLines.decoder();
        let mut events = decoder.push(b"{\"a\":1}\n\n{\"a\"");
        events.extend(decoder.push(b":2}"));
        events.extend(decoder.finish());
        assert_eq!(data_of(&events), vec!["{\"a\":1}", "{\"a\":2}"]);
    }

    /// The shape of an MCP `tools/call` answered with an SSE stream: progress
    /// notifications, then the JSON-RPC response that ends the stream.
    #[test]
    fn an_mcp_tool_call_stream_yields_every_message() {
        let body = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":1}}\n",
            "\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"structuredContent\":{\"temp\":18}}}\n",
            "\n",
        );
        assert_eq!(
            json_events(StreamFraming::ServerSentEvents, body.as_bytes()),
            vec![
                json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}}),
                json!({"jsonrpc": "2.0", "id": 1, "result": {"structuredContent": {"temp": 18}}}),
            ]
        );
    }

    #[test]
    fn json_events_drop_non_json_sentinels() {
        let body = b"data: {\"n\":1}\n\ndata: [DONE]\n\ndata: {\"n\":2}\n\n";
        assert_eq!(
            json_events(StreamFraming::ServerSentEvents, body),
            vec![json!({"n": 1}), json!({"n": 2})]
        );
    }
}
