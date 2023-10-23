//! Performance oriented JSON manipulation.

#![allow(missing_docs)] // FIXME

use std::cmp::min;
use std::fmt;

use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Entry;
use serde_json_bytes::Map;
pub(crate) use serde_json_bytes::Value;

use crate::error::FetchError;
use crate::spec::Schema;
use crate::spec::TYPENAME;

/// A JSON object.
pub(crate) type Object = Map<ByteString, Value>;

const FRAGMENT_PREFIX: &str = "... on ";

macro_rules! extract_key_value_from_object {
    ($object:expr, $key:literal, $pattern:pat => $var:ident) => {{
        match $object.remove($key) {
            Some($pattern) => Ok(Some($var)),
            None | Some(crate::json_ext::Value::Null) => Ok(None),
            _ => Err(concat!("invalid type for key: ", $key)),
        }
    }};
    ($object:expr, $key:literal) => {{
        match $object.remove($key) {
            None | Some(crate::json_ext::Value::Null) => None,
            Some(value) => Some(value),
        }
    }};
}

macro_rules! ensure_object {
    ($value:expr) => {{
        match $value {
            crate::json_ext::Value::Object(o) => Ok(o),
            _ => Err("invalid type, expected an object"),
        }
    }};
}

#[doc(hidden)]
/// Extension trait for [`serde_json::Value`].
pub(crate) trait ValueExt {
    /// Deep merge the JSON objects, array and override the values in `&mut self` if they already
    /// exists.
    #[track_caller]
    fn deep_merge(&mut self, other: Self);

    /// Returns `true` if the values are equal and the objects are ordered the same.
    ///
    /// **Note:** this is recursive.
    fn eq_and_ordered(&self, other: &Self) -> bool;

    /// Returns `true` if the set is a subset of another, i.e., `other` contains at least all the
    /// values in `self`.
    #[track_caller]
    fn is_subset(&self, superset: &Value) -> bool;

    /// Create a `Value` by inserting a value at a subpath.
    ///
    /// This will create objects, arrays and null nodes as needed if they
    /// are not present: the resulting Value is meant to be merged with an
    /// existing one that contains those nodes.
    #[track_caller]
    fn from_path(path: &Path, value: Value) -> Value;

    /// Insert a `Value` at a `Path`
    #[track_caller]
    fn insert(&mut self, path: &Path, value: Value) -> Result<(), FetchError>;

    /// Get a `Value` from a `Path`
    #[track_caller]
    fn get_path<'a>(&'a self, schema: &Schema, path: &'a Path) -> Result<&'a Value, FetchError>;

