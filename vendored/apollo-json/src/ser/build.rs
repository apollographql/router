//! Building documents from `Serialize` types.

use std::sync::Arc;

use ahash::AHashMap;
use serde::ser::{self, Impossible, Serialize};

use crate::arena::Arena;
use crate::document::Value;
use crate::error::JsonError;
use crate::handoff;
use crate::node::{Child, Entry, KeyRef, Node};
use crate::slab::SlabRef;

/// Serializes `value` into a new [`Value`].
///
/// The mapping follows `serde_json`: structs and maps become objects,
/// sequences and tuples become arrays, enum variants use the externally
/// tagged form, `None` and units become `null`, and bytes become arrays of
/// numbers. Map keys must be strings, characters, integers, or booleans —
/// they take their string form. Duplicate keys collapse to first position,
/// last value, exactly as parsing does. Numbers are written with
/// `serde_json`'s formatting; 128-bit integers keep their full value as
/// number literals, and non-finite floats become `null`, as `serde_json`
/// writes them.
///
/// Fields of type [`Value`] are **adopted by
/// reference**: the subtree is shared into the output document — one
/// refcount bump, no copy — and reserializes byte-identically, raw literal
/// spellings intact. The output then pins the source arena like any other
/// composition (see [`Value::into_self_contained`]). A serializer
/// wrapper that records and replays the serialization stream cannot carry
/// the shared handle across the replay; such values land as structural
/// copies instead — equal content, no sharing, never an error.
///
/// # Errors
/// [`JsonError::NonFiniteNumber`] for a non-finite float used as a map key,
/// and [`JsonError::Serialization`] for a map key that is not a string-like
/// type or a custom error from a `Serialize` implementation.
///
/// # Example
/// ```
/// use apollo_json::to_value;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Greeting {
///     hello: String,
/// }
///
/// let doc = to_value(&Greeting {
///     hello: "world".into(),
/// })?;
/// assert_eq!(doc.to_vec(), br#"{"hello":"world"}"#);
/// # Ok::<(), apollo_json::JsonError>(())
/// ```
pub fn to_value<T>(value: &T) -> Result<Value, JsonError>
where
    T: Serialize + ?Sized,
{
    let mut sink = Sink {
        arena: Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE),
    };
    let root = value.serialize(&mut sink)?;
    Ok(sink.into_value(root))
}

/// `serde_json`'s in-band marker: with its `arbitrary_precision` feature
/// enabled, `serde_json::Number` serializes as a struct with this name whose
/// single field is the literal text. Recognizing it keeps such numbers
/// crossing into documents as raw literals instead of nonsense objects.
const SERDE_JSON_NUMBER_TOKEN: &str = "$serde_json::private::Number";

/// The arena a document is being built into.
struct Sink {
    arena: Arena,
}

impl Sink {
    fn into_value(self, root: Child) -> Value {
        match root.as_local() {
            Some(id) => Value::rooted(self.arena, id),
            // The root itself was adopted: the output is the adopted value.
            None => {
                let fref = self
                    .arena
                    .foreign_ref(root.as_foreign().expect("root is foreign"));
                Value {
                    arena: Arc::clone(&fref.arena),
                    node: fref.node,
                }
            }
        }
    }

    fn push_node(&mut self, node: Node) -> Child {
        Child::local(self.arena.push_node(node))
    }

    fn push_string(&mut self, value: &str) -> Child {
        let text = self.arena.alloc_text(value);
        self.push_node(Node::OwnedString(text))
    }

    fn push_number(&mut self, literal: &str) -> Child {
        let text = self.arena.alloc_text(literal);
        self.push_node(Node::OwnedNumber(text))
    }

    fn push_f64(&mut self, value: f64) -> Child {
        // serde_json's serializers write null for NaN and the infinities —
        // JSON has no representation for them — and to_value matches.
        match serde_json::Number::from_f64(value) {
            Some(number) => self.push_number(&number.to_string()),
            None => self.push_node(Node::Null),
        }
    }

