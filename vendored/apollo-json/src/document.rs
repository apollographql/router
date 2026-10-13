//! Owned and borrowed handles for reading and sharing parsed JSON.

use std::borrow::Cow;
use std::sync::Arc;

use bytes::Bytes;

use crate::arena::{Arena, resolve, resolve_shared};
use crate::builder::PathSegment;
use crate::error::JsonError;
use crate::node::{Child, Node, NodeId};
use crate::options::ParseOptions;
use crate::stream::Chunks;

/// Chunk size used by `write_to`.
const WRITE_CHUNK_SIZE: usize = 16 * 1024;

/// An immutable, shareable JSON value: the owned handle to a parsed
/// document or to any subtree of one.
///
/// `Value` is to [`ValueRef`] what `String` is to `&str`: the owned form. It
/// is `Send + Sync + 'static`, clones with one atomic increment, and can
/// outlive the handle it was read out of. Mutation goes through
/// [`Value::edit`], which never disturbs other handles.
///
/// # Pinning
/// A `Value` shares its arena rather than copying anything, so it keeps
/// that arena — and every arena it references — fully resident for as long
/// as the `Value` lives. Retaining one small subtree handle pins everything
/// parsed alongside it. Before storing a value beyond the request that
/// produced it, sever the pin with [`Value::into_self_contained`] — free
/// when there is nothing external to release — or [`Value::compact`].
#[derive(Clone)]
pub struct Value {
    pub(crate) arena: Arc<Arena>,
    pub(crate) node: NodeId,
}

impl Value {
    /// Finalizes a freshly built arena into an owned handle, recording
    /// `root` as the arena's canonical root so subtree-rooted views of it
    /// (serde captures, adopted fragments) can be told apart from the value
    /// the arena was built to represent.
    pub(crate) fn rooted(mut arena: Arena, root: NodeId) -> Self {
        arena.set_root(root);
        Value {
            arena: Arc::new(arena),
            node: root,
        }
    }

    /// Parses JSON with default [`ParseOptions`].
    ///
    /// Takes anything convertible to [`Bytes`](bytes::Bytes): a `Vec<u8>` is
    /// taken over in place, and a caller already holding `Bytes` — an HTTP
    /// body, say — hands over a refcount rather than copying the input.
    ///
    /// # Errors
    /// Returns [`JsonError`] when the input is not valid JSON or exceeds the
    /// configured depth or arena-size limits.
    pub fn parse(input: impl Into<bytes::Bytes>) -> Result<Self, JsonError> {
        Self::parse_with_options(input, &ParseOptions::default())
    }

    /// Parses JSON with explicit limits.
    ///
    /// # Errors
    /// Returns [`JsonError`] when the input is not valid JSON or exceeds the
    /// configured depth or arena-size limits.
    pub fn parse_with_options(
        input: impl Into<bytes::Bytes>,
        options: &ParseOptions,
    ) -> Result<Self, JsonError> {
        let parsed = crate::parse::parse(input.into(), options)?;
        Ok(Value::rooted(parsed.arena, parsed.root))
    }

    /// Parses JSON, reusing storage recycled into `buffers`; see
    /// [`ParseBuffers`](crate::ParseBuffers).
    ///
    /// # Errors
    /// Returns [`JsonError`] when the input is not valid JSON or exceeds the
    /// configured depth or arena-size limits.
    pub fn parse_with_buffers(
        input: impl Into<bytes::Bytes>,
        options: &ParseOptions,
        buffers: &mut crate::ParseBuffers,
    ) -> Result<Self, JsonError> {
        let parsed = crate::parse::parse_with(input.into(), options, buffers)?;
        Ok(Value::rooted(parsed.arena, parsed.root))
    }

