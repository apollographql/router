//! Mutable document assembly.
//!
//! A [`ValueBuilder`] owns its arena outright, so every mutation of a node
//! it owns is in place. Nodes still owned by other documents (adopted
//! subtrees, or the whole tree when the source document was shared) are
//! copied on write: only the nodes along the mutated path are copied into the
//! builder's arena, and siblings keep referencing the original arenas. This
//! makes mutation isolation structural — no shared document can ever observe
//! a builder's changes.
//!
//! Container children live in immutable arena slabs. Replacing a child slot
//! writes the slab in place; a container that grows (new keys, appended
//! elements) is opened into a mutable overlay ([`Node::MutArray`] /
//! [`Node::MutObject`]) that [`ValueBuilder::seal`] flattens back into
//! slabs, so the sealed document is always in the packed immutable form.

use std::borrow::Cow;
use std::sync::Arc;

use crate::arena::Arena;
use crate::document::Value;
use crate::error::JsonError;
use crate::node::{Child, Entry, KeyRef, Node, NodeId};
use crate::slab::SlabRef;

/// One step of a mutation path.
#[derive(Clone, Copy, Debug)]
pub enum PathSegment<'a> {
    /// An object member key.
    Key(&'a str),
    /// An array element index.
    Index(usize),
}

/// A value to write into a document under construction.
///
/// Everything the mutation methods accept converts into this type, so plain
/// strings, numbers, booleans, `()` (null), and [`Value`]s can be
/// passed directly.
///
/// [`NewValue::Array`] and [`NewValue::Object`] describe a *pending* tree:
/// structure that does not exist in any arena yet. A caller that assembles a
/// tree top-down — response formatting, say — can hand the whole thing to one
/// builder in a single pass, rather than sealing a document per level and
/// adopting it into its parent. One arena, one set of keys, no intermediate
/// documents.
///
/// A pending tree is nested Rust data, so its depth is bounded by the caller,
/// not by [`ParseOptions`](crate::ParseOptions). Writing one into an arena
/// does not recurse, but dropping one does, via the compiler's `Drop` glue —
/// so build pending trees from data whose depth you already bound.
///
/// ```
/// use apollo_json::{ValueBuilder, NewValue};
///
/// let mut builder = ValueBuilder::new();
/// builder.set(
///     "people",
///     NewValue::Array(vec![NewValue::Object(vec![
///         ("name".into(), "bob".into()),
///         ("age".into(), 19i64.into()),
///     ])]),
/// )?;
/// assert_eq!(
///     builder.seal().to_string(),
///     r#"{"people":[{"name":"bob","age":19}]}"#
/// );
/// # Ok::<(), apollo_json::JsonError>(())
/// ```
#[non_exhaustive]
pub enum NewValue<'a> {
    Null,
    Bool(bool),
    Int(i64),
    /// Formatted like `serde_json` formats floats.
    Float(f64),
    String(Cow<'a, str>),
    /// A subtree from another document, adopted by reference — the other
    /// document's arena stays alive, nothing is copied.
    Node(Value),
    /// A pending array, built into the arena along with its elements.
    Array(Vec<NewValue<'a>>),
    /// A pending object, built into the arena along with its members. A
    /// repeated key keeps its first position and its last value, matching what
    /// parsing `{"a":1,"a":2}` produces.
    Object(Vec<(Cow<'a, str>, NewValue<'a>)>),
}

impl<'a> From<&'a str> for NewValue<'a> {
    fn from(value: &'a str) -> Self {
        NewValue::String(Cow::Borrowed(value))
    }
}

impl From<String> for NewValue<'_> {
    fn from(value: String) -> Self {
        NewValue::String(Cow::Owned(value))
    }
}

impl<'a> From<Cow<'a, str>> for NewValue<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        NewValue::String(value)
    }
}

impl From<i64> for NewValue<'_> {
    fn from(value: i64) -> Self {
        NewValue::Int(value)
    }
}

impl From<f64> for NewValue<'_> {
    fn from(value: f64) -> Self {
        NewValue::Float(value)
    }
}

impl From<bool> for NewValue<'_> {
    fn from(value: bool) -> Self {
        NewValue::Bool(value)
    }
}

impl From<()> for NewValue<'_> {
    fn from((): ()) -> Self {
        NewValue::Null
    }
}

impl From<Value> for NewValue<'_> {
    fn from(handle: Value) -> Self {
        NewValue::Node(handle)
    }
}

impl<'a, T: Into<NewValue<'a>>> From<Option<T>> for NewValue<'a> {
    /// `None` writes as `null`, so an optional field goes straight into
    /// `builder.set(key, value)` without unwrapping.
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => value.into(),
            None => NewValue::Null,
        }
    }
}

impl std::fmt::Debug for NewValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NewValue::Null => f.write_str("Null"),
            NewValue::Bool(b) => f.debug_tuple("Bool").field(b).finish(),
            NewValue::Int(i) => f.debug_tuple("Int").field(i).finish(),
            NewValue::Float(x) => f.debug_tuple("Float").field(x).finish(),
            NewValue::String(s) => f.debug_tuple("String").field(s).finish(),
            NewValue::Node(handle) => f.debug_tuple("Node").field(handle).finish(),
            NewValue::Array(items) => f.debug_tuple("Array").field(items).finish(),
            NewValue::Object(members) => f.debug_tuple("Object").field(members).finish(),
        }
    }
}

