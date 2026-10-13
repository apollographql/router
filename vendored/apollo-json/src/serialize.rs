//! Iterative serializer emitting untouched input spans verbatim.
//!
//! Strings and numbers parsed from input are written back byte-for-byte
//! (escapes and number formatting intact), so an unmodified document
//! round-trips byte-identically. Values introduced by mutation are escaped
//! with `serde_json`-compatible rules. Shared subtrees are expanded at every
//! reference; the walk uses an explicit stack, never recursion.

use crate::document::ValueRef;
use crate::node::{KeyRef, Node};
use crate::text::escape_into;

#[derive(Clone, Copy)]
struct Frame<'a> {
    value: ValueRef<'a>,
    next: usize,
}

/// Containers held inline before the frame stack spills to the heap.
const INLINE_FRAMES: usize = 16;

/// The open containers, innermost last. The first [`INLINE_FRAMES`] are held
/// inline, so serializing a document of ordinary depth allocates nothing but
/// its output.
struct FrameStack<'a> {
    inline: [Frame<'a>; INLINE_FRAMES],
    len: usize,
    spilled: Vec<Frame<'a>>,
}

impl<'a> FrameStack<'a> {
    fn new(filler: ValueRef<'a>) -> Self {
        FrameStack {
            inline: [Frame {
                value: filler,
                next: 0,
            }; INLINE_FRAMES],
            len: 0,
            spilled: Vec::new(),
        }
    }

    fn push(&mut self, frame: Frame<'a>) {
        if self.len < INLINE_FRAMES {
            self.inline[self.len] = frame;
            self.len += 1;
        } else {
            self.spilled.push(frame);
        }
    }

    fn last_mut(&mut self) -> Option<&mut Frame<'a>> {
        if !self.spilled.is_empty() {
            return self.spilled.last_mut();
        }
        self.len.checked_sub(1).map(|top| &mut self.inline[top])
    }

    fn pop(&mut self) {
        if self.spilled.pop().is_none() {
            self.len -= 1;
        }
    }
}

pub(crate) fn serialize(root: ValueRef<'_>, capacity_hint: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity_hint);
    serialize_into(root, &mut out);
    out
}

/// Appends the serialized `root` to `out`.
pub(crate) fn serialize_into(root: ValueRef<'_>, out: &mut Vec<u8>) {
    let mut stack = FrameStack::new(root);
    let mut pending = Some(root);

    loop {
        if let Some(value) = pending.take() {
            match value.node() {
                Node::Null => out.extend_from_slice(b"null"),
                Node::Bool(true) => out.extend_from_slice(b"true"),
                Node::Bool(false) => out.extend_from_slice(b"false"),
                Node::Number(span) => out.extend_from_slice(value.arena.span_bytes(span)),
                Node::OwnedNumber(text) => out.extend_from_slice(value.arena.text(text)),
                Node::String { span, .. } => {
                    out.push(b'"');
                    out.extend_from_slice(value.arena.span_bytes(span));
                    out.push(b'"');
                }
                Node::OwnedString(text) => {
                    out.push(b'"');
                    escape_into(value.arena.text(text), out);
                    out.push(b'"');
                }
                Node::Array(slab) => {
                    out.push(b'[');
                    if slab.len() == 0 {
                        out.push(b']');
                    } else {
                        stack.push(Frame { value, next: 0 });
                    }
                }
                Node::Object(slab) => {
                    out.push(b'{');
                    if slab.len() == 0 {
                        out.push(b'}');
                    } else {
                        stack.push(Frame { value, next: 0 });
                    }
                }
            }
            continue;
        }

        let Some(frame) = stack.last_mut() else {
            return;
        };
        match frame.value.node() {
            Node::Array(slab) => {
                let items = frame.value.arena.children(slab);
                if frame.next == items.len() {
                    out.push(b']');
                    stack.pop();
                    continue;
                }
                if frame.next > 0 {
                    out.push(b',');
                }
                let child = items[frame.next];
                frame.next += 1;
                pending = Some(frame.value.deref_child(child));
            }
            Node::Object(slab) => {
                let value = frame.value;
                let entries = value.arena.entries(slab);
                if frame.next == entries.len() {
                    out.push(b'}');
                    stack.pop();
                    continue;
                }
                if frame.next > 0 {
                    out.push(b',');
                }
                let entry = entries[frame.next];
                frame.next += 1;
                out.push(b'"');
                match entry.key {
                    KeyRef::Span { span, .. } => {
                        out.extend_from_slice(value.arena.span_bytes(span));
                    }
                    KeyRef::Owned(text) => escape_into(value.arena.text(text), out),
                }
                out.extend_from_slice(b"\":");
                pending = Some(value.deref_child(entry.child));
            }
            _ => unreachable!("only containers are stacked"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Value;

    #[test]
    fn documents_deeper_than_the_inline_frames_round_trip() {
        for depth in [0, 1, 15, 16, 17, 40] {
            let text = format!(
                "{}{}{}",
                r#"{"a":["#.repeat(depth),
                r#"{"k":"v","n":1}"#,
                "]}".repeat(depth)
            );
            let value = Value::parse(text.clone().into_bytes()).unwrap();
            assert_eq!(value.to_string(), text, "depth {depth}");
        }
    }

    #[test]
    fn serialize_into_appends_to_the_buffer() {
        let value = Value::parse(br#"{"a":[1,{"b":"c"}]}"#.to_vec()).unwrap();
        let mut out = b"{\"data\":".to_vec();
        value.serialize_into(&mut out);
        out.push(b'}');
        assert_eq!(out, br#"{"data":{"a":[1,{"b":"c"}]}}"#);
    }
}
