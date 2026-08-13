//! Sequence, map, map-key, and enum access for the typed deserializer.

use std::borrow::Cow;
use std::sync::Arc;

use serde::de::{self, DeserializeSeed, Unexpected, Visitor};
use serde::forward_to_deserialize_any;

use crate::arena::{Arena, resolve_shared};
use crate::error::JsonError;
use crate::node::{Child, Entry, Node};

use super::{
    ValueDeserializer, is_json_number, visit_f32_literal, visit_f64_literal, visit_i128_literal,
    visit_number_literal, visit_u128_literal,
};

pub(super) struct SeqAccess<'de> {
    arena: &'de Arc<Arena>,
    iter: std::slice::Iter<'de, Child>,
    /// Nesting budget for the elements.
    depth: usize,
}

impl<'de> SeqAccess<'de> {
    pub(super) fn new(arena: &'de Arc<Arena>, children: &'de [Child], depth: usize) -> Self {
        SeqAccess {
            arena,
            iter: children.iter(),
            depth,
        }
    }

    pub(super) fn is_exhausted(&self) -> bool {
        self.iter.len() == 0
    }
}

impl<'de> de::SeqAccess<'de> for SeqAccess<'de> {
    type Error = JsonError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, JsonError>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(&child) = self.iter.next() else {
            return Ok(None);
        };
        let (arena, node) = resolve_shared(self.arena, child);
        seed.deserialize(ValueDeserializer::resolved(arena, node, self.depth))
            .map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

pub(super) struct MapAccess<'de> {
    arena: &'de Arc<Arena>,
    iter: std::slice::Iter<'de, Entry>,
    /// The value belonging to the key just handed out.
    pending: Option<Child>,
    /// Nesting budget for the values.
    depth: usize,
}

impl<'de> MapAccess<'de> {
    pub(super) fn new(arena: &'de Arc<Arena>, entries: &'de [Entry], depth: usize) -> Self {
        MapAccess {
            arena,
            iter: entries.iter(),
            pending: None,
            depth,
        }
    }

    pub(super) fn is_exhausted(&self) -> bool {
        self.iter.len() == 0 && self.pending.is_none()
    }
}

impl<'de> de::MapAccess<'de> for MapAccess<'de> {
    type Error = JsonError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, JsonError>
    where
        K: DeserializeSeed<'de>,
    {
        let Some(entry) = self.iter.next() else {
            return Ok(None);
        };
        self.pending = Some(entry.child);
        let key = self.arena.key_unescaped(entry.key);
        seed.deserialize(MapKeyDeserializer { key }).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, JsonError>
    where
        V: DeserializeSeed<'de>,
    {
        let child = self
            .pending
            .take()
            .expect("next_value called before next_key");
        let (arena, node) = resolve_shared(self.arena, child);
        seed.deserialize(ValueDeserializer::resolved(arena, node, self.depth))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

/// Deserializer for one object key. Keys are strings; numeric and bool
/// target types coerce from the string form with `serde_json`'s rules.
/// Shared by the document and streaming paths.
pub(super) struct MapKeyDeserializer<'de> {
    key: Cow<'de, str>,
}

impl<'de> MapKeyDeserializer<'de> {
    pub(super) fn new(key: Cow<'de, str>) -> Self {
        MapKeyDeserializer { key }
    }

    fn visit_key<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Cow::Borrowed(s) => visitor.visit_borrowed_str(s),
            Cow::Owned(s) => visitor.visit_string(s),
        }
    }

    /// The key text when it spells a complete JSON number literal.
    fn numeric_key(&self) -> Result<&str, JsonError> {
        if is_json_number(&self.key) {
            Ok(&self.key)
        } else {
            Err(JsonError::Deserialization {
                message: format!("expected numeric key, found `{}`", self.key),
            })
        }
    }
}

macro_rules! deserialize_numeric_key {
    ($method:ident, $visit:ident) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, JsonError>
        where
            V: Visitor<'de>,
        {
            $visit(self.numeric_key()?, visitor)
        }
    };
}

impl<'de> de::Deserializer<'de> for MapKeyDeserializer<'de> {
    type Error = JsonError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.visit_key(visitor)
    }

    deserialize_numeric_key!(deserialize_i8, visit_number_literal);
    deserialize_numeric_key!(deserialize_i16, visit_number_literal);
    deserialize_numeric_key!(deserialize_i32, visit_number_literal);
    deserialize_numeric_key!(deserialize_i64, visit_number_literal);
    deserialize_numeric_key!(deserialize_i128, visit_i128_literal);
    deserialize_numeric_key!(deserialize_u8, visit_number_literal);
    deserialize_numeric_key!(deserialize_u16, visit_number_literal);
    deserialize_numeric_key!(deserialize_u32, visit_number_literal);
    deserialize_numeric_key!(deserialize_u64, visit_number_literal);
    deserialize_numeric_key!(deserialize_u128, visit_u128_literal);
    deserialize_numeric_key!(deserialize_f32, visit_f32_literal);
    deserialize_numeric_key!(deserialize_f64, visit_f64_literal);

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.key.as_ref() {
            "true" => visitor.visit_bool(true),
            "false" => visitor.visit_bool(false),
            _ => Err(de::Error::invalid_type(
                Unexpected::Str(&self.key),
                &visitor,
            )),
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        // Map keys cannot be null.
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
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
        visitor.visit_enum(EnumAccess::new(self.key, None))
    }

    forward_to_deserialize_any! {
        char str string bytes byte_buf unit unit_struct seq tuple tuple_struct
        map struct identifier ignored_any
    }
}

/// Enum access over a unit-variant string or a single-entry object.
pub(super) struct EnumAccess<'de> {
    variant: Cow<'de, str>,
    /// The variant's content; `None` for the bare-string (unit) form.
    value: Option<ValueDeserializer<'de>>,
}

impl<'de> EnumAccess<'de> {
    pub(super) fn new(variant: Cow<'de, str>, value: Option<ValueDeserializer<'de>>) -> Self {
        EnumAccess { variant, value }
    }
}

impl<'de> de::EnumAccess<'de> for EnumAccess<'de> {
    type Error = JsonError;
    type Variant = VariantAccess<'de>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, VariantAccess<'de>), JsonError>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(MapKeyDeserializer { key: self.variant })?;
        Ok((variant, VariantAccess { value: self.value }))
    }
}

pub(super) struct VariantAccess<'de> {
    value: Option<ValueDeserializer<'de>>,
}

impl<'de> de::VariantAccess<'de> for VariantAccess<'de> {
    type Error = JsonError;

    fn unit_variant(self) -> Result<(), JsonError> {
        match self.value {
            // Tolerate `{"Variant": null}` for a unit variant, as serde_json
            // does.
            Some(value) => de::Deserialize::deserialize(value),
            None => Ok(()),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, JsonError>
    where
        T: DeserializeSeed<'de>,
    {
        match self.value {
            Some(value) => seed.deserialize(value),
            None => Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"newtype variant",
            )),
        }
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value {
            Some(value) => match value.node() {
                Node::Array(slab) => value.visit_array_node(slab, visitor),
                _ => Err(value.invalid_type(&"tuple variant")),
            },
            None => Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"tuple variant",
            )),
        }
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.value {
            Some(value) => match value.node() {
                Node::Object(slab) => value.visit_object_node(slab, visitor),
                // serde_json also accepts array content for a struct variant
                // (fields taken in declaration order), same as a top-level
                // struct.
                Node::Array(slab) => value.visit_array_node(slab, visitor),
                _ => Err(value.invalid_type(&"struct variant")),
            },
            None => Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"struct variant",
            )),
        }
    }
}