impl<'a> From<&'a str> for PathSegment<'a> {
    fn from(key: &'a str) -> Self {
        PathSegment::Key(key)
    }
}

impl From<usize> for PathSegment<'_> {
    fn from(index: usize) -> Self {
        PathSegment::Index(index)
    }
}

/// What writing a non-finite float does. JSON has no way to spell one, so it
/// is either rejected or coerced, and which one is right depends on whether the
/// caller handed over plain Rust data or asked for a specific write.
#[derive(Clone, Copy)]
enum NonFinite {
    /// Coerce to `null`, for constructors taking plain Rust data.
    AsNull,
    /// Report [`JsonError::NonFiniteNumber`], for explicit mutations.
    Reject,
}

/// One open container while a pending [`NewValue`] tree is written into an
/// arena. Holding these on the heap is what keeps
/// [`ValueBuilder::new_child`] from recursing.
///
/// A frame does not own its finished children. They accumulate in the shared
/// [`Scratch`] buffers, and a frame records only where its own run starts —
/// otherwise every container in a pending tree would allocate a `Vec`, which
/// is the per-container cost pending trees exist to remove.
enum Frame<'a> {
    Array {
        items: std::vec::IntoIter<NewValue<'a>>,
        /// Start of this array's run in [`Scratch::children`].
        base: usize,
    },
    Object {
        members: std::vec::IntoIter<(Cow<'a, str>, NewValue<'a>)>,
        /// Start of this object's run in [`Scratch::entries`].
        base: usize,
        /// The key of the member currently being written, taken when its value
        /// finishes.
        key: Option<Cow<'a, str>>,
    },
}

impl<'a> Frame<'a> {
    /// The next member to descend into, or `None` when the container is
    /// complete.
    fn next_value(&mut self) -> Option<NewValue<'a>> {
        match self {
            Frame::Array { items, .. } => items.next(),
            Frame::Object { members, key, .. } => {
                let (next_key, value) = members.next()?;
                *key = Some(next_key);
                Some(value)
            }
        }
    }
}

/// Children of every open container in one pair of buffers, so writing a
/// pending tree allocates a bounded amount however many containers it has.
/// Each frame owns the tail of a buffer from its `base`, and closing a frame
/// truncates back to it.
#[derive(Default)]
struct Scratch {
    children: Vec<Child>,
    entries: Vec<Entry>,
}

/// Container shape used by the merge to decide between descending and
/// replacing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Object,
    Array,
    Scalar,
}

fn shape_of(node: Node) -> Shape {
    match node {
        Node::Object(_) | Node::MutObject(_) => Shape::Object,
        Node::Array(_) | Node::MutArray(_) => Shape::Array,
        _ => Shape::Scalar,
    }
}

/// A mutable document. Sealing produces an immutable, shareable
/// [`Value`].
///
/// # Example
/// ```
/// use apollo_json::Value;
///
/// let doc = Value::parse(br#"{"a":1,"tags":["x"],"tmp":true}"#.to_vec()).unwrap();
/// let mut builder = doc.edit();
/// builder.set("b", 2)?;
/// builder.remove("tmp");
/// let mut tags = builder.get_mut("tags")?;
/// tags.push("y")?;
/// assert_eq!(
///     builder.seal().to_vec(),
///     br#"{"a":1,"tags":["x","y"],"b":2}"#
/// );
/// # Ok::<(), apollo_json::JsonError>(())
/// ```
pub struct ValueBuilder {
    arena: Arena,
    root: Child,
    /// Overlays for containers opened for growth, flattened at seal.
    array_overlays: Vec<(NodeId, Vec<Child>)>,
    object_overlays: Vec<(NodeId, Vec<Entry>)>,
    /// What a non-finite float written through this builder becomes. Set once
    /// at construction rather than passed per write, so a cursor handed out by
    /// the builder inherits it.
    non_finite: NonFinite,
}

impl ValueBuilder {
    /// The arena backing this builder, for read access through a cursor.
    pub(crate) fn arena(&self) -> &Arena {
        &self.arena
    }

    /// A read-only view of the builder's root, including writes not yet
    /// sealed; see [`BuilderRef`](crate::BuilderRef).
    pub fn value(&self) -> crate::BuilderRef<'_> {
        crate::peek::BuilderRef::new(self, self.root)
    }

    pub(crate) fn object_overlay(&self, index: usize) -> &(NodeId, Vec<Entry>) {
        &self.object_overlays[index]
    }

    pub(crate) fn array_overlay(&self, index: usize) -> &(NodeId, Vec<Child>) {
        &self.array_overlays[index]
    }
}