    /// Wraps `content` in a single-member object, the externally tagged
    /// variant form.
    fn variant_object(&mut self, variant: &str, content: Child) -> Child {
        let key = KeyRef::Owned(self.arena.alloc_text(variant));
        let slab = self.arena.alloc_entries(&[Entry {
            key,
            child: content,
        }]);
        self.push_node(Node::Object(slab))
    }
}

impl<'a> ser::Serializer for &'a mut Sink {
    type Ok = Child;
    type Error = JsonError;
    type SerializeSeq = SeqBuilder<'a>;
    type SerializeTuple = SeqBuilder<'a>;
    type SerializeTupleStruct = SeqBuilder<'a>;
    type SerializeTupleVariant = VariantSeqBuilder<'a>;
    type SerializeMap = MapBuilder<'a>;
    type SerializeStruct = StructBuilder<'a>;
    type SerializeStructVariant = VariantStructBuilder<'a>;

    fn serialize_bool(self, value: bool) -> Result<Child, JsonError> {
        Ok(self.push_node(Node::Bool(value)))
    }

    fn serialize_i8(self, value: i8) -> Result<Child, JsonError> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<Child, JsonError> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<Child, JsonError> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<Child, JsonError> {
        Ok(self.push_number(&value.to_string()))
    }

    fn serialize_i128(self, value: i128) -> Result<Child, JsonError> {
        Ok(self.push_number(&value.to_string()))
    }

    fn serialize_u8(self, value: u8) -> Result<Child, JsonError> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<Child, JsonError> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<Child, JsonError> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<Child, JsonError> {
        Ok(self.push_number(&value.to_string()))
    }

    fn serialize_u128(self, value: u128) -> Result<Child, JsonError> {
        Ok(self.push_number(&value.to_string()))
    }