    /// Select all values matching a `Path`.
    ///
    /// the function passed as argument will be called with the values found and their Path
    /// if it encounters an invalid value, it will ignore it and continue
    #[track_caller]
    fn select_values_and_paths<'a, F>(&'a self, schema: &Schema, path: &'a Path, f: F)
    where
        F: FnMut(&Path, &'a Value);

    /// Select all values matching a `Path`, and allows to mutate those values.
    ///
    /// The behavior of the method is otherwise the same as it's non-mutable counterpart
    #[track_caller]
    fn select_values_and_paths_mut<'a, F>(&'a mut self, schema: &Schema, path: &'a Path, f: F)
    where
        F: FnMut(&Path, &'a mut Value);

    #[track_caller]
    fn is_valid_float_input(&self) -> bool;

    #[track_caller]
    fn is_valid_int_input(&self) -> bool;

    #[track_caller]
    fn is_valid_id_input(&self) -> bool;

    /// Returns whether this value is an object that matches the provided type.
    ///
    /// More precisely, this checks that this value is an object, looks at
    /// its `__typename` field and checks if that  `__typename` is either
    /// `maybe_type` or a subtype of it.
    ///
    /// If the value is not an object, this will always return `false`, but
    /// if the value is an object with no `__typename` field, then we default
    /// to `true` (meaning that the absences of way to check, we assume the
    /// value is of your expected type).
    ///
    /// TODO: in theory, this later default behaviour shouldn't matter since
    /// we should avoid calling this in cases where the `__typename` is
    /// unknown, but it is currently *relied* on due to some not-quite-right
    /// behaviour. See the comment in `ExecutionService.call` around the call
    /// to `select_values_and_paths` for details (the later relies on this
    /// function to handle `PathElement::Fragment`).
    #[track_caller]
    fn is_object_of_type(&self, schema: &Schema, maybe_type: &str) -> bool;
}

impl ValueExt for Value {
    fn deep_merge(&mut self, other: Self) {
        match (self, other) {
            (Value::Object(a), Value::Object(b)) => {
                for (key, value) in b.into_iter() {
                    match a.entry(key) {
                        Entry::Vacant(e) => {
                            e.insert(value);
                        }
                        Entry::Occupied(e) => {
                            e.into_mut().deep_merge(value);
                        }
                    }
                }
            }
            (Value::Array(a), Value::Array(mut b)) => {
                for (b_value, a_value) in b.drain(..min(a.len(), b.len())).zip(a.iter_mut()) {
                    a_value.deep_merge(b_value);
                }

                a.extend(b);
            }
            (_, Value::Null) => {}
            (Value::Object(_), Value::Array(_)) => {
                failfast_debug!("trying to replace an object with an array");
            }
            (Value::Array(_), Value::Object(_)) => {
                failfast_debug!("trying to replace an array with an object");
            }
            (a, b) => {
                if b != Value::Null {
                    *a = b;
                }
            }
        }
    }

    fn eq_and_ordered(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Object(a), Value::Object(b)) => {
                let mut it_a = a.iter();
                let mut it_b = b.iter();

                loop {
                    match (it_a.next(), it_b.next()) {
                        (Some(_), None) | (None, Some(_)) => break false,
                        (None, None) => break true,
                        (Some((field_a, value_a)), Some((field_b, value_b)))
                            if field_a == field_b && ValueExt::eq_and_ordered(value_a, value_b) =>
                        {
                            continue
                        }
                        (Some(_), Some(_)) => break false,
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                let mut it_a = a.iter();
                let mut it_b = b.iter();

                loop {
                    match (it_a.next(), it_b.next()) {
                        (Some(_), None) | (None, Some(_)) => break false,
                        (None, None) => break true,
                        (Some(value_a), Some(value_b))
                            if ValueExt::eq_and_ordered(value_a, value_b) =>
                        {
                            continue
                        }
                        (Some(_), Some(_)) => break false,
                    }
                }
            }
            (a, b) => a == b,
        }
    }

    fn is_subset(&self, superset: &Value) -> bool {
        match (self, superset) {
            (Value::Object(subset), Value::Object(superset)) => {
                subset.iter().all(|(key, value)| {
                    if let Some(other) = superset.get(key) {
                        value.is_subset(other)
                    } else {
                        false
                    }
                })
            }
            (Value::Array(subset), Value::Array(superset)) => {
                subset.len() == superset.len()
                    && subset.iter().enumerate().all(|(index, value)| {
                        if let Some(other) = superset.get(index) {
                            value.is_subset(other)
                        } else {
                            false
                        }
                    })
            }
            (a, b) => a == b,
        }
    }

    #[track_caller]
    fn from_path(path: &Path, value: Value) -> Value {
        let mut res_value = Value::default();
        let mut current_node = &mut res_value;

        for p in path.iter() {
            match p {
                PathElement::Flatten => {
                    return res_value;
                }

                &PathElement::Index(index) => match current_node {
                    Value::Array(a) => {
                        for _ in 0..index {
                            a.push(Value::default());
                        }
                        a.push(Value::default());
                        current_node = a
                            .get_mut(index)
                            .expect("we just created the value at that index");
                    }
                    Value::Null => {
                        let mut a = Vec::new();
                        for _ in 0..index {
                            a.push(Value::default());
                        }
                        a.push(Value::default());

                        *current_node = Value::Array(a);
                        current_node = current_node
                            .as_array_mut()
                            .expect("current_node was just set to a Value::Array")
                            .get_mut(index)
                            .expect("we just created the value at that index");
                    }
                    other => unreachable!("unreachable node: {:?}", other),
                },
                PathElement::Key(k) => {
                    let mut m = Map::new();
                    m.insert(k.as_str(), Value::default());

                    *current_node = Value::Object(m);
                    current_node = current_node
                        .as_object_mut()
                        .expect("current_node was just set to a Value::Object")
                        .get_mut(k.as_str())
                        .expect("the value at that key was just inserted");
                }
                PathElement::Fragment(_) => {}
            }
        }

        *current_node = value;
        res_value
    }

    /// Insert a `Value` at a `Path`
    #[track_caller]
    fn insert(&mut self, path: &Path, value: Value) -> Result<(), FetchError> {
        let mut current_node = self;

        for p in path.iter() {
            match p {
                PathElement::Flatten => {
                    if current_node.is_null() {
                        let a = Vec::new();
                        *current_node = Value::Array(a);
                    } else if !current_node.is_array() {
                        return Err(FetchError::ExecutionPathNotFound {
                            reason: "expected an array".to_string(),
                        });
                    }
                }

                &PathElement::Index(index) => match current_node {
                    Value::Array(a) => {
                        // add more elements if the index is after the end
                        for _ in a.len()..index + 1 {
                            a.push(Value::default());
                        }
                        current_node = a
                            .get_mut(index)
                            .expect("we just created the value at that index");
                    }
                    Value::Null => {
                        let mut a = Vec::new();
                        for _ in 0..index + 1 {
                            a.push(Value::default());
                        }

                        *current_node = Value::Array(a);
                        current_node = current_node
                            .as_array_mut()
                            .expect("current_node was just set to a Value::Array")
                            .get_mut(index)
                            .expect("we just created the value at that index");
                    }
                    _other => {
                        return Err(FetchError::ExecutionPathNotFound {
                            reason: "expected an array".to_string(),
                        })
                    }
                },
                PathElement::Key(k) => match current_node {
                    Value::Object(o) => {
                        current_node = o
                            .get_mut(k.as_str())
                            .expect("the value at that key was just inserted");
                    }
                    Value::Null => {
                        let mut m = Map::new();
                        m.insert(k.as_str(), Value::default());

                        *current_node = Value::Object(m);
                        current_node = current_node
                            .as_object_mut()
                            .expect("current_node was just set to a Value::Object")
                            .get_mut(k.as_str())
                            .expect("the value at that key was just inserted");
                    }
                    _other => {
                        return Err(FetchError::ExecutionPathNotFound {
                            reason: "expected an object".to_string(),
                        })
                    }
                },
                PathElement::Fragment(_) => {}
            }
        }

        *current_node = value;
        Ok(())
    }

    /// Get a `Value` from a `Path`
    #[track_caller]
    fn get_path<'a>(&'a self, schema: &Schema, path: &'a Path) -> Result<&'a Value, FetchError> {
        let mut res = Err(FetchError::ExecutionPathNotFound {
            reason: "value not found".to_string(),
        });
        iterate_path(
            schema,
            &mut Path::default(),
            &path.0,
            self,
            &mut |_path, value| {
                res = Ok(value);
            },
        );
        res
    }

    #[track_caller]
    fn select_values_and_paths<'a, F>(&'a self, schema: &Schema, path: &'a Path, mut f: F)
    where
        F: FnMut(&Path, &'a Value),
    {
        iterate_path(schema, &mut Path::default(), &path.0, self, &mut f)
    }

    #[track_caller]
    fn select_values_and_paths_mut<'a, F>(&'a mut self, schema: &Schema, path: &'a Path, mut f: F)
    where
        F: FnMut(&Path, &'a mut Value),
    {
        iterate_path_mut(schema, &mut Path::default(), &path.0, self, &mut f)
    }

    #[track_caller]
    fn is_valid_id_input(&self) -> bool {
        // https://spec.graphql.org/October2021/#sec-ID.Input-Coercion
        match self {
            // Any string and integer values are accepted
            Value::String(_) => true,
            Value::Number(n) => n.is_i64() || n.is_u64(),
            _ => false,
        }
    }

    #[track_caller]
    fn is_valid_float_input(&self) -> bool {
        // https://spec.graphql.org/draft/#sec-Float.Input-Coercion
        match self {
            // When expected as an input type, both integer and float input values are accepted.
            Value::Number(n) if n.is_f64() => true,
            // Integer input values are coerced to Float by adding an empty fractional part, for example 1.0 for the integer input value 1.
            Value::Number(n) => n.is_i64(),
            // All other input values, including strings with numeric content, must raise a request error indicating an incorrect type.
            _ => false,
        }
    }

    #[track_caller]
    fn is_valid_int_input(&self) -> bool {
        // https://spec.graphql.org/June2018/#sec-Int
        // The Int scalar type represents a signed 32‐bit numeric non‐fractional value.
        // When expected as an input type, only integer input values are accepted.
        // All other input values, including strings with numeric content, must raise a query error indicating an incorrect type.
        self.as_i64().and_then(|x| i32::try_from(x).ok()).is_some()
            || self.as_u64().and_then(|x| i32::try_from(x).ok()).is_some()
    }

    #[track_caller]
    fn is_object_of_type(&self, schema: &Schema, maybe_type: &str) -> bool {
        self.is_object()
            && self
                .get(TYPENAME)
                .and_then(|v| v.as_str())
                .map_or(true, |typename| {
                    typename == maybe_type || schema.is_subtype(maybe_type, typename)
                })
    }
}

fn iterate_path<'a, F>(
    schema: &Schema,
    parent: &mut Path,
    path: &'a [PathElement],
    data: &'a Value,
    f: &mut F,
) where
    F: FnMut(&Path, &'a Value),
{
    match path.get(0) {
        None => f(parent, data),
        Some(PathElement::Flatten) => {
            if let Some(array) = data.as_array() {
                for (i, value) in array.iter().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            }
        }
        Some(PathElement::Index(i)) => {
            if let Value::Array(a) = data {
                if let Some(value) = a.get(*i) {
                    parent.push(PathElement::Index(*i));

                    iterate_path(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            }
        }
        Some(PathElement::Key(k)) => {
            if let Value::Object(o) = data {
                if let Some(value) = o.get(k.as_str()) {
                    parent.push(PathElement::Key(k.to_string()));
                    iterate_path(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            } else if let Value::Array(array) = data {
                for (i, value) in array.iter().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path(schema, parent, path, value, f);
                    parent.pop();
                }
            }
        }
        Some(PathElement::Fragment(name)) => {
            if data.is_object_of_type(schema, name) {
                // Note that (not unlike `Flatten`) we do not include the fragment in the `parent`
                // path, because we want that path to be a "pure" response path. Fragments in path
                // are used to essentially create a type-based choice in a "selection" path, but
                // `parent` is a direct path to a specific position in the value and do not need
                // fragments.
                iterate_path(schema, parent, &path[1..], data, f);
            } else if let Value::Array(array) = data {
                for (i, value) in array.iter().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path(schema, parent, path, value, f);
                    parent.pop();
                }
            }
        }
    }
}

fn iterate_path_mut<'a, F>(
    schema: &Schema,
    parent: &mut Path,
    path: &'a [PathElement],
    data: &'a mut Value,
    f: &mut F,
) where
    F: FnMut(&Path, &'a mut Value),
{
    match path.get(0) {
        None => f(parent, data),
        Some(PathElement::Flatten) => {
            if let Some(array) = data.as_array_mut() {
                for (i, value) in array.iter_mut().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path_mut(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            }
        }
        Some(PathElement::Index(i)) => {
            if let Value::Array(a) = data {
                if let Some(value) = a.get_mut(*i) {
                    parent.push(PathElement::Index(*i));
                    iterate_path_mut(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            }
        }
        Some(PathElement::Key(k)) => {
            if let Value::Object(o) = data {
                if let Some(value) = o.get_mut(k.as_str()) {
                    parent.push(PathElement::Key(k.to_string()));
                    iterate_path_mut(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            } else if let Value::Array(array) = data {
                for (i, value) in array.iter_mut().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path_mut(schema, parent, path, value, f);
                    parent.pop();
                }
            }
        }
        Some(PathElement::Fragment(name)) => {
            if data.is_object_of_type(schema, name) {
                iterate_path_mut(schema, parent, &path[1..], data, f);
            } else if let Value::Array(array) = data {
                for (i, value) in array.iter_mut().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path_mut(schema, parent, path, value, f);
                    parent.pop();
                }
            }
        }
    }
}

/// A GraphQL path element that is composes of strings or numbers.
/// e.g `/book/3/name`
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(untagged)]
pub enum PathElement {
    /// A path element that given an array will flatmap the content.
    #[serde(
        deserialize_with = "deserialize_flatten",
        serialize_with = "serialize_flatten"
    )]
    Flatten,

    /// An index path element.
    Index(usize),

    /// A fragment application
    #[serde(
        deserialize_with = "deserialize_fragment",
        serialize_with = "serialize_fragment"
    )]
    Fragment(String),

    /// A key path element.
    Key(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponsePathElement<'a> {
    /// An index path element.
    Index(usize),

    /// A key path element.
    Key(&'a str),
}

fn deserialize_flatten<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(FlattenVisitor)
}

struct FlattenVisitor;

impl<'de> serde::de::Visitor<'de> for FlattenVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a string that is '@'")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if s == "@" {
            Ok(())
        } else {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(s),
                &self,
            ))
        }
    }
}

