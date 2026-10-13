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
//! Container children live in arena slabs. Replacing a child slot writes the
//! slab in place. A container that grows (new keys, appended elements) grows
//! its run in the slabs: into room it already has, which vacant slots after
//! its live elements mark; at the end of the chunk being filled when it is
//! already there; or by moving to a fresh run with room to spare. A node's
//! slab always holds exactly its live elements, so the document is readable
//! as it stands and [`ValueBuilder::seal`] has nothing to flatten.

use std::borrow::Cow;
use std::sync::Arc;

use crate::arena::{Arena, ForeignId};
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
        Node::Object(_) => Shape::Object,
        Node::Array(_) => Shape::Array,
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
    /// Reused while a shared container is copied on write, so the copy goes
    /// into the arena's slabs without a buffer of its own.
    import_children: Vec<Child>,
    import_entries: Vec<Entry>,
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
}

impl ValueBuilder {
    /// A builder holding an empty object.
    pub fn new() -> Self {
        let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
        let root = arena.push_node(Node::Object(SlabRef::EMPTY));
        ValueBuilder {
            arena,
            root: Child::local(root),
            import_children: Vec::new(),
            import_entries: Vec::new(),
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
            import_children: Vec::new(),
            import_entries: Vec::new(),
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
            Ok(mut owned) => ValueBuilder {
                arena: {
                    owned.mark_edited();
                    owned
                },
                root: Child::local(root),
                import_children: Vec::new(),
                import_entries: Vec::new(),
                non_finite: NonFinite::Reject,
            },
            Err(shared) => {
                let mut arena = Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE);
                let root = arena.push_foreign(shared, root);
                ValueBuilder {
                    arena,
                    root,
                    import_children: Vec::new(),
                    import_entries: Vec::new(),
                    non_finite: NonFinite::Reject,
                }
            }
        }
    }

    /// Freezes the builder into an immutable, shareable value. Containers
    /// are already in their packed form, so this only wraps the arena.
    pub fn seal(self) -> Value {
        match self.root.as_local() {
            Some(id) => Value::rooted(self.arena, id),
            None => {
                let id = self.root.as_foreign().expect("root is foreign");
                match self.arena.foreign_target(id) {
                    // Never mutated: the sealed value is the adopted one.
                    (Some(arena), node) => Value {
                        arena: Arc::clone(arena),
                        node,
                    },
                    // The adopted root's arena was absorbed into this one.
                    (None, node) => Value::rooted(self.arena, node),
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
                    self.push_entry(at, Entry { key, child });
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
                    self.push_child(at, child);
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
                    self.remove_entry(at, j);
                    true
                }
                None => false,
            },
            PathSegment::Index(index) => {
                if shape_of(self.arena.node(at)) == Shape::Array && index < self.array_len(at) {
                    self.remove_child(at, index);
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
        self.push_child(at, child);
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
                                self.push_entry(target, Entry { key, child });
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
                            self.push_child(target, child);
                        }
                    }
                }
                _ => unreachable!("only containers are stacked"),
            }
        }
    }

    /// Deep-merges `other` into this document the way execution merges a
    /// subgraph response into the response being assembled.
    ///
    /// A member or element `other` has and this document lacks is taken from
    /// `other` whatever it is, `null` included. Where both hold an object, or
    /// both an array, the merge descends; arrays merge index-wise and extra
    /// elements are appended. Anywhere else `policy` decides whether the
    /// incoming value replaces the one in place.
    ///
    /// The merge takes `other` by value so that it can take its storage over.
    /// When `other` holds the last reference to its arena, that arena is
    /// absorbed: its storage moves into this document's arena and its nodes
    /// become local ones, so the subtrees the merge adopts from it are part of
    /// this document, and a later edit through them is in place rather than a
    /// copy. Arenas that only `other` referenced are absorbed the same way,
    /// provided its references into each one are to subtrees that do not
    /// overlap. Otherwise `other` is adopted by reference, as
    /// [`ValueBuilder::merge`] adopts it.
    ///
    /// ```
    /// use apollo_json::{BuilderRef, JsonKind, MergePolicy, Value, json};
    ///
    /// /// A `null` arriving never overwrites a value already in place.
    /// struct KeepOnNull;
    /// impl MergePolicy for KeepOnNull {
    ///     fn replace(&mut self, _: Option<&str>, _: BuilderRef<'_>, incoming: BuilderRef<'_>) -> bool {
    ///         incoming.kind() != JsonKind::Null
    ///     }
    /// }
    ///
    /// let mut builder = json!({"a": 1, "b": {"c": 2}}).edit();
    /// builder.merge_from(json!({"a": null, "b": {"d": 3}, "e": null}), &mut KeepOnNull);
    /// assert_eq!(builder.seal(), json!({"a": 1, "b": {"c": 2, "d": 3}, "e": null}));
    /// ```
    pub fn merge_from(&mut self, other: Value, policy: &mut impl MergePolicy) {
        let source = self.take_in(other);
        let root_shape = self.child_shape(self.root);
        if root_shape == Shape::Scalar || root_shape != self.source_shape(&source) {
            let incoming = self.source_view(&source);
            if policy.replace(
                None,
                crate::peek::BuilderRef::new(self, self.root),
                incoming,
            ) {
                self.root = match source {
                    MergeSource::Local(node) => Child::local(node),
                    MergeSource::Foreign(arena, node) => self.arena.push_foreign(arena, node),
                };
            }
            return;
        }
        let root = self.localize_root();
        // One frame per container being merged, so the stack is as deep as
        // the documents are, not as wide.
        let mut stack = vec![self.merge_frame(root, source)];
        while let Some(frame) = stack.last_mut() {
            if frame.next == frame.len {
                stack.pop();
                continue;
            }
            let index = frame.next;
            frame.next += 1;
            let (target, source, room) = (frame.target, &frame.source, frame.room);
            if let Some(nested) = self.merge_member(target, source, index, room, policy) {
                stack.push(nested);
            }
        }
    }

    /// Opens a frame merging `source` into the local container `target`,
    /// reserving exactly the room the members it lacks will take. An empty
    /// `target` has no run to hold room yet, so its frame carries the count
    /// and the first member appended allocates the run at that size.
    fn merge_frame(&mut self, target: NodeId, source: MergeSource) -> MergeFrame {
        let (arena, node) = match &source {
            MergeSource::Local(node) => (&self.arena, *node),
            MergeSource::Foreign(arena, node) => (&**arena, *node),
        };
        let (len, extra, object) = match arena.node(node) {
            Node::Object(slab) => {
                let absent = arena
                    .entries(slab)
                    .iter()
                    .filter(|entry| {
                        let key = arena.key_unescaped(entry.key);
                        self.find_member_bytes(target, key.as_bytes()).is_none()
                    })
                    .count();
                (slab.len(), absent, true)
            }
            Node::Array(slab) => (
                slab.len(),
                slab.len().saturating_sub(self.array_len(target)),
                false,
            ),
            _ => unreachable!("only containers are merged into"),
        };
        let empty = if object {
            self.object_slab(target).len() == 0
        } else {
            self.array_len(target) == 0
        };
        if extra > 0 && !empty {
            if object {
                self.reserve_entries(target, extra);
            } else {
                self.reserve_children(target, extra);
            }
        }
        MergeFrame {
            target,
            source,
            next: 0,
            len,
            room: if empty { extra } else { 0 },
        }
    }

    /// Merges member `index` of `source` into the local container `target`,
    /// returning a frame to push when the merge descends into it.
    fn merge_member(
        &mut self,
        target: NodeId,
        source: &MergeSource,
        index: usize,
        room: usize,
        policy: &mut impl MergePolicy,
    ) -> Option<MergeFrame> {
        match source {
            MergeSource::Local(node) => self.merge_local_member(target, *node, index, room, policy),
            MergeSource::Foreign(arena, node) => {
                self.merge_foreign_member(target, arena, *node, index, room, policy)
            }
        }
    }

    /// [`ValueBuilder::merge_member`] for a container of this arena: a
    /// member taken over keeps its key and child as they are.
    fn merge_local_member(
        &mut self,
        target: NodeId,
        node: NodeId,
        index: usize,
        room: usize,
        policy: &mut impl MergePolicy,
    ) -> Option<MergeFrame> {
        match self.arena.node(node) {
            Node::Object(slab) => {
                let entry = self.arena.entries(slab)[index];
                let found = {
                    let key = self.arena.key_unescaped(entry.key);
                    self.find_member_bytes(target, key.as_bytes())
                };
                let Some(j) = found else {
                    self.push_entry_with_room(target, entry, room);
                    return None;
                };
                let incoming = self.merge_source(entry.child);
                self.merge_slot_from(
                    target,
                    Slot::Member(j),
                    MergeKey::Local(entry.key),
                    incoming,
                    policy,
                )
            }
            Node::Array(slab) => {
                let child = self.arena.children(slab)[index];
                if index < self.array_len(target) {
                    let incoming = self.merge_source(child);
                    self.merge_slot_from(
                        target,
                        Slot::Element(index),
                        MergeKey::None,
                        incoming,
                        policy,
                    )
                } else {
                    self.push_child_with_room(target, child, room);
                    None
                }
            }
            _ => unreachable!("only containers are merged into"),
        }
    }

    /// [`ValueBuilder::merge_member`] for a container of another arena: what
    /// is taken over is adopted as [`ValueBuilder::merge`] adopts it.
    fn merge_foreign_member(
        &mut self,
        target: NodeId,
        src: &Arc<Arena>,
        node: NodeId,
        index: usize,
        room: usize,
        policy: &mut impl MergePolicy,
    ) -> Option<MergeFrame> {
        match src.node(node) {
            Node::Object(slab) => {
                let entry = src.entries(slab)[index];
                let key = src.key_unescaped(entry.key);
                let (owner, child) = src.resolve_owner(entry.child);
                match self.find_member_bytes(target, key.as_bytes()) {
                    None => {
                        let key = KeyRef::Owned(self.arena.alloc_text(&key));
                        let child = self.import_child(owner, child);
                        self.push_entry_with_room(target, Entry { key, child }, room);
                        None
                    }
                    Some(j) => {
                        let key = MergeKey::Text(key.into_owned());
                        let incoming = MergeSource::Foreign(owner, child);
                        self.merge_slot_from(target, Slot::Member(j), key, incoming, policy)
                    }
                }
            }
            Node::Array(slab) => {
                let (owner, child) = src.resolve_owner(src.children(slab)[index]);
                if index < self.array_len(target) {
                    let incoming = MergeSource::Foreign(owner, child);
                    self.merge_slot_from(
                        target,
                        Slot::Element(index),
                        MergeKey::None,
                        incoming,
                        policy,
                    )
                } else {
                    let child = self.import_child(owner, child);
                    self.push_child_with_room(target, child, room);
                    None
                }
            }
            _ => unreachable!("only containers are merged into"),
        }
    }

    /// Merges `incoming` into one slot of the local container `target`:
    /// descends when both are the same container kind, otherwise lets
    /// `policy` decide whether `incoming` replaces what is there. Returns a
    /// frame to push when it descends.
    fn merge_slot_from(
        &mut self,
        target: NodeId,
        slot: Slot,
        key: MergeKey,
        incoming: MergeSource,
        policy: &mut impl MergePolicy,
    ) -> Option<MergeFrame> {
        let existing = match slot {
            Slot::Member(j) => self.object_child(target, j),
            Slot::Element(i) => self.array_child(target, i),
        };
        let shape = self.child_shape(existing);
        if shape != Shape::Scalar && shape == self.source_shape(&incoming) {
            let local = self.localize_child(existing);
            self.write_slot(target, slot, Child::local(local));
            return Some(self.merge_frame(local, incoming));
        }
        let replace = {
            let key = match &key {
                MergeKey::None => None,
                MergeKey::Local(key) => Some(self.arena.key_unescaped(*key)),
                MergeKey::Text(key) => Some(Cow::Borrowed(key.as_str())),
            };
            policy.replace(
                key.as_deref(),
                crate::peek::BuilderRef::new(self, existing),
                self.source_view(&incoming),
            )
        };
        if replace {
            let child = match incoming {
                MergeSource::Local(node) => Child::local(node),
                // Replaced slots stay by reference, as `merge` keeps them.
                MergeSource::Foreign(owner, node) => self.arena.push_foreign(owner, node),
            };
            self.write_slot(target, slot, child);
        }
        None
    }

    fn write_slot(&mut self, target: NodeId, slot: Slot, child: Child) {
        match slot {
            Slot::Member(j) => self.set_object_child(target, j, child),
            Slot::Element(i) => self.set_array_child(target, i, child),
        }
    }

    /// The shape of a merge source.
    fn source_shape(&self, source: &MergeSource) -> Shape {
        match source {
            MergeSource::Local(node) => shape_of(self.arena.node(*node)),
            MergeSource::Foreign(arena, node) => shape_of(arena.node(*node)),
        }
    }

    /// A read-only view of a merge source, for a policy to inspect.
    fn source_view<'a>(&'a self, source: &'a MergeSource) -> crate::BuilderRef<'a> {
        match source {
            MergeSource::Local(node) => crate::peek::BuilderRef::new(self, Child::local(*node)),
            MergeSource::Foreign(arena, node) => crate::peek::BuilderRef::in_arena(arena, *node),
        }
    }

    /// Where a merge reads a container from: this arena, or another one.
    fn merge_source(&self, child: Child) -> MergeSource {
        match self.arena.local_node(child) {
            Some(node) => MergeSource::Local(node),
            None => {
                let id = child.as_foreign().expect("child is foreign");
                let (arena, node) = self.arena.foreign_target(id);
                MergeSource::Foreign(Arc::clone(arena.expect("not absorbed")), node)
            }
        }
    }

    /// Brings `value` into this builder as a merge source, absorbing its
    /// arena when `value` holds the last reference to it.
    fn take_in(&mut self, value: Value) -> MergeSource {
        let Value { arena, node } = value;
        let arena = match Arc::try_unwrap(arena) {
            Ok(arena) => arena,
            Err(shared) => return MergeSource::Foreign(shared, node),
        };
        if !self.arena.fits_absorb(&arena) {
            return MergeSource::Foreign(Arc::new(arena), node);
        }
        let first_foreign = self.arena.foreign_len();
        let absorbed = self.arena.absorb(arena);
        self.absorb_referenced(first_foreign);
        MergeSource::Local(node + absorbed.node_offset)
    }

    /// Absorbs the arenas referenced from foreign entries `from` onwards that
    /// nothing else references, then those their entries reference, and so
    /// on. An arena qualifies when every handle to it is one of those entries
    /// and the subtrees they name are disjoint, so that once its nodes are
    /// local each one is still reached from exactly one place. Entries into
    /// an absorbed arena become aliases of the local nodes.
    fn absorb_referenced(&mut self, mut from: usize) {
        let mut groups: Vec<(usize, ForeignId)> = Vec::new();
        let mut nodes: Vec<NodeId> = Vec::new();
        loop {
            let to = self.arena.foreign_len();
            if from == to {
                return;
            }
            groups.clear();
            for id in from as ForeignId..to as ForeignId {
                if let (Some(arena), _) = self.arena.foreign_target(id) {
                    groups.push((Arc::as_ptr(arena) as usize, id));
                }
            }
            from = to;
            groups.sort_unstable();
            for group in groups.chunk_by(|a, b| a.0 == b.0) {
                let (Some(arena), _) = self.arena.foreign_target(group[0].1) else {
                    unreachable!("grouped entries hold an arena");
                };
                if Arc::strong_count(arena) != group.len() || Arc::weak_count(arena) != 0 {
                    continue;
                }
                nodes.clear();
                nodes.extend(group.iter().map(|&(_, id)| self.arena.foreign_target(id).1));
                if !arena.subtrees_disjoint(&mut nodes) {
                    continue;
                }
                self.absorb_group(group);
            }
        }
    }

    /// Absorbs the arena every entry in `group` points into, which those
    /// entries alone hold, and turns the entries into aliases. Leaves them
    /// as they were if the arena does not fit.
    fn absorb_group(&mut self, group: &[(usize, ForeignId)]) {
        let mut handle = None;
        for &(_, id) in group {
            handle = self.arena.take_foreign(id);
        }
        let handle = handle.expect("the group is not empty");
        let restore = |builder: &mut Self, handle: Arc<Arena>| {
            for &(_, id) in group {
                builder.arena.restore_foreign(id, Arc::clone(&handle));
            }
        };
        let arena = match Arc::try_unwrap(handle) {
            Ok(arena) => arena,
            Err(handle) => return restore(self, handle),
        };
        if !self.arena.fits_absorb(&arena) {
            return restore(self, Arc::new(arena));
        }
        let absorbed = self.arena.absorb(arena);
        for &(_, id) in group {
            self.arena.alias_foreign(id, absorbed.node_offset);
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
                        self.push_entry(
                            cur,
                            Entry {
                                key,
                                child: Child::local(id),
                            },
                        );
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
    /// foreign node: its own structure is copied into the arena's slabs, its
    /// children become references into the source arena (path copying, one
    /// level at a time).
    fn localize_child(&mut self, child: Child) -> NodeId {
        if let Some(id) = self.arena.local_node(child) {
            return id;
        }
        let (src, node) = match self
            .arena
            .foreign_target(child.as_foreign().expect("child is foreign"))
        {
            (Some(arena), node) => (Arc::clone(arena), node),
            (None, _) => unreachable!("local_node resolves absorbed entries"),
        };
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
                let mut imported = std::mem::take(&mut self.import_children);
                imported.extend(src.children(slab).iter().map(|&c| {
                    let (owner, id) = src.resolve_owner(c);
                    self.arena.push_foreign(owner, id)
                }));
                let slab = self.arena.alloc_children(&imported);
                imported.clear();
                self.import_children = imported;
                Node::Array(slab)
            }
            Node::Object(slab) => {
                let mut imported = std::mem::take(&mut self.import_entries);
                imported.extend(src.entries(slab).iter().map(|entry| {
                    let (owner, id) = src.resolve_owner(entry.child);
                    let key = KeyRef::Owned(self.arena.alloc_text(&src.key_unescaped(entry.key)));
                    Entry {
                        key,
                        child: self.arena.push_foreign(owner, id),
                    }
                }));
                let slab = self.arena.alloc_entries(&imported);
                imported.clear();
                self.import_entries = imported;
                Node::Object(slab)
            }
        };
        self.arena.push_node(copied)
    }

    /// The live run of the local object `id`.
    fn object_slab(&self, id: NodeId) -> SlabRef {
        match self.arena.node(id) {
            Node::Object(slab) => slab,
            _ => unreachable!("caller checked the node is an object"),
        }
    }

    /// The live run of the local array `id`.
    fn array_slab(&self, id: NodeId) -> SlabRef {
        match self.arena.node(id) {
            Node::Array(slab) => slab,
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    /// Appends a member to the local object `id`.
    fn push_entry(&mut self, id: NodeId, entry: Entry) {
        self.push_entry_with_room(id, entry, 0);
    }

    /// Appends a member to the local object `id`. If the object is empty and
    /// `room` is non-zero, its run is allocated with exactly `room` slots,
    /// for a caller about to append that many.
    fn push_entry_with_room(&mut self, id: NodeId, entry: Entry, room: usize) {
        let slab = self.object_slab(id);
        let len = slab.len();
        let (extra, exact) = first_growth(len, room);
        let slab = self
            .arena
            .grow_entries(slab, extra, exact)
            .with_len(len + 1);
        self.arena.entries_mut(slab)[len] = entry;
        self.arena.set_node(id, Node::Object(slab));
    }

    /// Appends an element to the local array `id`.
    fn push_child(&mut self, id: NodeId, child: Child) {
        self.push_child_with_room(id, child, 0);
    }

    /// Appends an element to the local array `id`, allocating an empty
    /// array's run with exactly `room` slots as
    /// [`ValueBuilder::push_entry_with_room`] does.
    fn push_child_with_room(&mut self, id: NodeId, child: Child, room: usize) {
        let slab = self.array_slab(id);
        let len = slab.len();
        let (extra, exact) = first_growth(len, room);
        let slab = self
            .arena
            .grow_children(slab, extra, exact)
            .with_len(len + 1);
        self.arena.children_mut(slab)[len] = child;
        self.arena.set_node(id, Node::Array(slab));
    }

    /// Makes room in the local object `id` for `extra` more members, exactly,
    /// for a caller that knows how many it is about to append.
    fn reserve_entries(&mut self, id: NodeId, extra: usize) {
        let slab = self.arena.grow_entries(self.object_slab(id), extra, true);
        self.arena.set_node(id, Node::Object(slab));
    }

    /// Makes room in the local array `id` for `extra` more elements, exactly.
    fn reserve_children(&mut self, id: NodeId, extra: usize) {
        let slab = self.arena.grow_children(self.array_slab(id), extra, true);
        self.arena.set_node(id, Node::Array(slab));
    }

    /// Removes member `at` of the local object `id`, keeping the order of the
    /// rest. The freed slot becomes room for the object to grow into.
    fn remove_entry(&mut self, id: NodeId, at: usize) {
        let slab = self.object_slab(id);
        let entries = self.arena.entries_mut(slab);
        entries[at..].copy_within(1.., 0);
        entries[slab.len() - 1] = Entry::VACANT;
        self.arena
            .set_node(id, Node::Object(slab.with_len(slab.len() - 1)));
    }

    /// Removes element `at` of the local array `id`, shifting later elements
    /// left.
    fn remove_child(&mut self, id: NodeId, at: usize) {
        let slab = self.array_slab(id);
        let children = self.arena.children_mut(slab);
        children[at..].copy_within(1.., 0);
        children[slab.len() - 1] = Child::VACANT;
        self.arena
            .set_node(id, Node::Array(slab.with_len(slab.len() - 1)));
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
        // An empty container needs no frame, and is what a caller creating
        // structure ahead of its contents writes most.
        match &value {
            NewValue::Array(items) if items.is_empty() => {
                return Ok(Child::local(
                    self.arena.push_node(Node::Array(SlabRef::EMPTY)),
                ));
            }
            NewValue::Object(members) if members.is_empty() => {
                return Ok(Child::local(
                    self.arena.push_node(Node::Object(SlabRef::EMPTY)),
                ));
            }
            _ => {}
        }
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

    /// The members of a local object.
    fn object_members(&self, obj: NodeId) -> Option<&[Entry]> {
        match self.arena.node(obj) {
            Node::Object(slab) => Some(self.arena.entries(slab)),
            _ => None,
        }
    }

    fn object_child(&self, obj: NodeId, index: usize) -> Child {
        self.object_members(obj).expect("node is an object")[index].child
    }

    fn set_object_child(&mut self, obj: NodeId, index: usize, child: Child) {
        match self.arena.node(obj) {
            Node::Object(slab) => self.arena.entries_mut(slab)[index].child = child,
            _ => unreachable!("caller checked the node is an object"),
        }
    }

    fn array_len(&self, arr: NodeId) -> usize {
        match self.arena.node(arr) {
            Node::Array(slab) => slab.len(),
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    fn array_child(&self, arr: NodeId, index: usize) -> Child {
        match self.arena.node(arr) {
            Node::Array(slab) => self.arena.children(slab)[index],
            _ => unreachable!("caller checked the node is an array"),
        }
    }

    fn set_array_child(&mut self, arr: NodeId, index: usize, child: Child) {
        match self.arena.node(arr) {
            Node::Array(slab) => self.arena.children_mut(slab)[index] = child,
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

/// Decides what [`ValueBuilder::merge_from`] does where both documents hold
/// a value and it does not descend: they are not both objects or both
/// arrays.
pub trait MergePolicy {
    /// Whether `incoming` replaces `existing`. `key` is the member's key, or
    /// `None` for an array element or the root.
    fn replace(
        &mut self,
        key: Option<&str>,
        existing: crate::BuilderRef<'_>,
        incoming: crate::BuilderRef<'_>,
    ) -> bool;
}

/// One container being merged: members of `source` from `next` on are still
/// to merge into `target`.
struct MergeFrame {
    target: NodeId,
    source: MergeSource,
    next: usize,
    len: usize,
    /// Members the merge will append to `target` when it starts out empty,
    /// so the first append allocates exactly this much; 0 otherwise.
    room: usize,
}

/// How far a container with `len` members grows to take one more: by the
/// `room` its caller asked for when it is empty (an empty run owns no room,
/// so a reservation could not be kept in it), else by one.
fn first_growth(len: usize, room: usize) -> (usize, bool) {
    if len == 0 && room > 0 {
        (room, true)
    } else {
        (1, false)
    }
}

/// A slot of the container a merge writes into.
#[derive(Clone, Copy)]
enum Slot {
    Member(usize),
    Element(usize),
}

/// The key of the member a merge slot holds, for the policy to see; read
/// from the arena only when the policy is asked, so the common case copies
/// nothing.
enum MergeKey {
    None,
    Local(KeyRef),
    Text(String),
}

/// A value a merge reads from.
enum MergeSource {
    /// A node of the builder's own arena, absorbed or not.
    Local(NodeId),
    /// A node of another arena.
    Foreign(Arc<Arena>, NodeId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuilderRef, JsonKind, json};

    /// Execution-merge replacement rules: an incoming `null` and a container
    /// meeting the other container kind keep what is there.
    struct Execution;

    impl MergePolicy for Execution {
        fn replace(
            &mut self,
            _: Option<&str>,
            existing: BuilderRef<'_>,
            incoming: BuilderRef<'_>,
        ) -> bool {
            !matches!(
                (existing.kind(), incoming.kind()),
                (_, JsonKind::Null)
                    | (JsonKind::Object, JsonKind::Array)
                    | (JsonKind::Array, JsonKind::Object)
            )
        }
    }

    fn parse(json: &str) -> Value {
        Value::parse(json.as_bytes().to_vec()).unwrap()
    }

    /// Places handles into `response` under `p0`, `p1`, ... the way a fetch
    /// places entities.
    fn place(response: &Value, paths: &[&[PathSegment<'_>]]) -> Value {
        let mut builder = ValueBuilder::new();
        for (i, path) in paths.iter().enumerate() {
            let handle = response.get_path(path).unwrap();
            builder.set(format!("p{i}").as_str(), handle).unwrap();
        }
        builder.seal()
    }

    fn merge(base: Value, other: Value) -> Value {
        let mut builder = base.edit();
        builder.merge_from(other, &mut Execution);
        builder.seal()
    }

    #[test]
    fn room_freed_by_a_removal_is_grown_into_and_emptied_containers_share_none() {
        let mut builder = json!({"a": [1, 2], "b": [], "c": {"x": 1}}).edit();
        builder.get_mut("a").unwrap().remove(0);
        builder.get_mut("a").unwrap().push(3).unwrap();
        builder.get_mut("c").unwrap().remove("x");
        // `a` emptied and `b` never had members: both are the empty run,
        // and growing either must not land in the room the other has.
        builder.get_mut("a").unwrap().remove(0);
        builder.get_mut("a").unwrap().remove(0);
        builder.get_mut("a").unwrap().push(4).unwrap();
        builder.get_mut("b").unwrap().push(5).unwrap();
        builder.get_mut("c").unwrap().set("y", 6).unwrap();
        assert_eq!(builder.seal(), json!({"a": [4], "b": [5], "c": {"y": 6}}));
    }

    #[test]
    fn a_merge_into_an_empty_container_allocates_its_run_once_and_exactly() {
        let mut builder = json!({"t": {}, "u": []}).edit();
        let before = builder.arena.slots();
        // Held twice, so the merge adopts it by reference and appends nothing
        // of its storage to this arena.
        let source = json!({"t": {"a": 1, "b": 2, "c": 3}, "u": [1, 2, 3]});
        let _held = source.clone();
        builder.merge_from(source, &mut Execution);
        let after = builder.arena.slots();
        assert_eq!((after.0 - before.0, after.1 - before.1), (3, 3));
        assert_eq!(
            builder.seal(),
            json!({"t": {"a": 1, "b": 2, "c": 3}, "u": [1, 2, 3]})
        );
    }

    #[test]
    fn a_uniquely_held_source_and_the_response_it_places_are_absorbed() {
        let response = parse(r#"{"items":[{"id":1},{"id":2}]}"#);
        let placed = place(
            &response,
            &[
                &[PathSegment::Key("items"), PathSegment::Index(0)],
                &[PathSegment::Key("items"), PathSegment::Index(1)],
            ],
        );
        drop(response);

        let merged = merge(json!({}), placed);
        assert_eq!(merged.arena.live_foreign(), 0);

        // A later merge into a placed subtree edits it where it is.
        let merged = merge(merged, parse(r#"{"p1":{"name":"two"}}"#));
        assert_eq!(merged.arena.live_foreign(), 0);
        assert_eq!(merged, json!({"p0":{"id":1},"p1":{"id":2,"name":"two"}}));
    }

    #[test]
    fn overlapping_placements_keep_the_response_by_reference() {
        let items: &[PathSegment<'_>] = &[PathSegment::Key("items")];
        let item: &[PathSegment<'_>] = &[PathSegment::Key("items"), PathSegment::Index(0)];
        // The same subtree twice, and a subtree inside another one. Absorbing
        // the response would leave one node reached from two places, so an
        // edit through `p1` would show through `p0`.
        for (paths, p0) in [
            ([item, item], json!({"id":1})),
            ([items, item], json!([{"id":1}])),
        ] {
            let response = parse(r#"{"items":[{"id":1}]}"#);
            let placed = place(&response, &paths);
            drop(response);
            let merged = merge(json!({}), placed);
            assert_eq!(merged.arena.live_foreign(), 2);
            let merged = merge(merged, parse(r#"{"p1":{"name":"one"}}"#));
            assert_eq!(merged.get("p0").unwrap(), p0);
            assert_eq!(merged.get("p1").unwrap(), json!({"id":1,"name":"one"}));
        }
    }

    #[test]
    fn a_shared_source_is_adopted_by_reference() {
        let source = parse(r#"{"a":{"b":1}}"#);
        let merged = merge(json!({}), source.clone());
        assert_eq!(merged.arena.live_foreign(), 1, "only the adopted member");
        let merged = merge(merged, parse(r#"{"a":{"c":2}}"#));
        assert_eq!(merged, json!({"a":{"b":1,"c":2}}));
        assert_eq!(source, json!({"a":{"b":1}}));
    }

    #[test]
    fn absorbed_inputs_serialize_and_stream_from_their_own_buffers() {
        let long = "x".repeat(300);
        let first = parse(&format!(r#"{{"a":"{long}","esc":"q\"uote"}}"#));
        let second = parse(&format!(r#"{{"b":"{long}","n":12.50}}"#));
        let merged = merge(merge(json!({"base":true}), first), second);
        let expected =
            format!(r#"{{"base":true,"a":"{long}","esc":"q\"uote","b":"{long}","n":12.50}}"#);
        assert_eq!(merged.to_string(), expected);
        let streamed: Vec<u8> = merged
            .clone()
            .into_chunks(64)
            .flat_map(|chunk| chunk.to_vec())
            .collect();
        assert_eq!(streamed, expected.as_bytes());
        assert_eq!(merged.get("esc").unwrap().as_str().unwrap(), "q\"uote");
    }
}