    fn serialize_f32(self, value: f32) -> Result<Child, JsonError> {
        self.serialize_f64(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<Child, JsonError> {
        Ok(self.push_f64(value))
    }

    fn serialize_char(self, value: char) -> Result<Child, JsonError> {
        Ok(self.push_string(value.encode_utf8(&mut [0u8; 4])))
    }

    fn serialize_str(self, value: &str) -> Result<Child, JsonError> {
        Ok(self.push_string(value))
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Child, JsonError> {
        let children: Vec<Child> = value
            .iter()
            .map(|&byte| self.push_number(&byte.to_string()))
            .collect();
        let slab = self.arena.alloc_children(&children);
        Ok(self.push_node(Node::Array(slab)))
    }

    fn serialize_none(self) -> Result<Child, JsonError> {
        Ok(self.push_node(Node::Null))
    }

    fn serialize_some<T>(self, value: &T) -> Result<Child, JsonError>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Child, JsonError> {
        Ok(self.push_node(Node::Null))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Child, JsonError> {
        Ok(self.push_node(Node::Null))
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Child, JsonError> {
        Ok(self.push_string(variant))
    }

    fn serialize_newtype_struct<T>(self, name: &'static str, value: &T) -> Result<Child, JsonError>
    where
        T: ?Sized + Serialize,
    {
        if name == handoff::TOKEN {
            // A Value field: adopt the stashed subtree by
            // reference. An empty slot means a wrapper replayed the marker
            // without the hand-off — serialize the content as a plain copy.
            if let Some((arena, node)) = handoff::take() {
                return Ok(self.arena.push_foreign(arena, node));
            }
        }
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Child, JsonError>
    where
        T: ?Sized + Serialize,
    {
        let content = value.serialize(&mut *self)?;
        Ok(self.variant_object(variant, content))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqBuilder<'a>, JsonError> {
        Ok(SeqBuilder {
            sink: self,
            children: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqBuilder<'a>, JsonError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<SeqBuilder<'a>, JsonError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantSeqBuilder<'a>, JsonError> {
        Ok(VariantSeqBuilder {
            variant,
            inner: SeqBuilder {
                sink: self,
                children: Vec::with_capacity(len),
            },
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<MapBuilder<'a>, JsonError> {
        Ok(MapBuilder {
            object: ObjectBuilder::new(self, len.unwrap_or(0)),
            pending: None,
        })
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<StructBuilder<'a>, JsonError> {
        if name == SERDE_JSON_NUMBER_TOKEN {
            return Ok(StructBuilder::RawNumber {
                sink: self,
                number: None,
            });
        }
        Ok(StructBuilder::Object(ObjectBuilder::new(self, len)))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantStructBuilder<'a>, JsonError> {
        Ok(VariantStructBuilder {
            variant,
            object: ObjectBuilder::new(self, len),
        })
    }
}

/// Collects array elements, packing them into a slab when the sequence ends.
struct SeqBuilder<'a> {
    sink: &'a mut Sink,
    children: Vec<Child>,
}

impl<'a> SeqBuilder<'a> {
    fn element<T>(&mut self, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        let child = value.serialize(&mut *self.sink)?;
        self.children.push(child);
        Ok(())
    }

    fn finish(self) -> (&'a mut Sink, Child) {
        let SeqBuilder { sink, children } = self;
        let slab = sink.arena.alloc_children(&children);
        let array = sink.push_node(Node::Array(slab));
        (sink, array)
    }
}

impl ser::SerializeSeq for SeqBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> Result<Child, JsonError> {
        Ok(self.finish().1)
    }
}

impl ser::SerializeTuple for SeqBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> Result<Child, JsonError> {
        Ok(self.finish().1)
    }
}

impl ser::SerializeTupleStruct for SeqBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        self.element(value)
    }

    fn end(self) -> Result<Child, JsonError> {
        Ok(self.finish().1)
    }
}

/// A tuple variant: the array wrapped in the externally tagged object.
struct VariantSeqBuilder<'a> {
    variant: &'static str,
    inner: SeqBuilder<'a>,
}

impl ser::SerializeTupleVariant for VariantSeqBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        self.inner.element(value)
    }

    fn end(self) -> Result<Child, JsonError> {
        let (sink, content) = self.inner.finish();
        Ok(sink.variant_object(self.variant, content))
    }
}

/// Collects object members, packing them into a slab when the container
/// ends.
struct ObjectBuilder<'a> {
    sink: &'a mut Sink,
    entries: Vec<Entry>,
    /// Owned key text to entry index, built once the object grows past the
    /// width where the linear duplicate scan turns quadratic — the same
    /// threshold the parser uses. Keys are copied out because the arena's
    /// text storage keeps growing while members serialize.
    index: Option<AHashMap<Box<[u8]>, u32>>,
}

impl<'a> ObjectBuilder<'a> {
    fn new(sink: &'a mut Sink, capacity: usize) -> Self {
        ObjectBuilder {
            sink,
            entries: Vec::with_capacity(capacity),
            index: None,
        }
    }

    fn member<T>(&mut self, key: SlabRef, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        let child = value.serialize(&mut *self.sink)?;
        self.insert_entry(key, child);
        Ok(())
    }

    /// Inserts `(key, child)` with parse semantics for duplicate keys: first
    /// position, last value.
    fn insert_entry(&mut self, key: SlabRef, child: Child) {
        let bytes = self.sink.arena.text(key);
        if let Some(map) = &mut self.index {
            // Probe with the borrowed key first: only new keys pay for the
            // boxed copy, duplicates stay allocation-free.
            if let Some(&slot) = map.get(bytes) {
                self.entries[slot as usize].child = child;
            } else {
                map.insert(
                    bytes.into(),
                    u32::try_from(self.entries.len()).expect("entry count within range"),
                );
                self.entries.push(Entry {
                    key: KeyRef::Owned(key),
                    child,
                });
            }
            return;
        }
        match self
            .entries
            .iter()
            .position(|entry| self.sink.arena.key_matches_bytes(entry.key, bytes))
        {
            Some(existing) => self.entries[existing].child = child,
            None => {
                self.entries.push(Entry {
                    key: KeyRef::Owned(key),
                    child,
                });
                if self.entries.len() >= crate::parse::INDEX_THRESHOLD {
                    self.index = Some(
                        self.entries
                            .iter()
                            .enumerate()
                            .map(|(i, entry)| {
                                let KeyRef::Owned(slab) = entry.key else {
                                    unreachable!("built objects only hold owned keys")
                                };
                                (
                                    self.sink.arena.text(slab).into(),
                                    u32::try_from(i).expect("entry count within range"),
                                )
                            })
                            .collect(),
                    );
                }
            }
        }
    }

    fn finish(self) -> (&'a mut Sink, Child) {
        let ObjectBuilder { sink, entries, .. } = self;
        let slab = sink.arena.alloc_entries(&entries);
        let object = sink.push_node(Node::Object(slab));
        (sink, object)
    }
}

/// Map access: keys arrive through their own serializer before each value.
struct MapBuilder<'a> {
    object: ObjectBuilder<'a>,
    pending: Option<SlabRef>,
}

impl ser::SerializeMap for MapBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        self.pending = Some(key.serialize(KeySerializer {
            sink: self.object.sink,
        })?);
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        let key = self
            .pending
            .take()
            .expect("serialize_value called before serialize_key");
        self.object.member(key, value)
    }

    fn end(self) -> Result<Child, JsonError> {
        Ok(self.object.finish().1)
    }
}