fn serialize_flatten<S>(serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str("@")
}

fn deserialize_fragment<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(FragmentVisitor)
}

struct FragmentVisitor;

impl<'de> serde::de::Visitor<'de> for FragmentVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a string that begins with '... on '")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        s.strip_prefix(FRAGMENT_PREFIX)
            .map(|v| v.to_string())
            .ok_or_else(|| serde::de::Error::invalid_value(serde::de::Unexpected::Str(s), &self))
    }
}

fn serialize_fragment<S>(name: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(format!("{FRAGMENT_PREFIX}{name}").as_str())
}

/// A path into the result document.
///
/// This can be composed of strings and numbers
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Default, Hash)]
#[serde(transparent)]
pub struct Path(pub Vec<PathElement>);

impl Path {
    pub fn from_slice<T: AsRef<str>>(s: &[T]) -> Self {
        Self(
            s.iter()
                .map(|x| x.as_ref())
                .map(|s| {
                    if let Ok(index) = s.parse::<usize>() {
                        PathElement::Index(index)
                    } else if s == "@" {
                        PathElement::Flatten
                    } else {
                        s.strip_prefix(FRAGMENT_PREFIX).map_or_else(
                            || PathElement::Key(s.to_string()),
                            |name| PathElement::Fragment(name.to_string()),
                        )
                    }
                })
                .collect(),
        )
    }