    /// Reclaims the value's storage into `buffers` for the next
    /// [`Value::parse_with_buffers`] call, returning whether anything was
    /// reclaimed. Recycling succeeds only when this handle is the last
    /// reference to its arena — clones, subtree handles, and compositions
    /// referencing it all keep it un-recyclable, so a value whose arena is
    /// shared is simply dropped. Everything the value pinned is released
    /// either way.
    pub fn recycle(self, buffers: &mut crate::ParseBuffers) -> bool {
        match Arc::try_unwrap(self.arena) {
            Ok(mut arena) => {
                arena.reset();
                buffers.arena = Some(arena);
                true
            }
            Err(_) => false,
        }
    }

    /// Opens the value for mutation; sugar for
    /// [`ValueBuilder::from_value`](crate::ValueBuilder::from_value).
    ///
    /// When this handle is the last reference to its arena, the builder
    /// takes the arena over and mutates in place. Otherwise — including any
    /// handle to a subtree of an arena that other handles still share —
    /// mutations copy the touched path into a fresh arena and every other
    /// subtree is shared by reference, so the other handles never observe
    /// changes.
    pub fn edit(self) -> crate::ValueBuilder {
        crate::ValueBuilder::from_value(self)
    }

    /// Whether the value's memory footprint is bounded by its own content:
    /// `true` when it is the root its arena was built for and pins no other
    /// arena. This is a runtime property of the handle, not of its type — a
    /// subtree captured out of a larger value (through [`Value::get`], serde
    /// capture, or composition) reports `false`, because it keeps the whole
    /// source arena resident until severed.
    pub fn is_self_contained(&self) -> bool {
        !self.arena.has_foreign() && self.node == self.arena.root()
    }

    /// The retention boundary: returns the value unchanged — and
    /// allocation-free — when it is already self-contained, and otherwise
    /// deep-copies it into a fresh minimal arena that pins nothing beyond
    /// its own content. Call this (or accept only self-contained values)
    /// before storing a value beyond the request that produced it: caches,
    /// subscriptions, deduplication state.
    pub fn into_self_contained(self) -> Value {
        if self.is_self_contained() {
            self
        } else {
            self.compact()
        }
    }

    /// Unconditionally deep-copies the value into a fresh, minimal arena,
    /// severing the pin on the source arena. Prefer
    /// [`Value::into_self_contained`], which is free when there is nothing
    /// external to release.
    pub fn compact(&self) -> Value {
        crate::detach::detach(self.value())
    }

    /// Converts to the legacy `serde_json_bytes::Value` in a single walk.
    pub fn to_legacy(&self) -> serde_json_bytes::Value {
        crate::convert::to_legacy(self.value())
    }

    /// Builds a value from the legacy `serde_json_bytes::Value` in a single
    /// walk.
    pub fn from_legacy(value: &serde_json_bytes::Value) -> Value {
        crate::convert::from_legacy(value)
    }

