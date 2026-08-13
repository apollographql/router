//! Reading a document that is still being built.
//!
//! [`ValueRef`](crate::ValueRef) reads a sealed document, where every
//! container is a packed slab. A [`ValueBuilder`] is not sealed: a
//! container that grows is held in a mutable overlay owned by the builder
//! rather than by the arena, so a reader that borrows only the arena cannot
//! see it — it reports the members absent and the length zero.
//!
//! [`BuilderRef`] borrows the builder as well as the arena, so it resolves
//! overlays and answers about the document as it stands. That is what lets a
//! caller assembling a response ask "have I written this key yet?" without
//! sealing, rebuilding, or copying.

use std::borrow::Cow;

use crate::arena::{Arena, resolve};
use crate::builder::ValueBuilder;
use crate::document::JsonKind;
use crate::document::Value;
use crate::node::{Child, Entry, Node, NodeId};

/// A read-only view of one value in a document under construction.
///
/// Obtained from [`ValueBuilder::value`] or
/// [`ValueMut::value`](crate::ValueMut::value). Unlike
/// [`ValueRef`](crate::ValueRef), it sees writes that have not been sealed.
#[derive(Clone, Copy)]
pub struct BuilderRef<'b> {
    builder: &'b ValueBuilder,
    /// The arena owning `node`, which is the builder's own arena for a local
    /// node and another document's for an adopted one.
    arena: &'b Arena,
    node: NodeId,
    /// Whether `arena` is the builder's, and so whether overlays apply.
    local: bool,
}

impl<'b> BuilderRef<'b> {
    /// Views `root`, resolving a foreign child to the arena that owns it. A
    /// builder over a shared document keeps its root foreign until a write
    /// localizes it, so the root's nodes may live in the shared arena rather
    /// than the builder's.
    pub(crate) fn new(builder: &'b ValueBuilder, root: Child) -> Self {
        let local = root.as_local().is_some();
        let (arena, node) = resolve(builder.arena(), root);
        BuilderRef {
            builder,
            arena,
            node,
            local,
        }
    }

    fn node(&self) -> Node {
        self.arena.node(self.node)
    }