    pub fn from_response_slice(s: &[ResponsePathElement]) -> Self {
        Self(
            s.iter()
                .map(|x| match x {
                    ResponsePathElement::Index(index) => PathElement::Index(*index),
                    ResponsePathElement::Key(s) => PathElement::Key(s.to_string()),
                })
                .collect(),
        )
    }

    pub fn iter(&self) -> impl Iterator<Item = &PathElement> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn empty() -> Path {
        Path(Default::default())
    }

    pub fn parent(&self) -> Option<Path> {
        if self.is_empty() {
            None
        } else {
            Some(Path(self.iter().take(self.len() - 1).cloned().collect()))
        }
    }

    pub fn join(&self, other: impl AsRef<Self>) -> Self {
        let other = other.as_ref();
        let mut new = Vec::with_capacity(self.len() + other.len());
        new.extend(self.iter().cloned());
        new.extend(other.iter().cloned());
        Path(new)
    }

    pub fn push(&mut self, element: PathElement) {
        self.0.push(element)
    }

    pub fn pop(&mut self) -> Option<PathElement> {
        self.0.pop()
    }

    pub fn last(&self) -> Option<&PathElement> {
        self.0.last()
    }

    pub fn last_key(&mut self) -> Option<String> {
        self.0.last().and_then(|elem| match elem {
            PathElement::Key(k) => Some(k.clone()),
            _ => None,
        })
    }