/// A struct: an object, or `serde_json`'s arbitrary-precision number in
/// disguise.
enum StructBuilder<'a> {
    Object(ObjectBuilder<'a>),
    RawNumber {
        sink: &'a mut Sink,
        number: Option<Child>,
    },
}

impl ser::SerializeStruct for StructBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        match self {
            StructBuilder::Object(object) => {
                let key = object.sink.arena.alloc_text(key);
                object.member(key, value)
            }
            StructBuilder::RawNumber { sink, number } => {
                *number = Some(value.serialize(RawNumberSerializer { sink })?);
                Ok(())
            }
        }
    }

    fn end(self) -> Result<Child, JsonError> {
        match self {
            StructBuilder::Object(object) => Ok(object.finish().1),
            StructBuilder::RawNumber { number, .. } => {
                number.ok_or_else(|| JsonError::Serialization {
                    message: "expected a JSON number literal".to_owned(),
                })
            }
        }
    }
}

/// A struct variant: the object wrapped in the externally tagged object.
struct VariantStructBuilder<'a> {
    variant: &'static str,
    object: ObjectBuilder<'a>,
}

impl ser::SerializeStructVariant for VariantStructBuilder<'_> {
    type Ok = Child;
    type Error = JsonError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), JsonError>
    where
        T: ?Sized + Serialize,
    {
        let key = self.object.sink.arena.alloc_text(key);
        self.object.member(key, value)
    }

    fn end(self) -> Result<Child, JsonError> {
        let (sink, content) = self.object.finish();
        Ok(sink.variant_object(self.variant, content))
    }
}

fn key_must_be_a_string() -> JsonError {
    JsonError::Serialization {
        message: "key must be a string".to_owned(),
    }
}

/// Serializer for one map key: strings, characters, integers, floats, and
/// booleans take their string form, as `serde_json` allows; everything else
/// is rejected.
struct KeySerializer<'a> {
    sink: &'a mut Sink,
}

impl KeySerializer<'_> {
    fn text(self, key: &str) -> Result<SlabRef, JsonError> {
        Ok(self.sink.arena.alloc_text(key))
    }
}

macro_rules! serialize_integer_key {
    ($method:ident, $ty:ty) => {
        fn $method(self, value: $ty) -> Result<SlabRef, JsonError> {
            self.text(&value.to_string())
        }
    };
}

impl<'a> ser::Serializer for KeySerializer<'a> {
    type Ok = SlabRef;
    type Error = JsonError;
    type SerializeSeq = Impossible<SlabRef, JsonError>;
    type SerializeTuple = Impossible<SlabRef, JsonError>;
    type SerializeTupleStruct = Impossible<SlabRef, JsonError>;
    type SerializeTupleVariant = Impossible<SlabRef, JsonError>;
    type SerializeMap = Impossible<SlabRef, JsonError>;
    type SerializeStruct = Impossible<SlabRef, JsonError>;
    type SerializeStructVariant = Impossible<SlabRef, JsonError>;

