//! Typed serde deserialization over the document model.

mod access;
mod capture;
mod stream;

use std::borrow::Cow;
use std::sync::Arc;

use serde::de::{self, Expected, Unexpected, Visitor};

use crate::arena::Arena;
use crate::document::{Value, ValueRef};
use crate::error::JsonError;
use crate::node::{Node, NodeId};
use crate::options::DEFAULT_MAX_DEPTH;
use crate::slab::SlabRef;

/// Deserializes a `T` from a parsed [`Value`].
///
/// Escape-free strings deserialize zero-copy: `&str` fields borrow the
/// value's arena directly.
///
/// Numbers deserialize at their natural width — `i64` for negative integer
/// literals, `u64` for non-negative ones, `f64` otherwise — and integer
/// literals beyond 64 bits fall back to `f64`, matching `serde_json`. So
/// does `-0`, which reads as the float `-0.0` to keep its sign.
/// Requesting `i128`/`u128` parses the full literal instead; there is no
/// arbitrary-precision representation. Float literals whose value overflows
/// to infinity are errors.
///
/// JSON object keys are strings; map keys of integer, float, and bool type
/// coerce from the string form with `serde_json`'s rules (the key must spell
/// a complete JSON number literal, or exactly `true`/`false`).
///
/// Duplicate object keys never reach the deserializer: parsing collapses
/// them to first position, last value, so a document reads `{"a":1,"a":2}`
/// as `{"a":2}`. [`from_slice`] and [`from_str`] deserialize from the byte
/// stream instead, see both entries, and reject duplicate struct fields
/// exactly as `serde_json` does.
///
/// Values nested deeper than the default parse depth limit (128 levels,
/// reachable only by composing values with
/// [`ValueBuilder`](crate::ValueBuilder)) fail with
/// [`JsonError::DepthLimitExceeded`] instead of recursing unboundedly.
///
/// # Errors
/// Returns [`JsonError`] when the value's shape does not match `T`.
pub fn from_value<'de, T>(value: &'de Value) -> Result<T, JsonError>
where
    T: de::Deserialize<'de>,
{
    T::deserialize(ValueDeserializer::new(&value.arena, value.node))
}

/// Deserializes a `T` from JSON bytes in a single pass, without building a
/// document; see [`from_value`] for the shared data-model semantics
/// (numbers, map keys, borrowing).
///
/// Because the stream sees every object entry, duplicate struct fields are
/// rejected exactly as `serde_json` rejects them — unlike [`from_value`],
/// where parsing has already collapsed duplicates to first position, last
/// value. Nesting beyond the default parse depth limit (128 levels) fails
/// with [`JsonError::DepthLimitExceeded`], which also bounds the stack the
/// deserializer can use. The limit is the parser's, not `serde_json`'s, so
/// acceptance differs at two margins: exactly 128 levels deserialize here
/// where `serde_json`'s recursion limit stops at 127, and the budget covers
/// ignored fields, whose content `serde_json` skips without any depth bound.
///
/// A field typed [`Value`] captures its subtree by
/// delimiting it in the stream and parsing just those bytes into a dedicated
/// arena: the capture owns a copy of only that slice of the input, and
/// pinning it retains nothing else. Because a capture builds a document, the
/// default arena cap (256 MiB) applies to its subtree and a larger one fails
/// with [`JsonError::ArenaLimitExceeded`]; fully typed fields stream without
/// that cap.
///
/// Both limits are fixed at the parse defaults here; to choose them, use
/// [`from_slice_with_buffers`], which takes [`ParseOptions`](crate::ParseOptions).
///
/// # Errors
/// Returns [`JsonError`] when the input is not valid JSON, exceeds the
/// depth limit, or does not match `T`'s shape.
pub fn from_slice<T>(input: &[u8]) -> Result<T, JsonError>
where
    T: de::DeserializeOwned,
{
    // Validate UTF-8 once for the whole input, as parsing does: quotes are
    // ASCII, so every string the lexer hands out is then trusted UTF-8.
    stream::deserialize(crate::utf8::validate_utf8(input)?)
}

/// Deserializes a `T` from a JSON string in a single pass; see
/// [`from_slice`] for the streaming semantics and [`from_value`] for the
/// shared data model.
///
/// # Errors
/// Returns [`JsonError`] when the input is not valid JSON, exceeds the
/// depth limit, or does not match `T`'s shape.
pub fn from_str<T>(input: &str) -> Result<T, JsonError>
where
    T: de::DeserializeOwned,
{
    stream::deserialize(input.into())
}