impl ValueBuilder {
    /// A builder holding an empty object.
    pub fn new() -> Self {
        let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
        let root = arena.push_node(Node::Object(SlabRef::EMPTY));
        ValueBuilder {
            arena,
            root: Child::local(root),
            array_overlays: Vec::new(),
            object_overlays: Vec::new(),
            non_finite: NonFinite::Reject,
        }
    }

    /// A builder holding an empty array, for assembling a document whose root
    /// is an array rather than an object.
    pub fn new_array() -> Self {
        let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
        let root = arena.push_node(Node::Array(SlabRef::EMPTY));
        ValueBuilder {
            arena,
            root: Child::local(root),
            array_overlays: Vec::new(),
            object_overlays: Vec::new(),
            non_finite: NonFinite::Reject,
        }
    }

    /// Turns this builder into one that coerces non-finite floats to `null`
    /// instead of reporting [`JsonError::NonFiniteNumber`], at any depth of a
    /// pending tree.
    ///
    /// This is for the constructors that take plain Rust data
    /// ([`Value::array`], [`Value::object`]): they promise not to hand a
    /// `NonFiniteNumber` error back to a caller who never asked about JSON's
    /// number range. Explicit mutations keep reporting it.
    pub(crate) fn coercing_non_finite(mut self) -> Self {
        self.non_finite = NonFinite::AsNull;
        self
    }

    /// Opens a value for mutation.
    ///
    /// When `value` holds the last reference to its arena, the builder takes
    /// the arena over and mutates in place. Otherwise — including any handle
    /// to a subtree of an arena that other handles still share — mutations
    /// copy the touched path into a fresh arena and every other subtree is
    /// shared by reference, so holders of the original value never observe
    /// changes.
    pub fn from_value(value: Value) -> Self {
        let Value { arena, node: root } = value;
        match Arc::try_unwrap(arena) {
            Ok(owned) => ValueBuilder {
                arena: owned,
                root: Child::local(root),
                array_overlays: Vec::new(),
                object_overlays: Vec::new(),
                non_finite: NonFinite::Reject,
            },
            Err(shared) => {
                let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
                let root = arena.push_foreign(shared, root);
                ValueBuilder {
                    arena,
                    root,
                    array_overlays: Vec::new(),
                    object_overlays: Vec::new(),
                    non_finite: NonFinite::Reject,
                }
            }
        }
    }

    /// Freezes the builder into an immutable, shareable value, flattening
    /// every mutable overlay into packed arena slabs.
    pub fn seal(mut self) -> Value {
        for (id, items) in std::mem::take(&mut self.array_overlays) {
            let slab = self.arena.alloc_children(&items);
            self.arena.set_node(id, Node::Array(slab));
        }
        for (id, members) in std::mem::take(&mut self.object_overlays) {
            let slab = self.arena.alloc_entries(&members);
            self.arena.set_node(id, Node::Object(slab));
        }
        match self.root.as_local() {
            Some(id) => Value::rooted(self.arena, id),
            // Never mutated: the sealed value is the adopted one.
            None => {
                let fref = self
                    .arena
                    .foreign_ref(self.root.as_foreign().expect("root is foreign"));
                Value {
                    arena: Arc::clone(&fref.arena),
                    node: fref.node,
                }
            }
        }
    }

    /// Writes `value` at `segment` of the root: replace-or-append for object
    /// keys, replace for in-range indexes, append for an index equal to the
    /// length. Accepts anything convertible to [`NewValue`], so
    /// `builder.set("name", "bob")` just works.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the segment does not apply to the
    /// root (key on a non-object, index out of range);
    /// [`JsonError::NonFiniteNumber`] for a non-finite float.
    pub fn set<'s, 'v>(
        &mut self,
        segment: impl Into<PathSegment<'s>>,
        value: impl Into<NewValue<'v>>,
    ) -> Result<(), JsonError> {
        let root = self.localize_root();
        self.set_at(root, &segment.into(), value.into())
    }

