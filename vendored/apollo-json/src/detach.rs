//! Deep copy of a subtree into a fresh, minimal arena.
//!
//! Detaching severs the pin on the source arena: the copy retains only its
//! own bytes. Spans are copied verbatim into the new arena's input buffer, so
//! a detached document serializes byte-identically to the source subtree.
//! Shared references are expanded into copies. The walk is iterative, and
//! children accumulate in one scratch stack that flushes into arena slabs as
//! containers complete.

use crate::arena::Arena;
use crate::document::{Value, ValueRef};
use crate::node::{Child, Entry, KeyRef, Node, Span};
use crate::utf8::{Utf8Bytes, ValidatedUtf8};

enum Task<'a> {
    Enter(ValueRef<'a>),
    /// Assemble a container from the children completed since `start` in the
    /// scratch stack.
    Exit(ValueRef<'a>, usize),
}

pub(crate) fn detach(root: ValueRef<'_>) -> Value {
    let mut input = String::new();
    let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
    let mut tasks: Vec<Task<'_>> = vec![Task::Enter(root)];
    let mut done: Vec<Child> = Vec::new();

    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter(value) => match value.node() {
                Node::Null => done.push(Child::local(arena.push_node(Node::Null))),
                Node::Bool(b) => done.push(Child::local(arena.push_node(Node::Bool(b)))),
                Node::Number(span) => {
                    let span = copy_span(&mut input, value.arena.input_utf8(span));
                    done.push(Child::local(arena.push_node(Node::Number(span))));
                }
                Node::OwnedNumber(text) => {
                    let text = arena.alloc_text(value.arena.text_str(text));
                    done.push(Child::local(arena.push_node(Node::OwnedNumber(text))));
                }
                Node::String { span, escaped } => {
                    let span = copy_span(&mut input, value.arena.input_utf8(span));
                    done.push(Child::local(
                        arena.push_node(Node::String { span, escaped }),
                    ));
                }
                Node::OwnedString(text) => {
                    let text = arena.alloc_text(value.arena.text_str(text));
                    done.push(Child::local(arena.push_node(Node::OwnedString(text))));
                }
                Node::Array(slab) => {
                    tasks.push(Task::Exit(value, done.len()));
                    // Children complete in reverse task order; push reversed
                    // so `done` receives them in document order.
                    for &child in value.arena.children(slab).iter().rev() {
                        tasks.push(Task::Enter(value.deref_child(child)));
                    }
                }
                Node::Object(slab) => {
                    tasks.push(Task::Exit(value, done.len()));
                    for entry in value.arena.entries(slab).iter().rev() {
                        tasks.push(Task::Enter(value.deref_child(entry.child)));
                    }
                }
                Node::MutArray(_) | Node::MutObject(_) => {
                    unreachable!("builder overlays are flattened at seal")
                }
            },
            Task::Exit(value, start) => {
                let node = match value.node() {
                    Node::Array(_) => {
                        let slab = arena.alloc_children(&done[start..]);
                        Node::Array(slab)
                    }
                    Node::Object(source_slab) => {
                        // Rewrite keys against the new arena, then pair them
                        // with the copied children.
                        let members: Vec<Entry> = value
                            .arena
                            .entries(source_slab)
                            .iter()
                            .zip(&done[start..])
                            .map(|(entry, &child)| Entry {
                                key: copy_key(&mut input, &mut arena, value.arena, entry.key),
                                child,
                            })
                            .collect();
                        Node::Object(arena.alloc_entries(&members))
                    }
                    _ => unreachable!("only containers exit"),
                };
                done.truncate(start);
                done.push(Child::local(arena.push_node(node)));
            }
        }
    }

    let root = done
        .pop()
        .and_then(Child::as_local)
        .expect("detach copies exactly one root node");
    arena.set_input(Utf8Bytes::from(input));
    Value::rooted(arena, root)
}

pub(crate) fn copy_span(input: &mut String, text: ValidatedUtf8<'_>) -> Span {
    let start = u32::try_from(input.len()).expect("detached input within span range");
    input.push_str(text.as_str());
    Span {
        start,
        len: u32::try_from(text.len()).expect("detached input within span range"),
    }
}

pub(crate) fn copy_key(
    input: &mut String,
    arena: &mut Arena,
    source: &Arena,
    key: KeyRef,
) -> KeyRef {
    match key {
        KeyRef::Span { span, escaped } => KeyRef::Span {
            span: copy_span(input, source.input_utf8(span)),
            escaped,
        },
        KeyRef::Owned(text) => KeyRef::Owned(arena.alloc_text(source.text_str(text))),
    }
}