    fn serialize_str(self, value: &str) -> Result<SlabRef, JsonError> {
        self.text(value)
    }

    fn serialize_char(self, value: char) -> Result<SlabRef, JsonError> {
        self.text(value.encode_utf8(&mut [0u8; 4]))
    }

    fn serialize_bool(self, value: bool) -> Result<SlabRef, JsonError> {
        self.text(if value { "true" } else { "false" })
    }

    serialize_integer_key!(serialize_i8, i8);
    serialize_integer_key!(serialize_i16, i16);
    serialize_integer_key!(serialize_i32, i32);
    serialize_integer_key!(serialize_i64, i64);
    serialize_integer_key!(serialize_i128, i128);
    serialize_integer_key!(serialize_u8, u8);
    serialize_integer_key!(serialize_u16, u16);
    serialize_integer_key!(serialize_u32, u32);
    serialize_integer_key!(serialize_u64, u64);
    serialize_integer_key!(serialize_u128, u128);

    fn serialize_f32(self, value: f32) -> Result<SlabRef, JsonError> {
        self.serialize_f64(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<SlabRef, JsonError> {
        let number = serde_json::Number::from_f64(value).ok_or(JsonError::NonFiniteNumber)?;
        self.text(&number.to_string())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<SlabRef, JsonError> {
        self.text(variant)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<SlabRef, JsonError>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<SlabRef, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_none(self) -> Result<SlabRef, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_some<T>(self, _value: &T) -> Result<SlabRef, JsonError>
    where
        T: ?Sized + Serialize,
    {
        Err(key_must_be_a_string())
    }

    fn serialize_unit(self) -> Result<SlabRef, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<SlabRef, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<SlabRef, JsonError>
    where
        T: ?Sized + Serialize,
    {
        Err(key_must_be_a_string())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, JsonError> {
        Err(key_must_be_a_string())
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, JsonError> {
        Err(key_must_be_a_string())
    }
}

/// Serializer for the field of `serde_json`'s arbitrary-precision number
/// struct: only a string spelling a complete JSON number literal is
/// accepted, and it becomes the document's raw literal.
struct RawNumberSerializer<'a> {
    sink: &'a mut Sink,
}

impl<'a> ser::Serializer for RawNumberSerializer<'a> {
    type Ok = Child;
    type Error = JsonError;
    type SerializeSeq = Impossible<Child, JsonError>;
    type SerializeTuple = Impossible<Child, JsonError>;
    type SerializeTupleStruct = Impossible<Child, JsonError>;
    type SerializeTupleVariant = Impossible<Child, JsonError>;
    type SerializeMap = Impossible<Child, JsonError>;
    type SerializeStruct = Impossible<Child, JsonError>;
    type SerializeStructVariant = Impossible<Child, JsonError>;

    fn serialize_str(self, value: &str) -> Result<Child, JsonError> {
        if crate::de::is_json_number(value) {
            Ok(self.sink.push_number(value))
        } else {
            Err(JsonError::Serialization {
                message: format!("invalid JSON number literal: {value}"),
            })
        }
    }

    fn serialize_bool(self, _value: bool) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_i8(self, _value: i8) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_i16(self, _value: i16) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_i32(self, _value: i32) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_i64(self, _value: i64) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_u8(self, _value: u8) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_u16(self, _value: u16) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_u32(self, _value: u32) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_u64(self, _value: u64) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_f32(self, _value: f32) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_f64(self, _value: f64) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_char(self, _value: char) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_none(self) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_some<T>(self, _value: &T) -> Result<Child, JsonError>
    where
        T: ?Sized + Serialize,
    {
        Err(invalid_raw_number())
    }

    fn serialize_unit(self) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Child, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> Result<Child, JsonError>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Child, JsonError>
    where
        T: ?Sized + Serialize,
    {
        Err(invalid_raw_number())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, JsonError> {
        Err(invalid_raw_number())
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, JsonError> {
        Err(invalid_raw_number())
    }
}

fn invalid_raw_number() -> JsonError {
    JsonError::Serialization {
        message: "expected a JSON number literal".to_owned(),
    }
}