/// Parses JSON bytes into a document, deserializes a `T` from it, and
/// reclaims the document's storage into `buffers`; see [`from_value`]
/// for the data-model semantics — including its duplicate-key collapse,
/// which this document-based entry point keeps where [`from_slice`]
/// rejects duplicates.
///
/// This is the deserialize-and-drop loop's document path (one call per
/// request): the parse draws its arena and scratch storage from `buffers`,
/// and once `T` is built the storage is reclaimed for the next call. Keep
/// one `ParseBuffers` per loop and steady state stays nearly
/// allocation-free, exactly like [`Value::parse_with_buffers`] followed
/// by [`Value::recycle`]. Prefer [`from_slice`] unless the loop also
/// needs the document itself.
///
/// When `T` captures part of the document — a [`Value`] field — the
/// captured subtree keeps the arena alive, so the arena cannot
/// be reclaimed and the next call allocates a fresh one; only the parser
/// scratch is reused. Recycling pays off for fully typed `T`.
///
/// # Example
/// ```
/// use apollo_json::{ParseBuffers, ParseOptions};
///
/// let options = ParseOptions::default();
/// let mut buffers = ParseBuffers::new();
/// for _ in 0..3 {
///     let ids: Vec<u32> = apollo_json::from_slice_with_buffers(
///         br#"[1, 2, 3]"#,
///         &options,
///         &mut buffers,
///     )?;
///     assert_eq!(ids, [1, 2, 3]); // storage backs the next iteration
/// }
/// # Ok::<(), apollo_json::JsonError>(())
/// ```
///
/// # Errors
/// Returns [`JsonError`] when the input is not valid JSON, exceeds the
/// limits in `options`, or does not match `T`'s shape.
pub fn from_slice_with_buffers<T>(
    input: &[u8],
    options: &crate::ParseOptions,
    buffers: &mut crate::ParseBuffers,
) -> Result<T, JsonError>
where
    T: de::DeserializeOwned,
{
    let parsed = Value::parse_with_buffers(input.to_vec(), options, buffers)?;
    let value = from_value(&parsed)?;
    parsed.recycle(buffers);
    Ok(value)
}

/// Serde deserializer positioned on one node.
pub(super) struct ValueDeserializer<'de> {
    arena: &'de Arc<Arena>,
    node: NodeId,
    /// Remaining nesting budget; opening a container at zero aborts.
    depth: usize,
}

impl<'de> ValueDeserializer<'de> {
    fn new(arena: &'de Arc<Arena>, node: NodeId) -> Self {
        ValueDeserializer {
            arena,
            node,
            depth: DEFAULT_MAX_DEPTH,
        }
    }

    pub(super) fn resolved(arena: &'de Arc<Arena>, node: NodeId, depth: usize) -> Self {
        ValueDeserializer { arena, node, depth }
    }

