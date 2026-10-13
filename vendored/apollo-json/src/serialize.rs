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

struct Frame<'a> {
    value: ValueRef<'a>,
    next: usize,
}

pub(crate) fn serialize(root: ValueRef<'_>, capacity_hint: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity_hint);
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut pending = Some(root);

    loop {
        if let Some(value) = pending.take() {
            match value.node() {
                Node::Null => out.extend_from_slice(b"null"),
                Node::Bool(true) => out.extend_from_slice(b"true"),
                Node::Bool(false) => out.extend_from_slice(b"false"),
                Node::Number(span) => out.extend_from_slice(span.slice(value.arena.input())),
                Node::OwnedNumber(text) => out.extend_from_slice(value.arena.text(text)),
                Node::String { span, .. } => {
                    out.push(b'"');
                    out.extend_from_slice(span.slice(value.arena.input()));
                    out.push(b'"');
                }
                Node::OwnedString(text) => {
                    out.push(b'"');
                    escape_into(value.arena.text(text), &mut out);
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
                Node::MutArray(_) | Node::MutObject(_) => {
                    unreachable!("builder overlays are flattened at seal")
                }
            }
            continue;
        }

        let Some(frame) = stack.last_mut() else {
            return out;
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
                        out.extend_from_slice(span.slice(value.arena.input()));
                    }
                    KeyRef::Owned(text) => escape_into(value.arena.text(text), &mut out),
                }
                out.extend_from_slice(b"\":");
                pending = Some(value.deref_child(entry.child));
            }
            _ => unreachable!("only containers are stacked"),
        }
    }
}