    /// A borrowed view of the value, for cheap traversal.
    pub fn value(&self) -> ValueRef<'_> {
        ValueRef {
            arena: &self.arena,
            node: self.node,
        }
    }

    /// The JSON type of the value. Shorthand for `self.value().kind()`.
    pub fn kind(&self) -> JsonKind {
        self.value().kind()
    }

    /// Whether the value is `null`. Shorthand for `self.value().is_null()`.
    pub fn is_null(&self) -> bool {
        self.value().is_null()
    }

    /// Whether the value is an object.
    pub fn is_object(&self) -> bool {
        self.value().is_object()
    }

    /// Whether the value is an array.
    pub fn is_array(&self) -> bool {
        self.value().is_array()
    }

    /// Whether the value is a string.
    pub fn is_string(&self) -> bool {
        self.value().is_string()
    }

    /// Whether the value is a number.
    pub fn is_number(&self) -> bool {
        self.value().is_number()
    }

    /// Whether the value is a boolean.
    pub fn is_boolean(&self) -> bool {
        self.value().is_boolean()
    }

    /// The boolean value. Shorthand for `self.value().as_bool()`.
    pub fn as_bool(&self) -> Option<bool> {
        self.value().as_bool()
    }

    /// The string value. Shorthand for `self.value().as_str()`.
    pub fn as_str(&self) -> Option<Cow<'_, str>> {
        self.value().as_str()
    }

    /// The string value, copied out of the document. Unlike
    /// [`Value::as_str`], the result borrows nothing, so it reads out of a
    /// temporary handle:
    ///
    /// ```
    /// use apollo_json::Value;
    ///
    /// let doc = Value::parse(br#"{"name":"ada"}"#.to_vec())?;
    /// let name = doc.get("name").and_then(|v| v.as_string());
    /// assert_eq!(name.as_deref(), Some("ada"));
    /// # Ok::<(), apollo_json::JsonError>(())
    /// ```
    pub fn as_string(&self) -> Option<String> {
        self.as_str().map(Cow::into_owned)
    }

    /// The number's literal text. Shorthand for `self.value().raw_number()`.
    pub fn raw_number(&self) -> Option<&str> {
        self.value().raw_number()
    }

    /// The number as `f64`. Shorthand for `self.value().as_f64()`.
    pub fn as_f64(&self) -> Option<f64> {
        self.value().as_f64()
    }

    /// The number as `i64`. Shorthand for `self.value().as_i64()`.
    pub fn as_i64(&self) -> Option<i64> {
        self.value().as_i64()
    }

    /// The number as `u64`. Shorthand for `self.value().as_u64()`.
    pub fn as_u64(&self) -> Option<u64> {
        self.value().as_u64()
    }

    /// Number of elements or members. Shorthand for `self.value().len()`.
    pub fn len(&self) -> Option<usize> {
        self.value().len()
    }

    /// Whether the container is empty. Shorthand for `self.value().is_empty()`.
    pub fn is_empty(&self) -> Option<bool> {
        self.value().is_empty()
    }

    /// Iterates array elements as owned handles. Empty for non-arrays.
    pub fn array_iter(&self) -> impl Iterator<Item = Value> + '_ {
        let len = self.value().len().unwrap_or(0);
        (0..len).filter_map(move |i| self.index(i))
    }

    /// Iterates object members as `(key, value)` pairs, in insertion order,
    /// with owned value handles. Keys borrow the document like
    /// [`ValueRef::object_iter`]'s do. Empty for non-objects.
    pub fn object_iter(&self) -> impl Iterator<Item = (Cow<'_, str>, Value)> + '_ {
        let entries = match self.arena.node(self.node) {
            Node::Object(slab) => self.arena.entries(slab),
            _ => &[],
        };
        entries.iter().map(|entry| {
            let key = self.arena.key_unescaped(entry.key);
            let (arena, node) = self.arena.resolve_owner(entry.child);
            (key, Value { arena, node })
        })
    }

    /// The array's elements as owned handles, or `None` for any other shape.
    /// [`Value::array_iter`] walks the same elements without the `Vec`.
    pub fn as_array(&self) -> Option<Vec<Value>> {
        self.is_array().then(|| self.array_iter().collect())
    }

    /// The object's members as owned `(key, value)` pairs in insertion order,
    /// or `None` for any other shape. [`Value::object_iter`] walks the same
    /// members without the `Vec`, and [`Value::get`] reads one by key.
    pub fn as_object(&self) -> Option<Vec<(String, Value)>> {
        self.is_object().then(|| {
            self.object_iter()
                .map(|(key, value)| (key.into_owned(), value))
                .collect()
        })
    }

    /// Looks up an object member, returning an owned handle.
    pub fn get(&self, key: &str) -> Option<Value> {
        let Node::Object(slab) = self.arena.node(self.node) else {
            return None;
        };
        let child = self
            .arena
            .entries(slab)
            .iter()
            .find(|entry| self.arena.key_matches_str(entry.key, key))
            .map(|entry| entry.child)?;
        let (arena, node) = self.arena.resolve_owner(child);
        Some(Value { arena, node })
    }

    /// Whether an object holds `key`; `false` for any other shape. Shorthand
    /// for `self.value().contains_key(key)`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.value().contains_key(key)
    }

    /// Resolves a path of keys and indexes, returning an owned handle to the
    /// value it names; `None` when any segment does not resolve. The walk
    /// borrows, so a deep path costs one handle at the end rather than one
    /// per step:
    ///
    /// ```
    /// use apollo_json::Value;
    ///
    /// let doc = Value::parse(br#"{"items":[{"id":7}]}"#.to_vec())?;
    /// let id = doc.get_path(&["items".into(), 0.into(), "id".into()]);
    /// assert_eq!(id.and_then(|v| v.as_i64()), Some(7));
    /// # Ok::<(), apollo_json::JsonError>(())
    /// ```
    pub fn get_path(&self, path: &[PathSegment<'_>]) -> Option<Value> {
        let mut arena = &self.arena;
        let mut node = self.node;
        for segment in path {
            let child = child_at(arena, node, segment)?;
            (arena, node) = resolve_shared(arena, child);
        }
        Some(Value {
            arena: Arc::clone(arena),
            node,
        })
    }

    /// Looks up an array element, returning an owned handle.
    pub fn index(&self, index: usize) -> Option<Value> {
        let Node::Array(slab) = self.arena.node(self.node) else {
            return None;
        };
        let child = *self.arena.children(slab).get(index)?;
        let (arena, node) = self.arena.resolve_owner(child);
        Some(Value { arena, node })
    }

    /// Serializes the value to JSON bytes. Untouched input spans are
    /// emitted verbatim; shared subtrees are expanded at every reference.
    pub fn to_vec(&self) -> Vec<u8> {
        // A parsed value serializes to roughly its input size; subtree
        // handles and values assembled from other arenas grow the buffer as
        // needed.
        let hint = if self.node == self.arena.root() {
            self.arena.input().len().max(64)
        } else {
            256
        };
        crate::serialize::serialize(self.value(), hint)
    }

    /// Serializes the value to a JSON string.
    ///
    /// The output is UTF-8 by construction — string spans were validated at
    /// parse, owned text is written from `str`, and everything the
    /// serializer adds is ASCII — so this only checks the bytes, it never
    /// copies or re-encodes them.
    // `Display` would route the serialized output through the formatter and
    // copy it a second time; serialization output warrants the direct path.
    #[expect(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        String::from_utf8(self.to_vec()).expect("serializer output is UTF-8 by construction")
    }

    /// Serializes the value to a shareable byte buffer.
    pub fn to_bytes(&self) -> Bytes {
        Bytes::from(self.to_vec())
    }

    /// Writes the serialized value to `writer` in chunks, without
    /// materializing one contiguous buffer.
    ///
    /// # Errors
    /// Returns any error from the writer.
    pub fn write_to<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        for chunk in self.clone().into_chunks(WRITE_CHUNK_SIZE) {
            writer.write_all(&chunk)?;
        }
        Ok(())
    }

    /// Streams the serialized value as [`Bytes`] chunks of roughly
    /// `target_chunk_size` bytes, suitable as an HTTP response body. Large
    /// untouched input spans are yielded as zero-copy slices sharing the
    /// arena's buffer, so a held chunk can keep that buffer alive.
    pub fn into_chunks(self, target_chunk_size: usize) -> Chunks {
        Chunks::new(self.arena, self.node, target_chunk_size)
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Value({})", self.to_string())
    }
}

