//! Single-walk, iterative conversion to and from `serde_json_bytes::Value`.
//!
//! Both directions are one traversal without an intermediate byte buffer.
//! Numbers cross the boundary through their literal text: converting to the
//! legacy type parses integers as `i64`/`u64` and everything else as `f64`
//! (the `serde_json` reading); higher-precision literals lose the digits
//! `f64` cannot represent, and literals beyond `f64` range saturate — the
//! legacy type simply cannot represent either. Converting back formats
//! numbers the way `serde_json` prints them.

use serde_json_bytes::{ByteString, Map, Value};

use crate::arena::Arena;
use crate::de::NumberClass;
use crate::document::ValueRef;
use crate::node::{Child, Entry, KeyRef, Node};

/// Converts a subtree to the legacy value type.
pub(crate) fn to_legacy(root: ValueRef<'_>) -> Value {
    enum Task<'a> {
        Enter(ValueRef<'a>),
        Exit(ValueRef<'a>, usize),
    }
    let mut tasks = vec![Task::Enter(root)];
    let mut done: Vec<Value> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter(value) => match value.node() {
                Node::Null => done.push(Value::Null),
                Node::Bool(b) => done.push(Value::Bool(b)),
                Node::Number(_) | Node::OwnedNumber(_) => {
                    let raw = value.raw_number().expect("node is a number");
                    done.push(Value::Number(parse_number(raw)));
                }
                Node::String { .. } | Node::OwnedString(_) => {
                    let text = value.as_str().expect("node is a string");
                    done.push(Value::String(ByteString::from(text.into_owned())));
                }
                Node::Array(slab) => {
                    tasks.push(Task::Exit(value, done.len()));
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
                let children: Vec<Value> = done.split_off(start);
                match value.node() {
                    Node::Array(_) => done.push(Value::Array(children)),
                    Node::Object(slab) => {
                        let mut map = Map::with_capacity(children.len());
                        for (entry, child) in value.arena.entries(slab).iter().zip(children) {
                            let key = value.arena.key_unescaped(entry.key);
                            map.insert(ByteString::from(key.into_owned()), child);
                        }
                        done.push(Value::Object(map));
                    }
                    _ => unreachable!("only containers exit"),
                }
            }
        }
    }
    done.pop().expect("conversion produces exactly one value")
}

/// Converts a legacy value into an owned handle with a fresh arena.
pub(crate) fn from_legacy(root: &Value) -> crate::document::Value {
    enum Task<'a> {
        Enter(&'a Value),
        Exit(&'a Value, usize),
    }
    let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
    let mut tasks = vec![Task::Enter(root)];
    let mut done: Vec<Child> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter(value) => match value {
                Value::Null => done.push(Child::local(arena.push_node(Node::Null))),
                Value::Bool(b) => done.push(Child::local(arena.push_node(Node::Bool(*b)))),
                Value::Number(n) => {
                    let text = arena.alloc_text(&n.to_string());
                    done.push(Child::local(arena.push_node(Node::OwnedNumber(text))));
                }
                Value::String(s) => {
                    let text = arena.alloc_text(s.as_str());
                    done.push(Child::local(arena.push_node(Node::OwnedString(text))));
                }
                Value::Array(items) => {
                    tasks.push(Task::Exit(value, done.len()));
                    for child in items.iter().rev() {
                        tasks.push(Task::Enter(child));
                    }
                }
                Value::Object(map) => {
                    tasks.push(Task::Exit(value, done.len()));
                    for (_, child) in map.iter().rev() {
                        tasks.push(Task::Enter(child));
                    }
                }
            },
            Task::Exit(value, start) => {
                let node = match value {
                    Value::Array(_) => Node::Array(arena.alloc_children(&done[start..])),
                    Value::Object(map) => {
                        let members: Vec<Entry> = map
                            .iter()
                            .zip(&done[start..])
                            .map(|((key, _), &child)| Entry {
                                key: KeyRef::Owned(arena.alloc_text(key.as_str())),
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
        .expect("conversion produces exactly one root node");
    crate::document::Value::rooted(arena, root)
}

/// Decodes a validated number literal the way `serde_json` would.
fn parse_number(raw: &str) -> serde_json::Number {
    match crate::de::classify_number(raw) {
        NumberClass::Unsigned(u) => u.into(),
        NumberClass::Signed(i) => i.into(),
        NumberClass::Float => {
            let mut f: f64 = raw.parse().expect("validated number literal");
            if !f.is_finite() {
                // The legacy type cannot represent out-of-range literals;
                // saturate.
                f = f64::MAX.copysign(f);
            }
            serde_json::Number::from_f64(f).expect("finite by construction")
        }
    }
}