    /// Removes `segment` of the root (an object member, or an array element
    /// shifting later elements left), returning whether anything was
    /// removed.
    pub fn remove<'s>(&mut self, segment: impl Into<PathSegment<'s>>) -> bool {
        let root = self.localize_root();
        self.remove_at(root, &segment.into())
    }

    /// Appends `value` to the root array.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the root is not an array;
    /// [`JsonError::NonFiniteNumber`] for a non-finite float.
    pub fn push<'v>(&mut self, value: impl Into<NewValue<'v>>) -> Result<(), JsonError> {
        let root = self.localize_root();
        self.push_at(root, value.into())
    }

    /// A mutable cursor at `segment` of the root; see
    /// [`ValueMut`](crate::ValueMut). Navigation localizes the copy-on-write
    /// spine once, so edits at the cursor are local operations. A missing
    /// object key is created as an empty object.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the segment does not resolve (key on
    /// a non-object, index out of range).
    pub fn get_mut<'s>(
        &mut self,
        segment: impl Into<PathSegment<'s>>,
    ) -> Result<crate::ValueMut<'_>, JsonError> {
        let root = self.localize_root();
        let node = self.navigate_mut(root, &segment.into())?;
        Ok(crate::cursor::cursor(self, node))
    }

    /// A mutable cursor at the root itself, for callers that walk a
    /// document uniformly (root included) rather than always addressing a
    /// child segment.
    pub fn root_mut(&mut self) -> crate::ValueMut<'_> {
        let root = self.localize_root();
        crate::cursor::cursor(self, root)
    }

    /// Localized navigation one segment down from the local node `at`,
    /// creating missing object keys as empty objects.
    pub(crate) fn navigate_mut(
        &mut self,
        at: NodeId,
        segment: &PathSegment<'_>,
    ) -> Result<NodeId, JsonError> {
        self.descend(at, segment, 0)
    }

    /// Writes `value` at a computed `path`, copying shared nodes along the
    /// path into the builder's arena (path copying) and mutating owned nodes
    /// in place. For hand-written edits prefer [`ValueBuilder::set`] and
    /// the [`ValueMut`](crate::ValueMut) cursor.
    ///
    /// An empty path replaces the root. Missing intermediate object keys are
    /// created as empty objects; a missing final key is appended; a final
    /// index equal to the array length appends.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when a segment traverses a scalar, an
    /// intermediate index is out of range, or the final segment indexes past
    /// the end of an array; [`JsonError::NonFiniteNumber`] for a non-finite
    /// [`NewValue::Float`].
    pub fn set_path<'v>(
        &mut self,
        path: &[PathSegment<'_>],
        value: impl Into<NewValue<'v>>,
    ) -> Result<(), JsonError> {
        let Some((last, parents)) = path.split_last() else {
            self.root = self.new_child(value.into(), self.non_finite)?;
            return Ok(());
        };
        let mut cur = self.localize_root();
        for (i, seg) in parents.iter().enumerate() {
            cur = self.descend(cur, seg, i)?;
        }
        self.set_at(cur, last, value.into())
            .map_err(|error| match error {
                JsonError::PathNotFound { .. } => JsonError::PathNotFound {
                    segment: path.len() - 1,
                },
                other => other,
            })
    }

    /// Writes `value` at `segment` of the local node `at`: replace-or-append
    /// for object keys, replace for in-range indexes, append for an index
    /// equal to the length.
    pub(crate) fn set_at(
        &mut self,
        at: NodeId,
        segment: &PathSegment<'_>,
        value: NewValue<'_>,
    ) -> Result<(), JsonError> {
        let child = self.new_child(value, self.non_finite)?;
        match *segment {
            PathSegment::Key(key) => match self.find_member(at, key) {
                Some(j) => self.set_object_child(at, j, child),
                None => {
                    if shape_of(self.arena.node(at)) != Shape::Object {
                        return Err(JsonError::PathNotFound { segment: 0 });
                    }
                    let key = KeyRef::Owned(self.arena.alloc_text(key));
                    let overlay = self.open_object(at);
                    self.object_overlays[overlay].1.push(Entry { key, child });
                }
            },
            PathSegment::Index(index) => {
                if shape_of(self.arena.node(at)) != Shape::Array {
                    return Err(JsonError::PathNotFound { segment: 0 });
                }
                let len = self.array_len(at);
                if index < len {
                    self.set_array_child(at, index, child);
                } else if index == len {
                    let overlay = self.open_array(at);
                    self.array_overlays[overlay].1.push(child);
                } else {
                    return Err(JsonError::PathNotFound { segment: 0 });
                }
            }
        }
        Ok(())
    }

    /// Removes the value at a computed `path`: an object member (by key) or
    /// an array element (by index, shifting later elements left). Returns
    /// whether anything was removed — a path that does not resolve removes
    /// nothing. Shared nodes along the path are copied on write, exactly as
    /// for [`ValueBuilder::set_path`].
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] for an empty path (the root cannot be
    /// removed).
    pub fn remove_path(&mut self, path: &[PathSegment<'_>]) -> Result<bool, JsonError> {
        let Some((last, parents)) = path.split_last() else {
            return Err(JsonError::PathNotFound { segment: 0 });
        };
        let Some(cur) = self.walk_localizing(parents) else {
            return Ok(false);
        };
        Ok(self.remove_at(cur, last))
    }

    /// Removes `segment` of the local node `at`; `false` when it does not
    /// resolve.
    pub(crate) fn remove_at(&mut self, at: NodeId, segment: &PathSegment<'_>) -> bool {
        match *segment {
            PathSegment::Key(key) => match self.find_member(at, key) {
                Some(j) => {
                    let overlay = self.open_object(at);
                    self.object_overlays[overlay].1.remove(j);
                    true
                }
                None => false,
            },
            PathSegment::Index(index) => {
                if shape_of(self.arena.node(at)) == Shape::Array && index < self.array_len(at) {
                    let overlay = self.open_array(at);
                    self.array_overlays[overlay].1.remove(index);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Appends `value` to the array at a computed `path` (the empty path
    /// addresses the root).
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the path does not resolve to an
    /// existing array; [`JsonError::NonFiniteNumber`] for a non-finite
    /// [`NewValue::Float`].
    pub fn push_path<'v>(
        &mut self,
        path: &[PathSegment<'_>],
        value: impl Into<NewValue<'v>>,
    ) -> Result<(), JsonError> {
        let target = self.walk_localizing(path).ok_or(JsonError::PathNotFound {
            segment: path.len().saturating_sub(1),
        })?;
        self.push_at(target, value.into())
            .map_err(|error| match error {
                JsonError::PathNotFound { .. } => JsonError::PathNotFound {
                    segment: path.len().saturating_sub(1),
                },
                other => other,
            })
    }

    /// Appends `value` to the local array node `at`.
    pub(crate) fn push_at(&mut self, at: NodeId, value: NewValue) -> Result<(), JsonError> {
        if shape_of(self.arena.node(at)) != Shape::Array {
            return Err(JsonError::PathNotFound { segment: 0 });
        }
        let child = self.new_child(value, self.non_finite)?;
        let overlay = self.open_array(at);
        self.array_overlays[overlay].1.push(child);
        Ok(())
    }

    /// Walks `path` from the root, localizing each node so the destination
    /// can be mutated; `None` when the path does not resolve (nothing is
    /// created).
    fn walk_localizing(&mut self, path: &[PathSegment<'_>]) -> Option<NodeId> {
        let mut cur = self.localize_root();
        for seg in path {
            let child = match seg {
                PathSegment::Key(key) => {
                    let j = self.find_member(cur, key)?;
                    self.object_child(cur, j)
                }
                PathSegment::Index(index) => {
                    if shape_of(self.arena.node(cur)) != Shape::Array
                        || *index >= self.array_len(cur)
                    {
                        return None;
                    }
                    self.array_child(cur, *index)
                }
            };
            let local = self.localize_child(child);
            match seg {
                PathSegment::Key(key) => {
                    let j = self.find_member(cur, key).expect("member found above");
                    self.set_object_child(cur, j, Child::local(local));
                }
                PathSegment::Index(index) => {
                    self.set_array_child(cur, *index, Child::local(local));
                }
            }
            cur = local;
        }
        Some(cur)
    }

    /// Deep-merges `other` into this document: object keys union recursively,
    /// array elements merge index-wise (extras appended), scalars and
    /// mismatched shapes replace. Subtrees taken from `other` are adopted by
    /// reference, never copied.
    pub fn merge(&mut self, other: &Value) {
        let src = Arc::clone(&other.arena);
        let src_shape = shape_of(src.node(other.node));
        let root_shape = self.child_shape(self.root);
        if src_shape == Shape::Scalar || src_shape != root_shape {
            self.root = self.import_child(src, other.node);
            return;
        }
        let root = self.localize_root();
        let mut stack: Vec<(NodeId, Arc<Arena>, NodeId)> = vec![(root, src, other.node)];
        while let Some((target, src, source)) = stack.pop() {
            match src.node(source) {
                Node::Object(slab) => {
                    for entry in src.entries(slab) {
                        let (sowner, snode) = src.resolve_owner(entry.child);
                        let key_text = src.key_unescaped(entry.key);
                        match self.find_member_bytes(target, key_text.as_bytes()) {
                            Some(j) => {
                                let tchild = self.object_child(target, j);
                                self.merge_slot(tchild, sowner, snode, &mut stack, |b, c| {
                                    b.set_object_child(target, j, c);
                                });
                            }
                            None => {
                                let key = KeyRef::Owned(self.arena.alloc_text(&key_text));
                                let child = self.import_child(sowner, snode);
                                let overlay = self.open_object(target);
                                self.object_overlays[overlay].1.push(Entry { key, child });
                            }
                        }
                    }
                }
                Node::Array(slab) => {
                    for (i, &schild) in src.children(slab).iter().enumerate() {
                        let (sowner, snode) = src.resolve_owner(schild);
                        if i < self.array_len(target) {
                            let tchild = self.array_child(target, i);
                            self.merge_slot(tchild, sowner, snode, &mut stack, |b, c| {
                                b.set_array_child(target, i, c);
                            });
                        } else {
                            let child = self.import_child(sowner, snode);
                            let overlay = self.open_array(target);
                            self.array_overlays[overlay].1.push(child);
                        }
                    }
                }
                _ => unreachable!("only containers are stacked"),
            }
        }
    }

    /// Merges one source child into one target slot: descend when both sides
    /// are the same container shape, replace (by reference) otherwise.
    fn merge_slot(
        &mut self,
        tchild: Child,
        sowner: Arc<Arena>,
        snode: NodeId,
        stack: &mut Vec<(NodeId, Arc<Arena>, NodeId)>,
        write: impl FnOnce(&mut Self, Child),
    ) {
        let sshape = shape_of(sowner.node(snode));
        if sshape != Shape::Scalar && sshape == self.child_shape(tchild) {
            let tlocal = self.localize_child(tchild);
            write(self, Child::local(tlocal));
            stack.push((tlocal, sowner, snode));
        } else {
            // Replaced slots stay by reference: a later chunk may overwrite
            // them again, and copied text would pile up dead in the arena.
            let adopted = self.arena.push_foreign(sowner, snode);
            write(self, adopted);
        }
    }

    /// Brings one merge-source value into this document. Containers are
    /// adopted by reference — that is where sharing pays — and so are
    /// escaped strings, whose original spelling only survives as a span
    /// into the source input. Other scalars are copied into the local
    /// arena: a few bytes of text cost less than a foreign-table entry
    /// plus its refcount traffic on every clone, drop, and resolve.
    fn import_child(&mut self, owner: Arc<Arena>, node: NodeId) -> Child {
        let copied = match owner.node(node) {
            Node::Null => Node::Null,
            Node::Bool(b) => Node::Bool(b),
            Node::Number(span) => {
                Node::OwnedNumber(self.arena.alloc_text(owner.input_utf8(span).as_str()))
            }
            Node::OwnedNumber(text) => {
                Node::OwnedNumber(self.arena.alloc_text(owner.text_str(text)))
            }
            // Escape-free string content re-escapes to itself.
            Node::String {
                span,
                escaped: false,
            } => Node::OwnedString(self.arena.alloc_text(owner.input_utf8(span).as_str())),
            Node::OwnedString(text) => {
                Node::OwnedString(self.arena.alloc_text(owner.text_str(text)))
            }
            _ => return self.arena.push_foreign(owner, node),
        };
        Child::local(self.arena.push_node(copied))
    }

    /// Resolves an intermediate path segment, localizing the child so the
    /// next step can mutate it. A key missing from an object is created as an
    /// empty object.
    fn descend(
        &mut self,
        cur: NodeId,
        seg: &PathSegment<'_>,
        index: usize,
    ) -> Result<NodeId, JsonError> {
        let child = match seg {
            PathSegment::Key(key) => {
                if shape_of(self.arena.node(cur)) != Shape::Object {
                    return Err(JsonError::PathNotFound { segment: index });
                }
                match self.find_member(cur, key) {
                    Some(j) => self.object_child(cur, j),
                    None => {
                        let id = self.arena.push_node(Node::Object(SlabRef::EMPTY));
                        let key = KeyRef::Owned(self.arena.alloc_text(key));
                        let overlay = self.open_object(cur);
                        self.object_overlays[overlay].1.push(Entry {
                            key,
                            child: Child::local(id),
                        });
                        return Ok(id);
                    }
                }
            }
            PathSegment::Index(i) => {
                if shape_of(self.arena.node(cur)) != Shape::Array {
                    return Err(JsonError::PathNotFound { segment: index });
                }
                if *i >= self.array_len(cur) {
                    return Err(JsonError::PathNotFound { segment: index });
                }
                self.array_child(cur, *i)
            }
        };
        let local = self.localize_child(child);
        match seg {
            PathSegment::Key(key) => {
                let j = self.find_member(cur, key).expect("member found above");
                self.set_object_child(cur, j, Child::local(local));
            }
            PathSegment::Index(i) => self.set_array_child(cur, *i, Child::local(local)),
        }
        Ok(local)
    }

    /// Ensures the root is a node in the builder's arena.
    fn localize_root(&mut self) -> NodeId {
        let id = self.localize_child(self.root);
        self.root = Child::local(id);
        id
    }

    /// Ensures a child is a node in the builder's arena, shallow-copying a
    /// foreign node: its own structure is copied, its children become
    /// references into the source arena (path copying, one level at a time).
    /// Foreign containers open directly into a mutable overlay.
    fn localize_child(&mut self, child: Child) -> NodeId {
        if let Some(id) = child.as_local() {
            return id;
        }
        let fref = self
            .arena
            .foreign_ref(child.as_foreign().expect("child is foreign"));
        let (src, node) = (Arc::clone(&fref.arena), fref.node);
        let copied = match src.node(node) {
            Node::Null => Node::Null,
            Node::Bool(b) => Node::Bool(b),
            Node::Number(span) => {
                Node::OwnedNumber(self.arena.alloc_text(src.input_utf8(span).as_str()))
            }
            Node::OwnedNumber(text) => Node::OwnedNumber(self.arena.alloc_text(src.text_str(text))),
            Node::String {
                span,
                escaped: false,
            } => Node::OwnedString(self.arena.alloc_text(src.input_utf8(span).as_str())),
            Node::String {
                span,
                escaped: true,
            } => Node::OwnedString(
                self.arena
                    .alloc_text(&crate::text::unescape(src.input_utf8(span))),
            ),
            Node::OwnedString(text) => Node::OwnedString(self.arena.alloc_text(src.text_str(text))),
            Node::Array(slab) => {
                let imported: Vec<Child> = src
                    .children(slab)
                    .iter()
                    .map(|&c| {
                        let (owner, id) = src.resolve_owner(c);
                        self.arena.push_foreign(owner, id)
                    })
                    .collect();
                let id = self
                    .arena
                    .push_node(Node::MutArray(overlay_index(self.array_overlays.len())));
                self.array_overlays.push((id, imported));
                return id;
            }
            Node::Object(slab) => {
                let imported: Vec<Entry> = src
                    .entries(slab)
                    .iter()
                    .map(|entry| {
                        let (owner, id) = src.resolve_owner(entry.child);
                        let key =
                            KeyRef::Owned(self.arena.alloc_text(&src.key_unescaped(entry.key)));
                        Entry {
                            key,
                            child: self.arena.push_foreign(owner, id),
                        }
                    })
                    .collect();
                let id = self
                    .arena
                    .push_node(Node::MutObject(overlay_index(self.object_overlays.len())));
                self.object_overlays.push((id, imported));
                return id;
            }
            Node::MutArray(_) | Node::MutObject(_) => {
                unreachable!("sealed documents contain no overlays")
            }
        };
        self.arena.push_node(copied)
    }

    /// Opens a local container for growth, copying its slab into a mutable
    /// overlay once; further growth appends to the overlay.
    fn open_object(&mut self, id: NodeId) -> usize {
        match self.arena.node(id) {
            Node::MutObject(i) => i as usize,
            Node::Object(slab) => {
                // Opened containers usually grow; leave headroom so the
                // first appends do not immediately reallocate.
                let entries = self.arena.entries(slab);
                let mut members = Vec::with_capacity(entries.len() * 2 + 8);
                members.extend_from_slice(entries);
                let index = self.object_overlays.len();
                self.arena
                    .set_node(id, Node::MutObject(overlay_index(index)));
                self.object_overlays.push((id, members));
                index
            }
            _ => unreachable!("caller checked the node is an object"),
        }
    }

    fn open_array(&mut self, id: NodeId) -> usize {
        match self.arena.node(id) {
            Node::MutArray(i) => i as usize,
            Node::Array(slab) => {
                let children = self.arena.children(slab);
                let mut items = Vec::with_capacity(children.len() * 2 + 8);
                items.extend_from_slice(children);
                let index = self.array_overlays.len();
                self.arena
                    .set_node(id, Node::MutArray(overlay_index(index)));
                self.array_overlays.push((id, items));
                index
            }
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    /// Writes a [`NewValue`] into this builder's arena and returns the child
    /// slot referring to it.
    ///
    /// A pending container ([`NewValue::Array`] / [`NewValue::Object`]) is
    /// written with an explicit frame stack rather than by recursing, so a deep
    /// pending tree cannot overflow the thread stack here — the same guarantee
    /// the parser gives. Elements are written depth-first so that a container's
    /// slab is allocated once, after all of its children exist.
    ///
    /// `non_finite` decides what a non-finite float becomes; it applies at
    /// every depth, so the constructors in [`crate::construct`] keep promising
    /// `null` for a whole pending tree and not merely its root.
    fn new_child(
        &mut self,
        value: NewValue<'_>,
        non_finite: NonFinite,
    ) -> Result<Child, JsonError> {
        let mut stack: Vec<Frame<'_>> = Vec::new();
        let mut scratch = Scratch::default();
        // The value to write next, or `None` once it has become a `child` and
        // the innermost frame should advance.
        let mut next = Some(value);
        loop {
            // Descend: a container opens a frame, a leaf becomes a child
            // directly.
            let mut child = match next.take() {
                Some(NewValue::Array(items)) => {
                    stack.push(Frame::Array {
                        items: items.into_iter(),
                        base: scratch.children.len(),
                    });
                    continue;
                }
                Some(NewValue::Object(members)) => {
                    stack.push(Frame::Object {
                        members: members.into_iter(),
                        base: scratch.entries.len(),
                        key: None,
                    });
                    continue;
                }
                Some(leaf) => Some(self.leaf_child(leaf, non_finite)?),
                None => None,
            };
            // Ascend: hand the finished child to its parent frame, then close
            // any frame that has no members left, repeating while closing a
            // frame finishes a child for the frame above it.
            loop {
                let Some(frame) = stack.last_mut() else {
                    // No parent frame: this child is the whole value.
                    return Ok(child.expect("the outermost value produced a child"));
                };
                if let Some(child) = child.take() {
                    Self::accept_child(&mut self.arena, &mut scratch, frame, child);
                }
                match frame.next_value() {
                    // Another member to descend into.
                    Some(value) => {
                        next = Some(value);
                        break;
                    }
                    // Frame complete: allocate its slab and let the loop hand
                    // the resulting child to the frame above.
                    None => {
                        let closed = stack.pop().expect("the frame was just borrowed");
                        child = Some(self.close_frame(&mut scratch, closed));
                    }
                }
            }
        }
    }

    /// Writes a non-container [`NewValue`] into the arena.
    fn leaf_child(
        &mut self,
        value: NewValue<'_>,
        non_finite: NonFinite,
    ) -> Result<Child, JsonError> {
        Ok(match value {
            NewValue::Null => Child::local(self.arena.push_node(Node::Null)),
            NewValue::Bool(b) => Child::local(self.arena.push_node(Node::Bool(b))),
            NewValue::Int(i) => {
                // Formatted into a stack buffer: `to_string()` here would be one
                // heap allocation per number written, which for a pending tree
                // of numbers is the whole per-member cost.
                let text = self.arena.alloc_text(itoa::Buffer::new().format(i));
                Child::local(self.arena.push_node(Node::OwnedNumber(text)))
            }
            NewValue::Float(f) if f.is_finite() => {
                // `ryu` is what serde_json formats finite floats through, so
                // this is the same text without the intermediate `String`.
                let text = self.arena.alloc_text(ryu::Buffer::new().format_finite(f));
                Child::local(self.arena.push_node(Node::OwnedNumber(text)))
            }
            NewValue::Float(_) => match non_finite {
                NonFinite::AsNull => Child::local(self.arena.push_node(Node::Null)),
                NonFinite::Reject => return Err(JsonError::NonFiniteNumber),
            },
            NewValue::String(s) => {
                let text = self.arena.alloc_text(&s);
                Child::local(self.arena.push_node(Node::OwnedString(text)))
            }
            NewValue::Node(handle) => self.arena.push_foreign(handle.arena, handle.node),
            NewValue::Array(_) | NewValue::Object(_) => {
                unreachable!("containers are written through frames, not as leaves")
            }
        })
    }

    /// Records a finished child in its parent frame, interning the object key
    /// held by the frame. A repeated key keeps its first position and takes the
    /// later value, matching the parser.
    fn accept_child(arena: &mut Arena, scratch: &mut Scratch, frame: &mut Frame<'_>, child: Child) {
        match frame {
            Frame::Array { .. } => scratch.children.push(child),
            Frame::Object { base, key, .. } => {
                let key = key
                    .take()
                    .expect("an object frame takes its key before the member's value");
                // Only this frame's own run can hold a duplicate; entries below
                // `base` belong to enclosing objects.
                let members = &mut scratch.entries[*base..];
                match members
                    .iter()
                    .position(|entry| arena.key_matches_str(entry.key, &key))
                {
                    Some(existing) => members[existing].child = child,
                    None => {
                        let key = KeyRef::Owned(arena.alloc_text(&key));
                        scratch.entries.push(Entry { key, child });
                    }
                }
            }
        }
    }

    /// Packs a finished frame's run of children into a slab, pushes its
    /// container node, and returns the scratch buffer to the frame's base so the
    /// enclosing container's run is on top again.
    fn close_frame(&mut self, scratch: &mut Scratch, frame: Frame<'_>) -> Child {
        let node = match frame {
            Frame::Array { base, .. } => {
                let slab = self.arena.alloc_children(&scratch.children[base..]);
                scratch.children.truncate(base);
                Node::Array(slab)
            }
            Frame::Object { base, .. } => {
                let slab = self.arena.alloc_entries(&scratch.entries[base..]);
                scratch.entries.truncate(base);
                Node::Object(slab)
            }
        };
        Child::local(self.arena.push_node(node))
    }

    fn find_member(&self, obj: NodeId, key: &str) -> Option<usize> {
        self.find_member_bytes(obj, key.as_bytes())
    }

    fn find_member_bytes(&self, obj: NodeId, key: &[u8]) -> Option<usize> {
        let members = self.object_members(obj)?;
        members
            .iter()
            .position(|entry| self.arena.key_matches_bytes(entry.key, key))
    }

    /// The members of a local object, reading through any overlay.
    fn object_members(&self, obj: NodeId) -> Option<&[Entry]> {
        match self.arena.node(obj) {
            Node::Object(slab) => Some(self.arena.entries(slab)),
            Node::MutObject(i) => Some(&self.object_overlays[i as usize].1),
            _ => None,
        }
    }

    fn object_child(&self, obj: NodeId, index: usize) -> Child {
        self.object_members(obj).expect("node is an object")[index].child
    }

    fn set_object_child(&mut self, obj: NodeId, index: usize, child: Child) {
        match self.arena.node(obj) {
            Node::Object(slab) => self.arena.entries_mut(slab)[index].child = child,
            Node::MutObject(i) => self.object_overlays[i as usize].1[index].child = child,
            _ => unreachable!("caller checked the node is an object"),
        }
    }

    fn array_len(&self, arr: NodeId) -> usize {
        match self.arena.node(arr) {
            Node::Array(slab) => slab.len(),
            Node::MutArray(i) => self.array_overlays[i as usize].1.len(),
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    fn array_child(&self, arr: NodeId, index: usize) -> Child {
        match self.arena.node(arr) {
            Node::Array(slab) => self.arena.children(slab)[index],
            Node::MutArray(i) => self.array_overlays[i as usize].1[index],
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    fn set_array_child(&mut self, arr: NodeId, index: usize, child: Child) {
        match self.arena.node(arr) {
            Node::Array(slab) => self.arena.children_mut(slab)[index] = child,
            Node::MutArray(i) => self.array_overlays[i as usize].1[index] = child,
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    /// The shape of a child, resolving foreign references.
    fn child_shape(&self, child: Child) -> Shape {
        let (arena, id) = crate::arena::resolve(&self.arena, child);
        shape_of(arena.node(id))
    }
}

impl Default for ValueBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ValueBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueBuilder").finish_non_exhaustive()
    }
}

fn overlay_index(index: usize) -> u32 {
    u32::try_from(index).expect("overlay count within range")
}