impl Default for Value {
    /// A value holding `null`; [`Value::null`] spelled out.
    fn default() -> Self {
        Value::null()
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}

impl Eq for Value {}

impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value().hash(state)
    }
}

impl PartialEq for ValueRef<'_> {
    /// Structural equality: same JSON type, and for objects, the same
    /// members regardless of insertion order (matching `serde_json`'s map
    /// equality) — order-sensitive comparisons belong to the caller.
    fn eq(&self, other: &Self) -> bool {
        match (self.kind(), other.kind()) {
            (JsonKind::Null, JsonKind::Null) => true,
            (JsonKind::Bool, JsonKind::Bool) => self.as_bool() == other.as_bool(),
            (JsonKind::Number, JsonKind::Number) => self.as_f64() == other.as_f64(),
            (JsonKind::String, JsonKind::String) => self.as_str() == other.as_str(),
            (JsonKind::Array, JsonKind::Array) => {
                self.len() == other.len() && self.array_iter().eq(other.array_iter())
            }
            (JsonKind::Object, JsonKind::Object) => {
                self.len() == other.len()
                    && self
                        .object_iter()
                        .all(|(key, value)| other.get(&key) == Some(value))
            }
            _ => false,
        }
    }
}

impl Eq for ValueRef<'_> {}