    pub(super) fn arena(&self) -> &'de Arc<Arena> {
        self.arena
    }

    pub(super) fn node_id(&self) -> NodeId {
        self.node
    }

    fn value(&self) -> ValueRef<'de> {
        ValueRef {
            arena: self.arena.as_ref(),
            node: self.node,
        }
    }

    pub(super) fn node(&self) -> Node {
        self.arena.node(self.node)
    }

    /// The nesting budget for children, or the depth error when this
    /// container would exceed it.
    fn descend(&self) -> Result<usize, JsonError> {
        self.depth
            .checked_sub(1)
            .ok_or(JsonError::DepthLimitExceeded {
                limit: DEFAULT_MAX_DEPTH,
            })
    }

    fn visit_string_node<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value().as_str() {
            Some(Cow::Borrowed(s)) => visitor.visit_borrowed_str(s),
            Some(Cow::Owned(s)) => visitor.visit_string(s),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn visit_number_node<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value().raw_number() {
            Some(raw) => visit_number_literal(raw, visitor),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    pub(super) fn visit_array_node<V>(
        self,
        slab: SlabRef,
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        let depth = self.descend()?;
        let children = self.arena().children(slab);
        let mut access = access::SeqAccess::new(self.arena, children, depth);
        let value = visitor.visit_seq(&mut access)?;
        if access.is_exhausted() {
            Ok(value)
        } else {
            Err(de::Error::invalid_length(
                children.len(),
                &"fewer elements in array",
            ))
        }
    }

    pub(super) fn visit_object_node<V>(
        self,
        slab: SlabRef,
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        let depth = self.descend()?;
        let entries = self.arena().entries(slab);
        let mut access = access::MapAccess::new(self.arena, entries, depth);
        let value = visitor.visit_map(&mut access)?;
        if access.is_exhausted() {
            Ok(value)
        } else {
            Err(de::Error::invalid_length(
                entries.len(),
                &"fewer elements in map",
            ))
        }
    }

    /// Byte offset of the value in its arena's input, for span-backed leaves.
    fn position(&self) -> Option<usize> {
        match self.node() {
            Node::Number(span) | Node::String { span, .. } => Some(span.start as usize),
            _ => None,
        }
    }

    /// The `invalid type` error for this node, with the found value, the
    /// visitor's expectation, and the byte offset where available.
    pub(super) fn invalid_type(&self, exp: &dyn Expected) -> JsonError {
        let value = self.value();
        let string;
        let unexpected = match self.node() {
            Node::Null => Unexpected::Unit,
            Node::Bool(b) => Unexpected::Bool(b),
            Node::Number(_) | Node::OwnedNumber(_) => {
                number_unexpected(value.raw_number().expect("node is a number"))
            }
            Node::String { .. } | Node::OwnedString(_) => {
                string = value.as_str().expect("node is a string");
                Unexpected::Str(&string)
            }
            Node::Array(_) => Unexpected::Seq,
            Node::Object(_) => Unexpected::Map,
            Node::MutArray(_) | Node::MutObject(_) => {
                unreachable!("builder-only nodes never appear in sealed documents")
            }
        };
        let mut message = format!("invalid type: {unexpected}, expected {exp}");
        if let Some(offset) = self.position() {
            message.push_str(&format!(" at byte offset {offset}"));
        }
        JsonError::Deserialization { message }
    }
}

impl<'de> de::Deserializer<'de> for ValueDeserializer<'de> {
    type Error = JsonError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Null => visitor.visit_unit(),
            Node::Bool(b) => visitor.visit_bool(b),
            Node::Number(_) | Node::OwnedNumber(_) => self.visit_number_node(visitor),
            Node::String { .. } | Node::OwnedString(_) => self.visit_string_node(visitor),
            Node::Array(slab) => self.visit_array_node(slab, visitor),
            Node::Object(slab) => self.visit_object_node(slab, visitor),
            Node::MutArray(_) | Node::MutObject(_) => {
                unreachable!("builder-only nodes never appear in sealed documents")
            }
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Bool(b) => visitor.visit_bool(b),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_i128<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value().raw_number() {
            Some(raw) => visit_i128_literal(raw, visitor),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_number_node(visitor)
    }

    fn deserialize_u128<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value().raw_number() {
            Some(raw) => visit_u128_literal(raw, visitor),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value().raw_number() {
            Some(raw) => visit_f32_literal(raw, visitor),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value().raw_number() {
            Some(raw) => visit_f64_literal(raw, visitor),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_string_node(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_string_node(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_string_node(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::String { .. } | Node::OwnedString(_) => self.visit_string_node(visitor),
            Node::Array(slab) => self.visit_array_node(slab, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Null => visitor.visit_unit(),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        if name == crate::handoff::TOKEN {
            return capture::capture(&self, visitor);
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Array(slab) => self.visit_array_node(slab, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Object(slab) => self.visit_object_node(slab, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::Object(slab) => self.visit_object_node(slab, visitor),
            Node::Array(slab) => self.visit_array_node(slab, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.node() {
            Node::String { .. } | Node::OwnedString(_) => {
                let variant = self.value().as_str().expect("node is a string");
                visitor.visit_enum(access::EnumAccess::new(variant, None))
            }
            Node::Object(slab) => {
                let entries = self.arena().entries(slab);
                let [entry] = entries else {
                    return Err(de::Error::invalid_value(
                        Unexpected::Map,
                        &"map with a single key",
                    ));
                };
                let variant = self.arena().key_unescaped(entry.key);
                let depth = self.descend()?;
                let (arena, node) = crate::arena::resolve_shared(self.arena, entry.child);
                let value = ValueDeserializer::resolved(arena, node, depth);
                visitor.visit_enum(access::EnumAccess::new(variant, Some(value)))
            }
            _ => Err(self.invalid_type(&"string or map")),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_string_node(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        // The document is already parsed; ignoring a value is free — nothing
        // is decoded or materialized.
        visitor.visit_unit()
    }
}

/// Whether the literal has a fraction or exponent part.
pub(crate) fn is_float_literal(raw: &str) -> bool {
    raw.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'))
}

/// `serde_json`'s reading of a number literal: `u64` for non-negative
/// integer literals, `i64` for negative ones, `f64` otherwise. Integer
/// literals beyond 64 bits fall back to `f64`, and so does `-0`, whose sign
/// only a float can carry.
pub(crate) enum NumberClass {
    Unsigned(u64),
    Signed(i64),
    Float,
}

pub(crate) fn classify_number(raw: &str) -> NumberClass {
    if !is_float_literal(raw) {
        if raw.starts_with('-') {
            match raw.parse::<i64>() {
                // `-0` must stay a float (-0.0) or its sign is silently
                // dropped; serde_json reads it the same way.
                Ok(0) => return NumberClass::Float,
                Ok(n) => return NumberClass::Signed(n),
                Err(_) => {}
            }
        } else if let Ok(n) = raw.parse::<u64>() {
            return NumberClass::Unsigned(n);
        }
    }
    NumberClass::Float
}

/// Visits a number literal at its natural width, per [`classify_number`].
pub(super) fn visit_number_literal<'de, V>(raw: &str, visitor: V) -> Result<V::Value, JsonError>
where
    V: Visitor<'de>,
{
    match classify_number(raw) {
        NumberClass::Unsigned(n) => visitor.visit_u64(n),
        NumberClass::Signed(n) => visitor.visit_i64(n),
        NumberClass::Float => visit_f64_literal(raw, visitor),
    }
}

pub(super) fn visit_i128_literal<'de, V>(raw: &str, visitor: V) -> Result<V::Value, JsonError>
where
    V: Visitor<'de>,
{
    if is_float_literal(raw) {
        return visit_number_literal(raw, visitor);
    }
    match raw.parse::<i128>() {
        Ok(n) => visitor.visit_i128(n),
        Err(_) => Err(number_out_of_range(raw)),
    }
}

pub(super) fn visit_u128_literal<'de, V>(raw: &str, visitor: V) -> Result<V::Value, JsonError>
where
    V: Visitor<'de>,
{
    if is_float_literal(raw) {
        return visit_number_literal(raw, visitor);
    }
    match raw.parse::<u128>() {
        Ok(n) => visitor.visit_u128(n),
        Err(_) => Err(number_out_of_range(raw)),
    }
}

pub(super) fn visit_f32_literal<'de, V>(raw: &str, visitor: V) -> Result<V::Value, JsonError>
where
    V: Visitor<'de>,
{
    let value: f32 = raw.parse().expect("number literals parse as floats");
    if value.is_finite() {
        visitor.visit_f32(value)
    } else {
        Err(number_out_of_range(raw))
    }
}

pub(super) fn visit_f64_literal<'de, V>(raw: &str, visitor: V) -> Result<V::Value, JsonError>
where
    V: Visitor<'de>,
{
    let value: f64 = raw.parse().expect("number literals parse as floats");
    if value.is_finite() {
        visitor.visit_f64(value)
    } else {
        Err(number_out_of_range(raw))
    }
}

fn number_out_of_range(raw: &str) -> JsonError {
    JsonError::Deserialization {
        message: format!("number out of range: {raw}"),
    }
}

/// Classifies a number literal for `invalid type` messages, mirroring the
/// widths [`visit_number_literal`] dispatches at.
pub(super) fn number_unexpected(raw: &str) -> Unexpected<'static> {
    match classify_number(raw) {
        NumberClass::Unsigned(n) => Unexpected::Unsigned(n),
        NumberClass::Signed(n) => Unexpected::Signed(n),
        NumberClass::Float => {
            Unexpected::Float(raw.parse().expect("number literals parse as floats"))
        }
    }
}

/// Whether `key` spells exactly one JSON number literal, the precondition for
/// numeric map-key coercion.
pub(crate) fn is_json_number(key: &str) -> bool {
    let bytes = key.as_bytes();
    let mut i = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            i += 1;
            while matches!(bytes.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        if !matches!(bytes.get(i), Some(b'0'..=b'9')) {
            return false;
        }
        while matches!(bytes.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !matches!(bytes.get(i), Some(b'0'..=b'9')) {
            return false;
        }
        while matches!(bytes.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
    }
    i == bytes.len()
}

#[cfg(test)]
mod tests;
