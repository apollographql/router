#![allow(unreachable_pub)]

use std::collections::VecDeque;
use std::convert::TryFrom;
use std::str::from_utf8;

use hyper::body::Bytes;

use super::error::Error;
use super::error::Result;

#[derive(Default, PartialEq)]
struct EventData {
    pub(crate) event_type: String,
    pub(crate) data: String,
    pub(crate) id: Option<String>,
    pub(crate) retry: Option<u64>,
}

impl EventData {
    fn new() -> Self {
        Self::default()
    }

    pub(crate) fn append_data(&mut self, value: &str) {
        self.data.push_str(value);
        self.data.push('\n');
    }

    pub(crate) fn with_id(mut self, value: Option<String>) -> Self {
        self.id = value;
        self
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Sse {
    Event(Event),
    Comment(String),
}

impl TryFrom<EventData> for Option<Sse> {
    type Error = Error;

    fn try_from(event_data: EventData) -> std::result::Result<Self, Self::Error> {
        if event_data == EventData::default() {
            return Err(Error::InvalidEvent);
        }

        if event_data.data.is_empty() {
            return Ok(None);
        }

        let event_type = if event_data.event_type.is_empty() {
            String::from("message")
        } else {
            event_data.event_type
        };

        let mut data = event_data.data.clone();
        data.truncate(data.len() - 1);

        let id = event_data.id.clone();

        let retry = event_data.retry;

        Ok(Some(Sse::Event(Event {
            event_type,
            data,
            id,
            retry,
        })))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub event_type: String,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

const LOGIFY_MAX_CHARS: usize = 100;
fn logify(bytes: &[u8]) -> &str {
    let stringified = from_utf8(bytes).unwrap_or("<bad utf8>");
    if stringified.len() <= LOGIFY_MAX_CHARS {
        stringified
    } else {
        &stringified[..LOGIFY_MAX_CHARS - 1]
    }
}

fn parse_field(line: &[u8]) -> Result<Option<(&str, &str)>> {
    if line.is_empty() {
        return Err(Error::InvalidLine(
            "should never try to parse an empty line (probably a bug)".into(),
        ));
    }

    match line.iter().position(|&b| b':' == b) {
        Some(0) => {
            let value = &line[1..];
            tracing::debug!("comment: {}", logify(value));
            Ok(Some(("comment", parse_value(value)?)))
        }
        Some(colon_pos) => {
            let key = &line[0..colon_pos];
            let key = parse_key(key)?;

            let mut value = &line[colon_pos + 1..];
            // remove the first initial space character if any (but remove no other whitespace)
            if value.starts_with(b" ") {
                value = &value[1..];
            }

            tracing::debug!("key: {}, value: {}", key, logify(value));

            Ok(Some((key, parse_value(value)?)))
        }
        None => Ok(Some((parse_key(line)?, ""))),
    }
}

fn parse_key(key: &[u8]) -> Result<&str> {
    from_utf8(key).map_err(|e| Error::InvalidLine(format!("malformed key: {:?}", e)))
}

fn parse_value(value: &[u8]) -> Result<&str> {
    from_utf8(value).map_err(|e| Error::InvalidLine(format!("malformed value: {:?}", e)))
}

#[must_use = "streams do nothing unless polled"]
pub struct EventParser {
    /// buffer for lines we know are complete (terminated) but not yet parsed into event fields, in
    /// the order received
    complete_lines: VecDeque<Vec<u8>>,
    /// buffer for the most-recently received line, pending completion (by a newline terminator) or
    /// extension (by more non-newline bytes)
    incomplete_line: Option<Vec<u8>>,
    /// flagged if the last character processed as a carriage return; used to help process CRLF
    /// pairs
    last_char_was_cr: bool,
    /// the event data currently being decoded
    event_data: Option<EventData>,
    /// the last-seen event ID; events without an ID will take on this value until it is updated.
    last_event_id: Option<String>,
    sse: VecDeque<Sse>,
}

impl EventParser {
    pub fn new() -> Self {
        Self {
            complete_lines: VecDeque::with_capacity(10),
            incomplete_line: None,
            last_char_was_cr: false,
            event_data: None,
            last_event_id: None,
            sse: VecDeque::with_capacity(3),
        }
    }

    pub fn was_processing(&self) -> bool {
        if self.incomplete_line.is_some() || !self.complete_lines.is_empty() {
            true
        } else {
            !self.sse.is_empty()
        }
    }

    pub fn get_event(&mut self) -> Option<Sse> {
        self.sse.pop_front()
    }

    pub fn process_bytes(&mut self, bytes: Bytes) -> Result<()> {
        tracing::trace!("Parsing bytes {:?}", bytes);
        // We get bytes from the underlying stream in chunks.  Decoding a chunk has two phases:
        // decode the chunk into lines, and decode the lines into events.
        //
        // We counterintuitively do these two phases in reverse order. Because both lines and
        // events may be split across chunks, we need to ensure we have a complete
        // (newline-terminated) line before parsing it, and a complete event
        // (empty-line-terminated) before returning it. So we buffer lines between poll()
        // invocations, and begin by processing any incomplete events from previous invocations,
        // before requesting new input from the underlying stream and processing that.

        self.decode_and_buffer_lines(bytes);
        self.parse_complete_lines_into_event()?;

        Ok(())
    }

    // Populate the event fields from the complete lines already seen, until we either encounter an
    // empty line - indicating we've decoded a complete event - or we run out of complete lines to
    // process.
    //
    // Returns the event for dispatch if it is complete.
    fn parse_complete_lines_into_event(&mut self) -> Result<()> {
        loop {
            let mut seen_empty_line = false;

            while let Some(line) = self.complete_lines.pop_front() {
                if line.is_empty() && self.event_data.is_some() {
                    seen_empty_line = true;
                    break;
                } else if line.is_empty() {
                    continue;
                }

                if let Some((key, value)) = parse_field(&line)? {
                    if key == "comment" {
                        self.sse.push_back(Sse::Comment(value.to_string()));
                        continue;
                    }

                    let id = &self.last_event_id;
                    let event_data = self
                        .event_data
                        .get_or_insert_with(|| EventData::new().with_id(id.clone()));

                    match key {
                        "event" => event_data.event_type = value.to_string(),
                        "data" => event_data.append_data(value),
                        "id" => {
                            // If id contains a null byte, it is a non-fatal error and the rest of
                            // the event should be parsed if possible.
                            if value.chars().any(|c| c == '\0') {
                                tracing::debug!("Ignoring event ID containing null byte");
                                continue;
                            }

                            if value.is_empty() {
                                self.last_event_id = Some("".to_string());
                            } else {
                                self.last_event_id = Some(value.to_string());
                            }

                            event_data.id = self.last_event_id.clone()
                        }
                        "retry" => {
                            match value.parse::<u64>() {
                                Ok(retry) => {
                                    event_data.retry = Some(retry);
                                }
                                _ => {
                                    tracing::debug!("Failed to parse {:?} into retry value", value)
                                }
                            };
                        }
                        _ => {}
                    }
                }
            }

            if seen_empty_line {
                let event_data = self.event_data.take();

                tracing::trace!(
                    "seen empty line, event_data is {:?})",
                    event_data.as_ref().map(|event_data| &event_data.event_type)
                );

                if let Some(event_data) = event_data {
                    match Option::<Sse>::try_from(event_data) {
                        Err(e) => return Err(e),
                        Ok(None) => (),
                        Ok(Some(event)) => self.sse.push_back(event),
                    };
                }

                continue;
            } else {
                tracing::trace!("processed all complete lines but event_data not yet complete");
            }

            break;
        }

        Ok(())
    }

    // Decode a chunk into lines and buffer them for subsequent parsing, taking account of
    // incomplete lines from previous chunks.
    fn decode_and_buffer_lines(&mut self, chunk: Bytes) {
        let mut lines = chunk.split_inclusive(|&b| b == b'\n' || b == b'\r');
        // The first and last elements in this split are special. The spec requires lines to be
        // terminated. But lines may span chunks, so:
        //  * the last line, if non-empty (i.e. if chunk didn't end with a line terminator),
        //    should be buffered as an incomplete line
        //  * the first line should be appended to the incomplete line, if any

        if let Some(incomplete_line) = self.incomplete_line.as_mut() {
            if let Some(line) = lines.next() {
                tracing::trace!(
                    "extending line from previous chunk: {:?}+{:?}",
                    logify(incomplete_line),
                    logify(line)
                );

                self.last_char_was_cr = false;
                if !line.is_empty() {
                    // Checking the last character handles lines where the last character is a
                    // terminator, but also where the entire line is a terminator.
                    match line.last().unwrap() {
                        b'\r' => {
                            incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                            let il = self.incomplete_line.take();
                            self.complete_lines.push_back(il.unwrap());
                            self.last_char_was_cr = true;
                        }
                        b'\n' => {
                            incomplete_line.extend_from_slice(&line[..line.len() - 1]);
                            let il = self.incomplete_line.take();
                            self.complete_lines.push_back(il.unwrap());
                        }
                        _ => incomplete_line.extend_from_slice(line),
                    };
                }
            }
        }

        let mut lines = lines.peekable();
        while let Some(line) = lines.next() {
            if let Some(actually_complete_line) = self.incomplete_line.take() {
                // we saw the next line, so the previous one must have been complete after all
                tracing::trace!(
                    "previous line was complete: {:?}",
                    logify(&actually_complete_line)
                );
                self.complete_lines.push_back(actually_complete_line);
            }

            if self.last_char_was_cr && line == [b'\n'] {
                // This is a continuation of a \r\n pair, so we can ignore this line. We do need to
                // reset our flag though.
                self.last_char_was_cr = false;
                continue;
            }

            self.last_char_was_cr = false;
            if line.ends_with(&[b'\r']) {
                self.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
                self.last_char_was_cr = true;
            } else if line.ends_with(&[b'\n']) {
                // self isn't a continuation, but rather a line ending with a LF terminator.
                self.complete_lines
                    .push_back(line[..line.len() - 1].to_vec());
            } else if line.is_empty() {
                // this is the last line and it's empty, no need to buffer it
                tracing::trace!("chunk ended with a line terminator");
            } else if lines.peek().is_some() {
                // this line isn't the last and we know from previous checks it doesn't end in a
                // terminator, so we can consider it complete
                self.complete_lines.push_back(line.to_vec());
            } else {
                // last line needs to be buffered as it may be incomplete
                tracing::trace!("buffering incomplete line: {:?}", logify(line));
                self.incomplete_line = Some(line.to_vec());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error::*;
    use super::*;

    fn field<'a>(key: &'a str, value: &'a str) -> Result<Option<(&'a str, &'a str)>> {
        Ok(Some((key, value)))
    }

    #[test]
    fn test_parse_field_invalid() {
        assert!(parse_field(b"").is_err());

        match parse_field(b"\x80: invalid UTF-8") {
            Err(InvalidLine(msg)) => assert!(msg.contains("Utf8Error")),
            res => panic!("expected InvalidLine error, got {:?}", res),
        }
    }

    #[test]
    fn test_event_id_error_if_invalid_utf8() {
        let mut bytes = Vec::from("id: ");
        let mut invalid = vec![b'\xf0', b'\x28', b'\x8c', b'\xbc'];
        bytes.append(&mut invalid);
        bytes.push(b'\n');
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from(bytes)).is_err());
    }

    #[test]
    fn test_parse_field_comments() {
        assert_eq!(parse_field(b":"), field("comment", ""));
        assert_eq!(
            parse_field(b":hello \0 world"),
            field("comment", "hello \0 world")
        );
        assert_eq!(parse_field(b":event: foo"), field("comment", "event: foo"));
    }

    #[test]
    fn test_parse_field_valid() {
        assert_eq!(parse_field(b"event:foo"), field("event", "foo"));
        assert_eq!(parse_field(b"event: foo"), field("event", "foo"));
        assert_eq!(parse_field(b"event:  foo"), field("event", " foo"));
        assert_eq!(parse_field(b"event:\tfoo"), field("event", "\tfoo"));
        assert_eq!(parse_field(b"event: foo "), field("event", "foo "));

        assert_eq!(parse_field(b"disconnect:"), field("disconnect", ""));
        assert_eq!(parse_field(b"disconnect: "), field("disconnect", ""));
        assert_eq!(parse_field(b"disconnect:  "), field("disconnect", " "));
        assert_eq!(parse_field(b"disconnect:\t"), field("disconnect", "\t"));

        assert_eq!(parse_field(b"disconnect"), field("disconnect", ""));

        assert_eq!(parse_field(b" : foo"), field(" ", "foo"));
        assert_eq!(parse_field(b"\xe2\x98\x83: foo"), field("☃", "foo"));
    }

    fn event(typ: &str, data: &str) -> Sse {
        Sse::Event(Event {
            data: data.to_string(),
            id: None,
            event_type: typ.to_string(),
            retry: None,
        })
    }

    #[test]
    fn test_event_without_data_yields_no_event() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("id: abc\n\n")).is_ok());
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_ignore_id_containing_null() {
        let mut parser = EventParser::new();
        assert!(parser
            .process_bytes(Bytes::from("id: a\x00bc\nevent: add\ndata: abc\n\n"))
            .is_ok());

        if let Some(Sse::Event(event)) = parser.get_event() {
            assert!(event.id.is_none());
        } else {
            panic!("Event should have been received");
        }
    }

    #[test]
    fn test_comment_is_separate_from_event() {
        let mut parser = EventParser::new();
        let result = parser.process_bytes(Bytes::from(":comment\ndata:hello\n\n"));
        assert!(result.is_ok());

        let comment = parser.get_event();
        assert!(matches!(comment, Some(Sse::Comment(_))));

        let event = parser.get_event();
        assert!(matches!(event, Some(Sse::Event(_))));

        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_comment_with_trailing_blank_line() {
        let mut parser = EventParser::new();
        let result = parser.process_bytes(Bytes::from(":comment\n\r\n\r"));
        assert!(result.is_ok());

        let comment = parser.get_event();
        assert!(matches!(comment, Some(Sse::Comment(_))));

        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_line_split_across_chunks() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("data:foo")).is_ok());
        assert!(parser.process_bytes(Bytes::from("")).is_ok());
        assert!(parser.process_bytes(Bytes::from("baz\n\n")).is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "foobaz")));
        assert!(parser.get_event().is_none());

        assert!(parser.process_bytes(Bytes::from("data:foo")).is_ok());
        assert!(parser.process_bytes(Bytes::from("bar")).is_ok());
        assert!(parser.process_bytes(Bytes::from("baz\n\n")).is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "foobarbaz")));
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_concatenates_multiple_values_for_same_field() {
        let mut parser = EventParser::new();
        assert!(parser.process_bytes(Bytes::from("data:hello\n")).is_ok());
        assert!(parser.process_bytes(Bytes::from("data:world\n\n")).is_ok());
        assert_eq!(parser.get_event(), Some(event("message", "hello\nworld")));
        assert!(parser.get_event().is_none());
    }

    #[test]
    fn test_decode_extra_terminators_between_events() {
        let mut parser = EventParser::new();
        assert!(parser
            .process_bytes(Bytes::from("data: abc\n\n\ndata: def\n\n"))
            .is_ok());

        assert_eq!(parser.get_event(), Some(event("message", "abc")));
        assert_eq!(parser.get_event(), Some(event("message", "def")));
        assert!(parser.get_event().is_none());
    }
}