/// Comparisons against Rust scalars read through the lazy accessors
/// (`as_bool`, `as_i64`, `as_u64`, `as_f64`), so a number equals an integer
/// only when the accessor of that width reads it as one — `1e2` equals
/// `100.0f64` but not `100i64` — and a value of any other JSON type compares
/// unequal rather than erroring.
macro_rules! scalar_partial_eq {
    ($(($ty:ty, $accessor:ident)),* $(,)?) => {
        $(
            impl PartialEq<$ty> for ValueRef<'_> {
                fn eq(&self, other: &$ty) -> bool {
                    self.$accessor() == Some(*other)
                }
            }

            impl PartialEq<ValueRef<'_>> for $ty {
                fn eq(&self, other: &ValueRef<'_>) -> bool {
                    other == self
                }
            }

            impl PartialEq<$ty> for Value {
                fn eq(&self, other: &$ty) -> bool {
                    self.value() == *other
                }
            }

            impl PartialEq<Value> for $ty {
                fn eq(&self, other: &Value) -> bool {
                    other == self
                }
            }
        )*
    };
}

scalar_partial_eq!((bool, as_bool), (i64, as_i64), (u64, as_u64), (f64, as_f64));

/// String comparisons match [`ValueRef::as_str`]: only strings compare equal
/// to a `str`, by their unescaped text.
impl PartialEq<str> for ValueRef<'_> {
    fn eq(&self, other: &str) -> bool {
        self.as_str().is_some_and(|s| s == other)
    }
}

impl PartialEq<ValueRef<'_>> for str {
    fn eq(&self, other: &ValueRef<'_>) -> bool {
        other == self
    }
}

impl PartialEq<&str> for ValueRef<'_> {
    fn eq(&self, other: &&str) -> bool {
        *self == **other
    }
}

impl PartialEq<ValueRef<'_>> for &str {
    fn eq(&self, other: &ValueRef<'_>) -> bool {
        other == self
    }
}

impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.value() == *other
    }
}

impl PartialEq<Value> for str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.value() == **other
    }
}

impl PartialEq<Value> for &str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl std::hash::Hash for ValueRef<'_> {
    /// Consistent with [`PartialEq`]: objects hash their members with an
    /// order-independent fold so two objects differing only in key order
    /// still collide.
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Every value is framed with its kind, and containers with their
        // length, so distinct nestings of the same leaves (`[[1],[2]]` vs
        // `[[1,2]]`, `1` vs `[1]`) feed distinct byte streams to the hasher.
        let kind = self.kind();
        state.write_u8(kind as u8);
        match kind {
            JsonKind::Null => {}
            JsonKind::Bool => self.as_bool().hash(state),
            JsonKind::Number => {
                // Equality compares numbers as `f64`, where `0.0 == -0.0`;
                // fold the zero signs together so equal values hash equal.
                self.as_f64()
                    .map(|f| if f == 0.0 { 0.0f64 } else { f }.to_bits())
                    .hash(state)
            }
            JsonKind::String => self.as_str().hash(state),
            JsonKind::Array => {
                self.len().hash(state);
                for item in self.array_iter() {
                    item.hash(state);
                }
            }
            JsonKind::Object => {
                self.len().hash(state);
                use std::hash::{BuildHasher as _, Hasher as _};
                // Members must be hashed independently of `state` (whose
                // value is order-sensitive), but with a keyed hasher: an
                // unkeyed fold would let precomputed member collisions defeat
                // whatever keyed hasher the containing map uses. One
                // process-random key keeps equal objects hashing equal.
                static MEMBER_KEY: std::sync::LazyLock<std::hash::RandomState> =
                    std::sync::LazyLock::new(std::hash::RandomState::new);
                let mut combined: u64 = 0;
                for (key, value) in self.object_iter() {
                    let mut member_hasher = MEMBER_KEY.build_hasher();
                    key.hash(&mut member_hasher);
                    value.hash(&mut member_hasher);
                    combined = combined.wrapping_add(member_hasher.finish());
                }
                state.write_u64(combined);
            }
        }
    }
}

