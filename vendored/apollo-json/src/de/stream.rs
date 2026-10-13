//! Streaming serde deserializer driven directly off JSON bytes.
//!
//! [`from_slice`](super::from_slice) and [`from_str`](super::from_str)
//! deserialize in a single pass over the input: visitors are fed straight
//! from the lexer and no document is materialized. Because the stream sees
//! every object entry, duplicate struct fields error exactly as they do in
//! `serde_json` — unlike the document path, where parsing collapses
//! duplicates first.
//!
//! Nesting spends the same budget the parser enforces
//! ([`DEFAULT_MAX_DEPTH`]), so the recursion through serde's data model is
//! stack-bounded by the cap. A capture (a field typed
//! [`Value`](crate::Value)) delimits its subtree in the
//! stream and parses just those bytes into a dedicated arena, so the capture
//! pins only its own slice of the input.

use std::borrow::Cow;

use serde::de::{self, DeserializeSeed, Expected, Unexpected, Visitor};

use crate::document::Value;
use crate::error::JsonError;
use crate::lex::Lexer;
use crate::options::{DEFAULT_MAX_DEPTH, ParseOptions};
use crate::utf8::ValidatedUtf8;

use super::access::MapKeyDeserializer;
use super::{
    number_unexpected, visit_f32_literal, visit_f64_literal, visit_i128_literal,
    visit_number_literal, visit_u128_literal,
};