    pub fn starts_with(&self, other: &Path) -> bool {
        self.0.starts_with(&other.0[..])
    }
}

impl FromIterator<PathElement> for Path {
    fn from_iter<T: IntoIterator<Item = PathElement>>(iter: T) -> Self {
        Path(iter.into_iter().collect())
    }
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl<T> From<T> for Path
where
    T: AsRef<str>,
{
    fn from(s: T) -> Self {
        Self(
            s.as_ref()
                .split('/')
                .map(|s| {
                    if let Ok(index) = s.parse::<usize>() {
                        PathElement::Index(index)
                    } else if s == "@" {
                        PathElement::Flatten
                    } else {
                        s.strip_prefix(FRAGMENT_PREFIX).map_or_else(
                            || PathElement::Key(s.to_string()),
                            |name| PathElement::Fragment(name.to_string()),
                        )
                    }
                })
                .collect(),
        )
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for element in self.iter() {
            write!(f, "/")?;
            match element {
                PathElement::Index(index) => write!(f, "{index}")?,
                PathElement::Key(key) => write!(f, "{key}")?,
                PathElement::Flatten => write!(f, "@")?,
                PathElement::Fragment(name) => write!(f, "{FRAGMENT_PREFIX}{name}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    macro_rules! assert_is_subset {
        ($a:expr, $b:expr $(,)?) => {
            assert!($a.is_subset(&$b));
        };
    }

    macro_rules! assert_is_not_subset {
        ($a:expr, $b:expr $(,)?) => {
            assert!(!$a.is_subset(&$b));
        };
    }

    /// Functions that walk on path needs a schema to handle potential fragment (type conditions) in
    /// the path, and so we use the following simple schema for tests. Note however that tests that
    /// don't use fragments in the path essentially ignore this schema.
    fn test_schema() -> Schema {
        Schema::parse_test(
            r#"
           schema
             @core(feature: "https://specs.apollo.dev/core/v0.1"),
             @core(feature: "https://specs.apollo.dev/join/v0.1")
           {
             query: Query
           }
           directive @core(feature: String!) repeatable on SCHEMA
           directive @join__graph(name: String!, url: String!) on ENUM_VALUE

           enum join__Graph {
               FAKE @join__graph(name:"fake" url: "http://localhost:4001/fake")
           }

           type Query {
             i: [I]
           }

           interface I {
             x: Int
           }

           type A implements I {
             x: Int
           }

           type B {
             y: Int
           }
        "#,
            &Default::default(),
        )
        .unwrap()
    }

    fn select_values<'a>(
        schema: &Schema,
        path: &'a Path,
        data: &'a Value,
    ) -> Result<Vec<&'a Value>, FetchError> {
        let mut v = Vec::new();
        data.select_values_and_paths(schema, path, |_path, value| {
            v.push(value);
        });
        Ok(v)
    }

    #[test]
    fn test_get_at_path() {
        let schema = test_schema();
        let json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/1/prop1");
        let result = select_values(&schema, &path, &json).unwrap();
        assert_eq!(result, vec![&Value::Number(2.into())]);
    }

    #[test]
    fn test_get_at_path_flatmap() {
        let schema = test_schema();
        let json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/@");
        let result = select_values(&schema, &path, &json).unwrap();
        assert_eq!(result, vec![&json!({"prop1":1}), &json!({"prop1":2})]);
    }

    #[test]
    fn test_get_at_path_flatmap_nested() {
        let schema = test_schema();
        let json = json!({
            "obj": {
                "arr": [
                    {
                        "prop1": [
                            {"prop2": {"prop3": 1}, "prop4": -1},
                            {"prop2": {"prop3": 2}, "prop4": -2},
                        ],
                    },
                    {
                        "prop1": [
                            {"prop2": {"prop3": 3}, "prop4": -3},
                            {"prop2": {"prop3": 4}, "prop4": -4},
                        ],
                    },
                ],
            },
        });
        let path = Path::from("obj/arr/@/prop1/@/prop2");
        let result = select_values(&schema, &path, &json).unwrap();
        assert_eq!(
            result,
            vec![
                &json!({"prop3":1}),
                &json!({"prop3":2}),
                &json!({"prop3":3}),
                &json!({"prop3":4}),
            ],
        );
    }

    #[test]
    fn test_deep_merge() {
        let mut json = json!({"obj":{"arr":[{"prop1":1},{"prop2":2}]}});
        json.deep_merge(json!({"obj":{"arr":[{"prop1":2,"prop3":3},{"prop4":4}]}}));
        assert_eq!(
            json,
            json!({"obj":{"arr":[{"prop1":2, "prop3":3},{"prop2":2, "prop4":4}]}})
        );
    }

    #[test]
    fn test_is_subset_eq() {
        assert_is_subset!(
            json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
            json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
        );
    }

    #[test]
    fn test_is_subset_missing_pop() {
        assert_is_subset!(
            json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
            json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}}),
        );
    }

    #[test]
    fn test_is_subset_array_lengths_differ() {
        assert_is_not_subset!(
            json!({"obj":{"arr":[{"prop1":1}]}}),
            json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}}),
        );
    }

    #[test]
    fn test_is_subset_extra_prop() {
        assert_is_not_subset!(
            json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}}),
            json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
        );
    }

    #[test]
    fn eq_and_ordered() {
        // test not objects
        assert!(json!([1, 2, 3]).eq_and_ordered(&json!([1, 2, 3])));
        assert!(!json!([1, 3, 2]).eq_and_ordered(&json!([1, 2, 3])));

        // test objects not nested
        assert!(json!({"foo":1,"bar":2}).eq_and_ordered(&json!({"foo":1,"bar":2})));
        assert!(!json!({"foo":1,"bar":2}).eq_and_ordered(&json!({"foo":1,"bar":3})));
        assert!(!json!({"foo":1,"bar":2}).eq_and_ordered(&json!({"foo":1,"bar":2,"baz":3})));
        assert!(!json!({"foo":1,"bar":2,"baz":3}).eq_and_ordered(&json!({"foo":1,"bar":2})));
        assert!(!json!({"bar":2,"foo":1}).eq_and_ordered(&json!({"foo":1,"bar":2})));

        // test objects nested
        assert!(json!({"baz":{"foo":1,"bar":2}}).eq_and_ordered(&json!({"baz":{"foo":1,"bar":2}})));
        assert!(!json!({"baz":{"bar":2,"foo":1}}).eq_and_ordered(&json!({"baz":{"foo":1,"bar":2}})));
        assert!(!json!([1,{"bar":2,"foo":1},2]).eq_and_ordered(&json!([1,{"foo":1,"bar":2},2])));
    }

    #[test]
    fn test_from_path() {
        let json = json!([{"prop1":1},{"prop1":2}]);
        let path = Path::from("obj/arr");
        let result = Value::from_path(&path, json);
        assert_eq!(result, json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}}));
    }

    #[test]
    fn test_from_path_index() {
        let json = json!({"prop1":1});
        let path = Path::from("obj/arr/1");
        let result = Value::from_path(&path, json);
        assert_eq!(result, json!({"obj":{"arr":[null, {"prop1":1}]}}));
    }

    #[test]
    fn test_from_path_flatten() {
        let json = json!({"prop1":1});
        let path = Path::from("obj/arr/@/obj2");
        let result = Value::from_path(&path, json);
        assert_eq!(result, json!({"obj":{"arr":null}}));
    }

    #[test]
    fn test_is_object_of_type() {
        let schema = test_schema();

        // Basic matching
        assert!(json!({ "__typename": "A", "x": "42"}).is_object_of_type(&schema, "A"));

        // Matching with subtyping
        assert!(json!({ "__typename": "A", "x": "42"}).is_object_of_type(&schema, "I"));

        // Matching when missing __typename (see comment on the method declaration).
        assert!(json!({ "x": "42"}).is_object_of_type(&schema, "A"));

        // Non-matching because not an object
        assert!(!json!([{ "__typename": "A", "x": "42"}]).is_object_of_type(&schema, "A"));
        assert!(!json!("foo").is_object_of_type(&schema, "I"));
        assert!(!json!(42).is_object_of_type(&schema, "I"));

        // Non-matching because not of the asked type.
        assert!(!json!({ "__typename": "B", "y": "42"}).is_object_of_type(&schema, "A"));
        assert!(!json!({ "__typename": "B", "y": "42"}).is_object_of_type(&schema, "I"));
    }

    #[test]
    fn test_get_at_path_with_conditions() {
        let schema = test_schema();
        let json = json!({
            "i": [
                {
                    "__typename": "A",
                    "x": 0,
                },
                {
                    "__typename": "B",
                    "y": 1,
                },
                {
                    "__typename": "B",
                    "y": 2,
                },
                {
                    "__typename": "A",
                    "x": 3,
                },
            ],
        });
        let path = Path::from("i/... on A");
        let result = select_values(&schema, &path, &json).unwrap();
        assert_eq!(
            result,
            vec![
                &json!({
                    "__typename": "A",
                    "x": 0,
                }),
                &json!({
                    "__typename": "A",
                    "x": 3,
                }),
            ],
        );
    }

    #[test]
    fn path_serde_json() {
        let path: Path = serde_json::from_str(
            r#"[
            "k",
            "... on T",
            "@",
            "arr",
            3
        ]"#,
        )
        .unwrap();
        assert_eq!(
            path.0,
            vec![
                PathElement::Key("k".to_string()),
                PathElement::Fragment("T".to_string()),
                PathElement::Flatten,
                PathElement::Key("arr".to_string()),
                PathElement::Index(3),
            ]
        );

        assert_eq!(
            serde_json::to_string(&path).unwrap(),
            "[\"k\",\"... on T\",\"@\",\"arr\",3]",
        );
    }
}