/// The JSON type of a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

/// A borrowed, `Copy` view of a value, for allocation-free traversal.
#[derive(Clone, Copy)]
pub struct ValueRef<'a> {
    pub(crate) arena: &'a Arena,
    pub(crate) node: NodeId,
}

impl<'a> ValueRef<'a> {
    pub(crate) fn node(&self) -> Node {
        self.arena.node(self.node)
    }

    pub(crate) fn deref_child(&self, child: crate::node::Child) -> ValueRef<'a> {
        let (arena, node) = resolve(self.arena, child);
        ValueRef { arena, node }
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

    /// Whether the value is an object.
    pub fn is_object(&self) -> bool {
        self.kind() == JsonKind::Object
    }

    /// Whether the value is an array.
    pub fn is_array(&self) -> bool {
        self.kind() == JsonKind::Array
    }

    /// Whether the value is a string.
    pub fn is_string(&self) -> bool {
        self.kind() == JsonKind::String
    }

    /// Whether the value is a number.
    pub fn is_number(&self) -> bool {
        self.kind() == JsonKind::Number
    }

    /// Whether the value is a boolean.
    pub fn is_boolean(&self) -> bool {
        self.kind() == JsonKind::Bool
    }

    /// The boolean value.
    pub fn as_bool(&self) -> Option<bool> {
        match self.node() {
            Node::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// The string value, unescaped lazily: escape-free strings borrow the
    /// arena input.
    pub fn as_str(&self) -> Option<Cow<'a, str>> {
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

    /// The number's literal text, exactly as it appeared in the input.
    pub fn raw_number(&self) -> Option<&'a str> {
        match self.node() {
            Node::Number(span) => Some(self.arena.input_utf8(span).as_str()),
            Node::OwnedNumber(text) => Some(self.arena.text_str(text)),
            _ => None,
        }
    }

    /// The number as `f64`, parsed lazily from its literal text.
    pub fn as_f64(&self) -> Option<f64> {
        self.raw_number()?.parse().ok()
    }

    /// The number as `i64`, when its literal is an integer in range.
    pub fn as_i64(&self) -> Option<i64> {
        let raw = self.raw_number()?;
        if raw.bytes().any(|b| matches!(b, b'.' | b'e' | b'E')) {
            return None;
        }
        raw.parse().ok()
    }

    /// The number as `u64`, when its literal is a non-negative integer in
    /// range.
    pub fn as_u64(&self) -> Option<u64> {
        let raw = self.raw_number()?;
        if raw.bytes().any(|b| matches!(b, b'.' | b'e' | b'E' | b'-')) {
            return None;
        }
        raw.parse().ok()
    }

    /// Number of elements (arrays) or members (objects).
    pub fn len(&self) -> Option<usize> {
        match self.node() {
            Node::Array(slab) => Some(slab.len()),
            Node::Object(slab) => Some(slab.len()),
            _ => None,
        }
    }

    /// Whether the container is empty; `None` for scalars.
    pub fn is_empty(&self) -> Option<bool> {
        Some(self.len()? == 0)
    }

    /// Looks up an object member by key.
    pub fn get(&self, key: &str) -> Option<ValueRef<'a>> {
        let Node::Object(slab) = self.node() else {
            return None;
        };
        self.arena
            .entries(slab)
            .iter()
            .find(|entry| self.arena.key_matches_str(entry.key, key))
            .map(|entry| self.deref_child(entry.child))
    }