/// Deserializes a `T` from validated input.
pub(super) fn deserialize<T>(input: ValidatedUtf8<'_>) -> Result<T, JsonError>
where
    T: de::DeserializeOwned,
{
    let mut deserializer = StreamDeserializer::new(input);
    let value = T::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

pub(super) struct StreamDeserializer<'de> {
    lex: Lexer<'de>,
    /// Remaining nesting budget; opening a container at zero aborts.
    depth: usize,
    /// Reusable container stack for skipped subtrees (`true` = object).
    skip_stack: Vec<bool>,
    /// Scratch reused across capture sub-parses (the arenas themselves stay
    /// with their captures).
    capture_buffers: crate::ParseBuffers,
}

/// Maps a capture sub-parse error back into whole-input terms: syntax
/// offsets shift by the capture's start, and the depth limit reports the
/// overall cap rather than the remaining budget the sub-parse ran under.
fn rebase_capture_error(error: JsonError, start: usize) -> JsonError {
    match error {
        JsonError::Syntax { offset, reason } => JsonError::Syntax {
            offset: offset + start,
            reason,
        },
        JsonError::DepthLimitExceeded { .. } => JsonError::DepthLimitExceeded {
            limit: DEFAULT_MAX_DEPTH,
        },
        other => other,
    }
}

impl<'de> StreamDeserializer<'de> {
    fn new(input: ValidatedUtf8<'de>) -> Self {
        StreamDeserializer {
            lex: Lexer::new(input),
            depth: DEFAULT_MAX_DEPTH,
            skip_stack: Vec::new(),
            capture_buffers: crate::ParseBuffers::new(),
        }
    }

    /// Checks nothing but whitespace remains after the root value.
    fn end(&mut self) -> Result<(), JsonError> {
        self.lex.skip_ws();
        if self.lex.at_end() {
            Ok(())
        } else {
            Err(self.lex.syntax("trailing characters after the document"))
        }
    }

    fn descend(&mut self) -> Result<(), JsonError> {
        match self.depth.checked_sub(1) {
            Some(depth) => {
                self.depth = depth;
                Ok(())
            }
            None => Err(JsonError::DepthLimitExceeded {
                limit: DEFAULT_MAX_DEPTH,
            }),
        }
    }

    fn ascend(&mut self) {
        self.depth += 1;
    }

    /// Scans the string at the cursor; escape-free content borrows the
    /// input.
    fn scan_str(&mut self) -> Result<Cow<'de, str>, JsonError> {
        let (range, escaped) = self.lex.scan_string()?;
        let raw = self.lex.input().slice(range);
        Ok(if escaped {
            Cow::Owned(crate::text::unescape(raw))
        } else {
            Cow::Borrowed(raw.as_str())
        })
    }

    /// Scans the number literal at the cursor; literals are ASCII.
    fn scan_number_str(&mut self) -> Result<&'de str, JsonError> {
        let range = self.lex.scan_number()?;
        Ok(self.lex.input().slice(range).as_str())
    }

    fn visit_string_value<V>(&mut self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        match self.scan_str()? {
            Cow::Borrowed(s) => visitor.visit_borrowed_str(s),
            Cow::Owned(s) => visitor.visit_string(s),
        }
    }

    fn visit_seq_value<V>(&mut self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.bump();
        self.descend()?;
        let (value, count) = {
            let mut access = SeqAccess {
                de: &mut *self,
                first: true,
                count: 0,
            };
            let value = visitor.visit_seq(&mut access)?;
            (value, access.count)
        };
        self.lex.skip_ws();
        match self.lex.next()? {
            b']' => {
                self.ascend();
                Ok(value)
            }
            b',' => Err(de::Error::invalid_length(count, &"fewer elements in array")),
            _ => Err(self.lex.syntax_before("expected ',' or ']'")),
        }
    }

    fn visit_map_value<V>(&mut self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.bump();
        self.descend()?;
        let (value, count) = {
            let mut access = MapAccess {
                de: &mut *self,
                first: true,
                count: 0,
            };
            let value = visitor.visit_map(&mut access)?;
            (value, access.count)
        };
        self.lex.skip_ws();
        match self.lex.next()? {
            b'}' => {
                self.ascend();
                Ok(value)
            }
            b',' => Err(de::Error::invalid_length(count, &"fewer elements in map")),
            _ => Err(self.lex.syntax_before("expected ',' or '}'")),
        }
    }

    /// The `invalid type` error for the value at the cursor: reads the value
    /// to report what was found, the visitor's expectation, and the byte
    /// offset (for strings and numbers, the offset the document path's
    /// spans would report).
    fn invalid_type(&mut self, exp: &dyn Expected) -> JsonError {
        self.lex.skip_ws();
        let mut offset = self.lex.pos();
        let peeked = match self.lex.peek() {
            Ok(byte) => byte,
            Err(error) => return error,
        };
        let string;
        let unexpected = match peeked {
            b'n' => match self.lex.expect_literal(b"null") {
                Ok(()) => Unexpected::Unit,
                Err(error) => return error,
            },
            b't' => match self.lex.expect_literal(b"true") {
                Ok(()) => Unexpected::Bool(true),
                Err(error) => return error,
            },
            b'f' => match self.lex.expect_literal(b"false") {
                Ok(()) => Unexpected::Bool(false),
                Err(error) => return error,
            },
            b'"' => {
                offset += 1;
                match self.scan_str() {
                    Ok(s) => {
                        string = s;
                        Unexpected::Str(&string)
                    }
                    Err(error) => return error,
                }
            }
            b'-' | b'0'..=b'9' => match self.scan_number_str() {
                Ok(raw) => number_unexpected(raw),
                Err(error) => return error,
            },
            b'[' => Unexpected::Seq,
            b'{' => Unexpected::Map,
            _ => return self.lex.syntax("expected a JSON value"),
        };
        JsonError::Deserialization {
            message: format!("invalid type: {unexpected}, expected {exp} at byte offset {offset}"),
        }
    }

    /// Skips (and fully validates) the value at the cursor, spending the
    /// same nesting budget as deserializing it would.
    fn skip_value(&mut self) -> Result<(), JsonError> {
        let mut stack = std::mem::take(&mut self.skip_stack);
        stack.clear();
        let result = self.skip_value_with(&mut stack);
        self.skip_stack = stack;
        result
    }

    fn skip_value_with(&mut self, stack: &mut Vec<bool>) -> Result<(), JsonError> {
        'value: loop {
            self.lex.skip_ws();
            match self.lex.peek()? {
                b'{' => {
                    self.lex.bump();
                    self.check_skip_depth(stack.len())?;
                    self.lex.skip_ws();
                    if self.lex.peek()? == b'}' {
                        self.lex.bump();
                    } else {
                        self.skip_key()?;
                        stack.push(true);
                        continue 'value;
                    }
                }
                b'[' => {
                    self.lex.bump();
                    self.check_skip_depth(stack.len())?;
                    self.lex.skip_ws();
                    if self.lex.peek()? == b']' {
                        self.lex.bump();
                    } else {
                        stack.push(false);
                        continue 'value;
                    }
                }
                b'"' => {
                    self.lex.scan_string()?;
                }
                b'-' | b'0'..=b'9' => {
                    self.lex.scan_number()?;
                }
                b't' => self.lex.expect_literal(b"true")?,
                b'f' => self.lex.expect_literal(b"false")?,
                b'n' => self.lex.expect_literal(b"null")?,
                _ => return Err(self.lex.syntax("expected a JSON value")),
            }
            // A value just completed; unwind any containers that close here.
            loop {
                let Some(&in_object) = stack.last() else {
                    return Ok(());
                };
                self.lex.skip_ws();
                match (self.lex.next()?, in_object) {
                    (b',', false) => continue 'value,
                    (b',', true) => {
                        self.lex.skip_ws();
                        self.skip_key()?;
                        continue 'value;
                    }
                    (b']', false) | (b'}', true) => {
                        stack.pop();
                    }
                    (_, false) => return Err(self.lex.syntax_before("expected ',' or ']'")),
                    (_, true) => return Err(self.lex.syntax_before("expected ',' or '}'")),
                }
            }
        }
    }

    fn skip_key(&mut self) -> Result<(), JsonError> {
        if self.lex.peek()? != b'"' {
            return Err(self.lex.syntax("expected an object key"));
        }
        self.lex.scan_string()?;
        self.lex.skip_ws();
        if self.lex.next()? != b':' {
            return Err(self.lex.syntax_before("expected ':' after object key"));
        }
        Ok(())
    }

    /// A skipped subtree at `nesting` levels below the cursor shares the
    /// deserializer's remaining budget.
    fn check_skip_depth(&self, nesting: usize) -> Result<(), JsonError> {
        if nesting + 1 > self.depth {
            Err(JsonError::DepthLimitExceeded {
                limit: DEFAULT_MAX_DEPTH,
            })
        } else {
            Ok(())
        }
    }

    /// Fulfils a capture request: parses the one value at the cursor into a
    /// dedicated arena and hands it over for the
    /// [`Value`](crate::Value) visitor to take.
    ///
    /// The capture's arena holds a copy of only the subtree's bytes, so it
    /// pins nothing beyond the subtree itself.
    fn capture<V>(&mut self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        let start = self.lex.pos();
        // The subtree may nest as deep as the remaining budget allows; the
        // arena cap applies to the capture alone.
        let options = ParseOptions::default().with_max_depth(self.depth);
        let (parsed, consumed) = crate::parse::parse_prefix(
            self.lex.input().slice(start..),
            &options,
            &mut self.capture_buffers,
        )
        .map_err(|error| rebase_capture_error(error, start))?;
        self.lex.advance(consumed);
        let Value { arena, node } = Value::rooted(parsed.arena, parsed.root);
        crate::handoff::stash(arena, node);
        let result = visitor.visit_unit();
        // The visitor takes the stash; if a foreign impl requested our
        // marker name but never collected, drop the capture rather than
        // leak it into a later deserialization.
        crate::handoff::clear();
        result
    }
}