    /// Follows a child, switching arenas when it is an adopted subtree. An
    /// adopted subtree is sealed by construction, so overlays stop applying.
    fn child(&self, child: Child) -> BuilderRef<'b> {
        let (arena, node) = resolve(self.arena, child);
        let local = self.local && std::ptr::eq(arena, self.arena);
        BuilderRef {
            builder: self.builder,
            arena,
            node,
            local,
        }
    }

    /// The members of an object, from the overlay when one is open.
    fn entries(&self) -> Option<&'b [Entry]> {
        match self.node() {
            Node::Object(slab) => Some(self.arena.entries(slab)),
            Node::MutObject(index) if self.local => {
                Some(&self.builder.object_overlay(index as usize).1)
            }
            _ => None,
        }
    }

    /// The elements of an array, from the overlay when one is open.
    fn children(&self) -> Option<&'b [Child]> {
        match self.node() {
            Node::Array(slab) => Some(self.arena.children(slab)),
            Node::MutArray(index) if self.local => {
                Some(&self.builder.array_overlay(index as usize).1)
            }
            _ => None,
        }
    }

    /// The JSON type of the value.
    pub fn kind(&self) -> JsonKind {
        match self.node() {
            Node::Null => JsonKind::Null,
            Node::Bool(_) => JsonKind::Bool,
            Node::Number(_) | Node::OwnedNumber(_) => JsonKind::Number,
            Node::String { .. } | Node::OwnedString(_) => JsonKind::String,
            Node::Array(_) | Node::MutArray(_) => JsonKind::Array,
            Node::Object(_) | Node::MutObject(_) => JsonKind::Object,
        }
    }

    /// Whether the value is `null`.
    pub fn is_null(&self) -> bool {
        matches!(self.node(), Node::Null)
    }

    /// Number of members or elements; `None` for a scalar.
    pub fn len(&self) -> Option<usize> {
        if let Some(entries) = self.entries() {
            return Some(entries.len());
        }
        self.children().map(<[Child]>::len)
    }

    /// Whether the container is empty; `None` for a scalar.
    pub fn is_empty(&self) -> Option<bool> {
        Some(self.len()? == 0)
    }

    /// Looks up an object member.
    pub fn get(&self, key: &str) -> Option<BuilderRef<'b>> {
        let entries = self.entries()?;
        entries
            .iter()
            .find(|entry| self.arena.key_matches_str(entry.key, key))
            .map(|entry| self.child(entry.child))
    }

    /// Whether an object holds `key`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Looks up an array element.
    pub fn index(&self, index: usize) -> Option<BuilderRef<'b>> {
        self.children()?.get(index).map(|child| self.child(*child))
    }

    /// Iterates array elements, in order. Empty for a non-array.
    pub fn array_iter(&self) -> impl Iterator<Item = BuilderRef<'b>> + use<'b> {
        let this = *self;
        let children = self.children().unwrap_or(&[]);
        children.iter().map(move |child| this.child(*child))
    }

    /// Iterates object members as `(key, value)`, in insertion order. Empty
    /// for a non-object.
    pub fn object_iter(&self) -> impl Iterator<Item = (Cow<'b, str>, BuilderRef<'b>)> + use<'b> {
        let this = *self;
        let entries = self.entries().unwrap_or(&[]);
        entries
            .iter()
            .map(move |entry| (this.arena.key_unescaped(entry.key), this.child(entry.child)))
    }

    /// The boolean value.
    pub fn as_bool(&self) -> Option<bool> {
        match self.node() {
            Node::Bool(value) => Some(value),
            _ => None,
        }
    }

    /// The string value, unescaped on access.
    pub fn as_str(&self) -> Option<Cow<'b, str>> {
        match self.node() {
            Node::String {
                span,
                escaped: false,
            } => Some(Cow::Borrowed(self.arena.input_utf8(span).as_str())),
            Node::String {
                span,
                escaped: true,
            } => Some(Cow::Owned(crate::text::unescape(
                self.arena.input_utf8(span),
            ))),
            Node::OwnedString(text) => Some(Cow::Borrowed(self.arena.text_str(text))),
            _ => None,
        }
    }

    /// The number's literal text, exactly as written.
    pub fn raw_number(&self) -> Option<&'b str> {
        match self.node() {
            Node::Number(span) => Some(self.arena.input_utf8(span).as_str()),
            Node::OwnedNumber(text) => Some(self.arena.text_str(text)),
            _ => None,
        }
    }

    /// The number as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        self.raw_number()?.parse().ok()
    }

    /// The number as `i64`, when the literal is an integer in range.
    pub fn as_i64(&self) -> Option<i64> {
        let raw = self.raw_number()?;
        if raw.bytes().any(|b| matches!(b, b'.' | b'e' | b'E')) {
            return None;
        }
        raw.parse().ok()
    }

    /// Deep-copies this subtree into a fresh, minimal owned handle, reading
    /// through any open overlays. The copy retains only its own bytes; see
    /// [`Value::compact`](crate::Value::compact), which does the same for a
    /// sealed subtree.
    pub fn to_value(&self) -> Value {
        enum Task<'b> {
            Enter(BuilderRef<'b>),
            /// Assemble a container from the children completed since
            /// `start` in the scratch stack.
            Exit(BuilderRef<'b>, usize),
        }

        let mut input = String::new();
        let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
        let mut tasks: Vec<Task<'_>> = vec![Task::Enter(*self)];
        let mut done: Vec<Child> = Vec::new();

        while let Some(task) = tasks.pop() {
            match task {
                Task::Enter(value) => match value.node() {
                    Node::Null => done.push(Child::local(arena.push_node(Node::Null))),
                    Node::Bool(b) => done.push(Child::local(arena.push_node(Node::Bool(b)))),
                    Node::Number(span) => {
                        let span =
                            crate::detach::copy_span(&mut input, value.arena.input_utf8(span));
                        done.push(Child::local(arena.push_node(Node::Number(span))));
                    }
                    Node::OwnedNumber(text) => {
                        let text = arena.alloc_text(value.arena.text_str(text));
                        done.push(Child::local(arena.push_node(Node::OwnedNumber(text))));
                    }
                    Node::String { span, escaped } => {
                        let span =
                            crate::detach::copy_span(&mut input, value.arena.input_utf8(span));
                        done.push(Child::local(
                            arena.push_node(Node::String { span, escaped }),
                        ));
                    }
                    Node::OwnedString(text) => {
                        let text = arena.alloc_text(value.arena.text_str(text));
                        done.push(Child::local(arena.push_node(Node::OwnedString(text))));
                    }
                    Node::Array(_) | Node::MutArray(_) => {
                        tasks.push(Task::Exit(value, done.len()));
                        // Children complete in reverse task order; push
                        // reversed so `done` receives them in document order.
                        for &child in value.children().expect("node is an array").iter().rev() {
                            tasks.push(Task::Enter(value.child(child)));
                        }
                    }
                    Node::Object(_) | Node::MutObject(_) => {
                        tasks.push(Task::Exit(value, done.len()));
                        for entry in value.entries().expect("node is an object").iter().rev() {
                            tasks.push(Task::Enter(value.child(entry.child)));
                        }
                    }
                },
                Task::Exit(value, start) => {
                    let node = match value.node() {
                        Node::Array(_) | Node::MutArray(_) => {
                            Node::Array(arena.alloc_children(&done[start..]))
                        }
                        Node::Object(_) | Node::MutObject(_) => {
                            let members: Vec<Entry> = value
                                .entries()
                                .expect("node is an object")
                                .iter()
                                .zip(&done[start..])
                                .map(|(entry, &child)| Entry {
                                    key: crate::detach::copy_key(
                                        &mut input,
                                        &mut arena,
                                        value.arena,
                                        entry.key,
                                    ),
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
            .expect("the copy yields exactly one root node");
        arena.set_input(crate::utf8::Utf8Bytes::from(input));
        Value::rooted(arena, root)
    }
}

impl std::fmt::Debug for BuilderRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BuilderRef({:?})", self.kind())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Value, ValueBuilder};

    #[test]
    fn a_grown_object_reads_back_its_new_members() {
        // Growing a container opens an overlay. A reader that saw only the
        // arena reported these members absent, which turned a caller's
        // read-modify-write into an overwrite.
        let mut builder = ValueBuilder::new();
        builder.set("a", 1_i64).unwrap();
        builder.set("b", "two").unwrap();

        let root = builder.value();
        assert_eq!(root.len(), Some(2));
        assert!(root.contains_key("a"));
        assert_eq!(
            root.get("b").and_then(|b| b.as_str()).as_deref(),
            Some("two")
        );
        assert_eq!(
            root.object_iter()
                .map(|(k, _)| k.into_owned())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn a_grown_array_reads_back_its_length() {
        let doc = Value::parse(br#"{"xs":[1]}"#.to_vec()).unwrap();
        let mut builder = doc.edit();
        let mut xs = builder.get_mut("xs").unwrap();
        xs.push(2_i64).unwrap();
        xs.push(3_i64).unwrap();

        let xs = xs.value();
        assert_eq!(xs.len(), Some(3), "the two appended elements are visible");
        assert_eq!(xs.index(2).and_then(|v| v.as_i64()), Some(3));
    }

    #[test]
    fn a_foreign_root_reads_from_the_arena_that_owns_it() {
        // Editing a shared document keeps the root as a reference into the
        // shared arena until a write localizes it. The view resolved the
        // foreign node id but read it against the builder's own (empty)
        // arena, which panicked on any access.
        let doc = Value::parse(br#"{"a":1,"xs":[true]}"#.to_vec()).unwrap();
        let shared = doc.clone();
        let builder = shared.edit();

        let root = builder.value();
        assert_eq!(root.kind(), crate::JsonKind::Object);
        assert_eq!(root.len(), Some(2));
        assert_eq!(root.get("a").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(
            root.get("xs")
                .and_then(|xs| xs.index(0))
                .and_then(|v| v.as_bool()),
            Some(true),
        );
        assert_eq!(root.to_value().to_vec(), br#"{"a":1,"xs":[true]}"#);
    }

    #[test]
    fn to_value_copies_through_overlays() {
        let mut builder = ValueBuilder::new();
        builder.set("kept", "yes").unwrap();
        let copy = builder
            .value()
            .get("kept")
            .is_some()
            .then(|| builder.value().to_value());
        assert_eq!(copy.unwrap().to_vec(), br#"{"kept":"yes"}"#);
    }

    #[test]
    fn reads_reach_through_an_adopted_subtree() {
        let source = Value::parse(br#"{"deep":{"kept":true}}"#.to_vec()).unwrap();
        let mut builder = ValueBuilder::new();
        builder.set("adopted", source.get("deep").unwrap()).unwrap();

        let adopted = builder.value().get("adopted").expect("member is present");
        assert_eq!(adopted.get("kept").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn a_nested_object_reads_back_after_its_parent_grew() {
        let mut builder = ValueBuilder::new();
        // `get_mut` creates a missing key as an empty object.
        let mut outer = builder.get_mut("outer").unwrap();
        outer.set("inner", 1_i64).unwrap();
        // The cursor holds the builder's only mutable borrow; end it here so
        // the root can grow again.
        let _ = outer;
        builder.set("sibling", 2_i64).unwrap();

        let root = builder.value();
        assert_eq!(root.len(), Some(2));
        assert_eq!(
            root.get("outer")
                .and_then(|o| o.get("inner"))
                .and_then(|v| v.as_i64()),
            Some(1),
        );
    }
}