    /// Whether an object holds `key`; `false` for any other shape.
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Resolves a path of keys and indexes; `None` when any segment does not
    /// resolve. See [`Value::get_path`] for the owned form.
    pub fn get_path(&self, path: &[PathSegment<'_>]) -> Option<ValueRef<'a>> {
        let mut cur = *self;
        for segment in path {
            let child = child_at(cur.arena, cur.node, segment)?;
            cur = cur.deref_child(child);
        }
        Some(cur)
    }

    /// Looks up an array element by index.
    pub fn index(&self, index: usize) -> Option<ValueRef<'a>> {
        let Node::Array(slab) = self.node() else {
            return None;
        };
        self.arena
            .children(slab)
            .get(index)
            .map(|&child| self.deref_child(child))
    }

    /// The object member at `index`, in insertion order.
    pub fn member_at(&self, index: usize) -> Option<(Cow<'a, str>, ValueRef<'a>)> {
        let Node::Object(slab) = self.node() else {
            return None;
        };
        let entry = self.arena.entries(slab).get(index)?;
        Some((
            self.arena.key_unescaped(entry.key),
            self.deref_child(entry.child),
        ))
    }

    /// Iterates array elements. Empty for non-arrays.
    pub fn array_iter(&self) -> impl Iterator<Item = ValueRef<'a>> + use<'a> {
        let items: &'a [crate::node::Child] = match self.node() {
            Node::Array(slab) => self.arena.children(slab),
            _ => &[],
        };
        let this = *self;
        items.iter().map(move |&child| this.deref_child(child))
    }

    /// Iterates object members as (key, value). Empty for non-objects.
    pub fn object_iter(&self) -> impl Iterator<Item = (Cow<'a, str>, ValueRef<'a>)> + use<'a> {
        let entries: &'a [crate::node::Entry] = match self.node() {
            Node::Object(slab) => self.arena.entries(slab),
            _ => &[],
        };
        let this = *self;
        entries.iter().map(move |entry| {
            (
                this.arena.key_unescaped(entry.key),
                this.deref_child(entry.child),
            )
        })
    }

    /// Serializes the subtree to JSON bytes.
    pub fn to_vec(&self) -> Vec<u8> {
        crate::serialize::serialize(*self, 256)
    }

    /// Serializes the subtree to a JSON string; UTF-8 by construction, so
    /// this only checks the bytes.
    #[expect(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        String::from_utf8(self.to_vec()).expect("serializer output is UTF-8 by construction")
    }

    /// Serializes the subtree to a shareable byte buffer.
    pub fn to_bytes(&self) -> Bytes {
        Bytes::from(self.to_vec())
    }
}

/// Resolves one path segment against a node, returning the child slot so the
/// caller decides between a borrowed and an owned continuation.
fn child_at(arena: &Arena, node: NodeId, segment: &PathSegment<'_>) -> Option<Child> {
    match *segment {
        PathSegment::Key(key) => {
            let Node::Object(slab) = arena.node(node) else {
                return None;
            };
            arena
                .entries(slab)
                .iter()
                .find(|entry| arena.key_matches_str(entry.key, key))
                .map(|entry| entry.child)
        }
        PathSegment::Index(index) => {
            let Node::Array(slab) = arena.node(node) else {
                return None;
            };
            arena.children(slab).get(index).copied()
        }
    }
}

impl std::fmt::Debug for ValueRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ValueRef({})", self.to_string())
    }
}