macro_rules! deserialize_integer {
    ($method:ident) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, JsonError>
        where
            V: Visitor<'de>,
        {
            self.lex.skip_ws();
            match self.lex.peek()? {
                b'-' | b'0'..=b'9' => {
                    let raw = self.scan_number_str()?;
                    visit_number_literal(raw, visitor)
                }
                _ => Err(self.invalid_type(&visitor)),
            }
        }
    };
}

macro_rules! deserialize_wide_number {
    ($method:ident, $visit:ident) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, JsonError>
        where
            V: Visitor<'de>,
        {
            self.lex.skip_ws();
            match self.lex.peek()? {
                b'-' | b'0'..=b'9' => {
                    let raw = self.scan_number_str()?;
                    $visit(raw, visitor)
                }
                _ => Err(self.invalid_type(&visitor)),
            }
        }
    };
}

impl<'de> de::Deserializer<'de> for &mut StreamDeserializer<'de> {
    type Error = JsonError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'n' => {
                self.lex.expect_literal(b"null")?;
                visitor.visit_unit()
            }
            b't' => {
                self.lex.expect_literal(b"true")?;
                visitor.visit_bool(true)
            }
            b'f' => {
                self.lex.expect_literal(b"false")?;
                visitor.visit_bool(false)
            }
            b'"' => self.visit_string_value(visitor),
            b'-' | b'0'..=b'9' => {
                let raw = self.scan_number_str()?;
                visit_number_literal(raw, visitor)
            }
            b'[' => self.visit_seq_value(visitor),
            b'{' => self.visit_map_value(visitor),
            _ => Err(self.lex.syntax("expected a JSON value")),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        match self.lex.peek()? {
            b't' => {
                self.lex.expect_literal(b"true")?;
                visitor.visit_bool(true)
            }
            b'f' => {
                self.lex.expect_literal(b"false")?;
                visitor.visit_bool(false)
            }
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    deserialize_integer!(deserialize_i8);
    deserialize_integer!(deserialize_i16);
    deserialize_integer!(deserialize_i32);
    deserialize_integer!(deserialize_i64);
    deserialize_integer!(deserialize_u8);
    deserialize_integer!(deserialize_u16);
    deserialize_integer!(deserialize_u32);
    deserialize_integer!(deserialize_u64);
    deserialize_wide_number!(deserialize_i128, visit_i128_literal);
    deserialize_wide_number!(deserialize_u128, visit_u128_literal);
    deserialize_wide_number!(deserialize_f32, visit_f32_literal);
    deserialize_wide_number!(deserialize_f64, visit_f64_literal);

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'"' => self.visit_string_value(visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'"' => self.visit_string_value(visitor),
            b'[' => self.visit_seq_value(visitor),
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
        self.lex.skip_ws();
        if self.lex.peek()? == b'n' {
            self.lex.expect_literal(b"null")?;
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'n' => {
                self.lex.expect_literal(b"null")?;
                visitor.visit_unit()
            }
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
            return self.capture(visitor);
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'[' => self.visit_seq_value(visitor),
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
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'{' => self.visit_map_value(visitor),
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
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'{' => self.visit_map_value(visitor),
            b'[' => self.visit_seq_value(visitor),
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
        self.lex.skip_ws();
        match self.lex.peek()? {
            b'"' => {
                let variant = self.scan_str()?;
                visitor.visit_enum(EnumAccess { de: None, variant })
            }
            b'{' => {
                self.lex.bump();
                self.descend()?;
                self.lex.skip_ws();
                if self.lex.peek()? != b'"' {
                    return Err(self.lex.syntax("expected an object key"));
                }
                let variant = self.scan_str()?;
                self.lex.skip_ws();
                if self.lex.next()? != b':' {
                    return Err(self.lex.syntax_before("expected ':' after object key"));
                }
                let value = visitor.visit_enum(EnumAccess {
                    de: Some(&mut *self),
                    variant,
                })?;
                self.lex.skip_ws();
                if self.lex.next()? != b'}' {
                    return Err(self
                        .lex
                        .syntax_before("expected the enum object to hold a single entry"));
                }
                self.ascend();
                Ok(value)
            }
            _ => Err(self.invalid_type(&"string or map")),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, JsonError>
    where
        V: Visitor<'de>,
    {
        self.skip_value()?;
        visitor.visit_unit()
    }
}

struct SeqAccess<'a, 'de> {
    de: &'a mut StreamDeserializer<'de>,
    first: bool,
    count: usize,
}

impl<'de> de::SeqAccess<'de> for SeqAccess<'_, 'de> {
    type Error = JsonError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, JsonError>
    where
        T: DeserializeSeed<'de>,
    {
        self.de.lex.skip_ws();
        match self.de.lex.peek()? {
            b']' => return Ok(None),
            b',' if !self.first => {
                self.de.lex.bump();
                self.de.lex.skip_ws();
                if self.de.lex.peek()? == b']' {
                    return Err(self.de.lex.syntax("trailing comma"));
                }
            }
            _ if self.first => {}
            _ => return Err(self.de.lex.syntax("expected ',' or ']'")),
        }
        self.first = false;
        self.count += 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
}

struct MapAccess<'a, 'de> {
    de: &'a mut StreamDeserializer<'de>,
    first: bool,
    count: usize,
}

impl<'de> de::MapAccess<'de> for MapAccess<'_, 'de> {
    type Error = JsonError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, JsonError>
    where
        K: DeserializeSeed<'de>,
    {
        self.de.lex.skip_ws();
        match self.de.lex.peek()? {
            b'}' => return Ok(None),
            b',' if !self.first => {
                self.de.lex.bump();
                self.de.lex.skip_ws();
                if self.de.lex.peek()? == b'}' {
                    return Err(self.de.lex.syntax("trailing comma"));
                }
            }
            _ if self.first => {}
            _ => return Err(self.de.lex.syntax("expected ',' or '}'")),
        }
        self.first = false;
        self.count += 1;
        if self.de.lex.peek()? != b'"' {
            return Err(self.de.lex.syntax("expected an object key"));
        }
        let key = self.de.scan_str()?;
        seed.deserialize(MapKeyDeserializer::new(key)).map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, JsonError>
    where
        V: DeserializeSeed<'de>,
    {
        self.de.lex.skip_ws();
        if self.de.lex.next()? != b':' {
            return Err(self.de.lex.syntax_before("expected ':' after object key"));
        }
        seed.deserialize(&mut *self.de)
    }
}

/// Enum access over a unit-variant string or a single-entry object.
struct EnumAccess<'a, 'de> {
    /// The stream positioned on the variant's content; `None` for the
    /// bare-string (unit) form.
    de: Option<&'a mut StreamDeserializer<'de>>,
    variant: Cow<'de, str>,
}

impl<'a, 'de> de::EnumAccess<'de> for EnumAccess<'a, 'de> {
    type Error = JsonError;
    type Variant = VariantAccess<'a, 'de>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), JsonError>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(MapKeyDeserializer::new(self.variant))?;
        Ok((variant, VariantAccess { de: self.de }))
    }
}

struct VariantAccess<'a, 'de> {
    de: Option<&'a mut StreamDeserializer<'de>>,
}

impl<'de> de::VariantAccess<'de> for VariantAccess<'_, 'de> {
    type Error = JsonError;

    fn unit_variant(self) -> Result<(), JsonError> {
        match self.de {
            // Tolerate `{"Variant": null}` for a unit variant, as serde_json
            // does.
            Some(de) => de::Deserialize::deserialize(de),
            None => Ok(()),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, JsonError>
    where
        T: DeserializeSeed<'de>,
    {
        match self.de {
            Some(de) => seed.deserialize(de),
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
        match self.de {
            Some(de) => {
                de.lex.skip_ws();
                match de.lex.peek()? {
                    b'[' => de.visit_seq_value(visitor),
                    _ => Err(de.invalid_type(&"tuple variant")),
                }
            }
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
        match self.de {
            Some(de) => {
                de.lex.skip_ws();
                match de.lex.peek()? {
                    b'{' => de.visit_map_value(visitor),
                    // serde_json also accepts array content for a struct
                    // variant (fields taken in declaration order), same as a
                    // top-level struct.
                    b'[' => de.visit_seq_value(visitor),
                    _ => Err(de.invalid_type(&"struct variant")),
                }
            }
            None => Err(de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"struct variant",
            )),
        }
    }
}
