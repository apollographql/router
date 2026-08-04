//! Performance oriented JSON manipulation.

#![allow(missing_docs)] // FIXME

use std::fmt;

use apollo_json::DocumentBuilder;
use apollo_json::JsonKind;
use apollo_json::NewValue;
use apollo_json::ValueMut;
pub(crate) use apollo_json::Value;
use num_traits::ToPrimitive;
use once_cell::sync::Lazy;
use regex::Captures;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;

use crate::error::FetchError;
use crate::spec::Schema;
use crate::spec::TYPENAME;

/// A JSON object, spelled with key and value type arguments so that
/// `buildstructor` recognizes a constructor parameter of this type as a map and
/// derives the `.extension(key, value)` / `.extensions(map)` pair from it. The
/// two arguments name what such a setter accepts — `K` its key and `V` its
/// value — and carry no data; [`Object`] is the only inhabited spelling.
pub(crate) struct ObjectMap<K, V>(Value, std::marker::PhantomData<fn() -> (K, V)>);

impl<K, V> Clone for ObjectMap<K, V> {
    fn clone(&self) -> Self {
        ObjectMap(self.0.clone(), self.1)
    }
}

/// A JSON object.
///
/// Object membership only exists inside an apollo-json document, so this
/// wraps a [`Value`] known to be object-shaped and provides a
/// `serde_json_bytes::Map`-like API for the small metadata objects
/// (extensions, variables, coprocessor payloads) it is used for. Each
/// mutation reopens a builder over a detached copy of the current value
/// and reseals it, which is cheap at these sizes but is not the zero-copy
/// path this crate exists for — response bodies build through
/// [`DocumentBuilder`] directly instead of through `Object`.
pub(crate) type Object = ObjectMap<String, NewValue>;

/// Wraps `value` without checking its shape; the checked entry points are
/// [`Object::try_from_value`] and `From<Value>`.
fn wrap_object(value: Value) -> Object {
    ObjectMap(value, std::marker::PhantomData)
}

impl Object {
    pub(crate) fn new() -> Self {
        wrap_object(DocumentBuilder::new().seal().root_handle())
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len().unwrap_or(0)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn get(&self, key: &str) -> Option<Value> {
        self.0.get(key)
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.0.get(key).is_some()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (String, Value)> + '_ {
        self.0.object_iter()
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = String> + '_ {
        self.0.object_iter().map(|(key, _)| key)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = Value> + '_ {
        self.0.object_iter().map(|(_, value)| value)
    }

    /// Inserts `value` at `key`, returning the previous value if any.
    pub(crate) fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<NewValue>,
    ) -> Option<Value> {
        let key = key.into();
        let previous = self.0.get(&key);
        let mut builder = self.0.detach().edit();
        builder
            .set(key.as_str(), value)
            .expect("the root of a freshly detached object always accepts a key");
        self.0 = builder.seal().root_handle();
        previous
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<Value> {
        let previous = self.0.get(key)?;
        let mut builder = self.0.detach().edit();
        builder.remove(key);
        self.0 = builder.seal().root_handle();
        Some(previous)
    }

    pub(crate) fn into_value(self) -> Value {
        self.0
    }
}

impl Default for Object {
    fn default() -> Self {
        Object::new()
    }
}

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Object({})", self.0.to_string())
    }
}

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Object {}

impl From<Value> for Object {
    /// Wraps an object-shaped value. Panics if `value` is not an object —
    /// callers that cannot guarantee the shape should check `.kind()` first.
    fn from(value: Value) -> Self {
        assert_eq!(
            value.kind(),
            JsonKind::Object,
            "Object::from requires an object-shaped Value"
        );
        wrap_object(value)
    }
}

impl Serialize for Object {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(wrap_object)
    }
}

const FRAGMENT_PREFIX: &str = "... on ";

static TYPE_CONDITIONS_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\|\[(?<condition>.+?)?\]")
        .expect("this regex to check for type conditions is valid")
});

/// Extract the condition list from the regex captures.
fn extract_matched_conditions(caps: &Captures) -> TypeConditions {
    caps.name("condition")
        .map(|c| c.as_str().split(',').map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

fn split_path_element_and_type_conditions(s: &str) -> (String, Option<TypeConditions>) {
    let mut type_conditions = None;
    let path_element = TYPE_CONDITIONS_REGEX.replace(s, |caps: &Captures| {
        type_conditions = Some(extract_matched_conditions(caps));
        ""
    });
    (path_element.to_string(), type_conditions)
}

/// Converts a borrowed value into a [`NewValue`] for writing into a
/// document under construction. Containers are adopted by reference — the
/// source arena stays alive, nothing is copied — matching apollo-json's
/// own merge semantics.
fn to_new_value(value: Value) -> NewValue {
    NewValue::Node(value)
}

#[doc(hidden)]
/// Extension trait for [`Value`].
pub(crate) trait ValueExt {
    /// Deep merge the JSON objects, array and override the values in `&mut self` if they already
    /// exists.
    #[track_caller]
    fn deep_merge(&mut self, other: Self);

    /// Deep merge two JSON objects, overwriting values in `self` if it has the same key as `other`.
    /// For GraphQL response objects, this uses schema information to avoid overwriting a concrete
    /// `__typename` with an interface name.
    #[track_caller]
    fn type_aware_deep_merge(&mut self, other: Self, schema: &Schema);

    /// Returns `true` if the values are equal and the objects are ordered the same.
    ///
    /// **Note:** this is recursive.
    #[cfg(test)]
    fn eq_and_ordered(&self, other: &Self) -> bool;

    /// Returns `true` if the set is a subset of another, i.e., `other` contains at least all the
    /// values in `self`.
    #[track_caller]
    #[cfg(test)]
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
    fn get_path(&self, schema: &Schema, path: &Path) -> Result<Value, FetchError>;

    /// Select all values matching a `Path`.
    ///
    /// the function passed as argument will be called with the values found and their Path
    /// if it encounters an invalid value, it will ignore it and continue
    #[track_caller]
    fn select_values_and_paths<F>(&self, schema: &Schema, path: &Path, f: F)
    where
        F: FnMut(&Path, Value);

    /// Select all values matching a `Path`, and allows to mutate those values.
    ///
    /// The behavior of the method is otherwise the same as it's non-mutable counterpart
    #[track_caller]
    fn select_values_and_paths_mut<F>(&mut self, schema: &Schema, path: &Path, f: F)
    where
        F: FnMut(&Path, ValueMut<'_>);

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

    fn as_i32(&self) -> Option<i32>;

    /// The value as an object, or `None` when it holds any other shape.
    fn as_object(&self) -> Option<Object>;

    /// The array's elements. This materializes a `Vec` of handles;
    /// [`Value::array_iter`] walks the same elements without one.
    fn as_array(&self) -> Option<Vec<Value>>;

    fn is_object(&self) -> bool;

    fn is_array(&self) -> bool;

    fn is_string(&self) -> bool;

    fn is_number(&self) -> bool;

    fn is_boolean(&self) -> bool;

    /// The string value, owned. [`Value::as_str`] borrows from `self`, so it
    /// cannot be used on a temporary — `v.get(k).and_then(|v| v.as_str())`
    /// does not compile; this copies the string instead.
    fn as_str_owned(&self) -> Option<String>;
}

impl ValueExt for Value {
    fn deep_merge(&mut self, other: Self) {
        match (self.kind(), other.kind()) {
            (JsonKind::Object, JsonKind::Object) | (JsonKind::Array, JsonKind::Array) => {
                let mut builder = self.detach().edit();
                merge_builder_root(&mut builder, other.value());
                *self = builder.seal().root_handle();
            }
            (_, JsonKind::Null) => {}
            (JsonKind::Object, JsonKind::Array) => {
                failfast_debug!("trying to replace an object with an array");
            }
            (JsonKind::Array, JsonKind::Object) => {
                failfast_debug!("trying to replace an array with an object");
            }
            (_, _) => {
                *self = other;
            }
        }
    }

    fn type_aware_deep_merge(&mut self, other: Self, schema: &Schema) {
        match (self.kind(), other.kind()) {
            (JsonKind::Object, JsonKind::Object) | (JsonKind::Array, JsonKind::Array) => {
                let mut builder = self.detach().edit();
                type_aware_merge_builder_root(&mut builder, other.value(), schema);
                *self = builder.seal().root_handle();
            }
            (_, JsonKind::Null) => {}
            (JsonKind::Object, JsonKind::Array) => {
                failfast_debug!("trying to replace an object with an array");
            }
            (JsonKind::Array, JsonKind::Object) => {
                failfast_debug!("trying to replace an array with an object");
            }
            (_, _) => {
                *self = other;
            }
        }
    }

    #[cfg(test)]
    fn eq_and_ordered(&self, other: &Self) -> bool {
        match (self.kind(), other.kind()) {
            (JsonKind::Object, JsonKind::Object) => {
                self.len() == other.len()
                    && self
                        .object_iter()
                        .zip(other.object_iter())
                        .all(|((ka, va), (kb, vb))| ka == kb && va.eq_and_ordered(&vb))
            }
            (JsonKind::Array, JsonKind::Array) => {
                self.len() == other.len()
                    && self
                        .array_iter()
                        .zip(other.array_iter())
                        .all(|(a, b)| a.eq_and_ordered(&b))
            }
            _ => self == other,
        }
    }

    #[cfg(test)]
    fn is_subset(&self, superset: &Value) -> bool {
        match (self.kind(), superset.kind()) {
            (JsonKind::Object, JsonKind::Object) => self.object_iter().all(|(key, value)| {
                superset
                    .get(&key)
                    .is_some_and(|other| value.is_subset(&other))
            }),
            (JsonKind::Array, JsonKind::Array) => {
                self.len() == superset.len()
                    && self
                        .array_iter()
                        .zip(superset.array_iter())
                        .all(|(value, other)| value.is_subset(&other))
            }
            _ => self == superset,
        }
    }

    #[track_caller]
    fn from_path(path: &Path, value: Value) -> Value {
        let mut builder = DocumentBuilder::new();
        let mut cursor = builder.root_mut();

        for p in path.iter() {
            match p {
                // Type conditions don't matter here since we're just creating a default value.
                PathElement::Flatten(_) => {
                    return builder.seal().root_handle();
                }
                &PathElement::Index(index) => {
                    for _ in cursor.value().len().unwrap_or(0)..=index {
                        let _ = cursor.push(());
                    }
                    cursor = cursor
                        .get_mut(index)
                        .expect("we just grew the array to include that index");
                }
                // Type conditions don't matter here since we're just creating a default value.
                PathElement::Key(k, _) => {
                    cursor = cursor
                        .get_mut(k.as_str())
                        .expect("a missing key is created as an empty object");
                }
                PathElement::Fragment(_) => {}
            }
        }

        set_cursor_to(&mut cursor, value);
        builder.seal().root_handle()
    }

    #[track_caller]
    fn insert(&mut self, path: &Path, value: Value) -> Result<(), FetchError> {
        let mut builder = self.detach().edit();
        let mut cursor = builder.root_mut();
        let mut value = value;

        for p in path.iter() {
            match p {
                PathElement::Flatten(type_conditions) => {
                    value = filter_type_conditions(value, type_conditions);
                    match cursor.value().kind() {
                        JsonKind::Null => {
                            // Coerce to an empty array; the loop continues onto the
                            // next path element without descending into an index,
                            // matching the pre-apollo-json behavior.
                            let mut new_builder = DocumentBuilder::new();
                            new_builder
                                .set(0usize, ())
                                .ok();
                            let _ = new_builder;
                            set_cursor_to(&mut cursor, empty_array());
                        }
                        JsonKind::Array => {}
                        _ => {
                            return Err(FetchError::ExecutionPathNotFound {
                                reason: "expected an array".to_string(),
                            });
                        }
                    }
                }
                &PathElement::Index(index) => match cursor.value().kind() {
                    JsonKind::Array => {
                        for _ in cursor.value().len().unwrap_or(0)..=index {
                            let _ = cursor.push(());
                        }
                        cursor = cursor
                            .get_mut(index)
                            .expect("we just grew the array to include that index");
                    }
                    JsonKind::Null => {
                        set_cursor_to(&mut cursor, empty_array());
                        for _ in 0..=index {
                            let _ = cursor.push(());
                        }
                        cursor = cursor
                            .get_mut(index)
                            .expect("we just grew the array to include that index");
                    }
                    _other => {
                        return Err(FetchError::ExecutionPathNotFound {
                            reason: "expected an array".to_string(),
                        });
                    }
                },
                PathElement::Key(k, type_conditions) => {
                    value = filter_type_conditions(value, type_conditions);
                    match cursor.value().kind() {
                        JsonKind::Object => {
                            cursor = cursor
                                .get_mut(k.as_str())
                                .expect("a missing key is created as an empty object");
                        }
                        JsonKind::Null => {
                            set_cursor_to(&mut cursor, Object::new().into_value());
                            cursor = cursor
                                .get_mut(k.as_str())
                                .expect("a missing key is created as an empty object");
                        }
                        _other => {
                            return Err(FetchError::ExecutionPathNotFound {
                                reason: "expected an object".to_string(),
                            });
                        }
                    }
                }
                PathElement::Fragment(_) => {}
            }
        }

        set_cursor_to(&mut cursor, value);
        *self = builder.seal().root_handle();
        Ok(())
    }

    #[track_caller]
    fn get_path(&self, schema: &Schema, path: &Path) -> Result<Value, FetchError> {
        let mut res = Err(FetchError::ExecutionPathNotFound {
            reason: "value not found".to_string(),
        });
        iterate_path(
            schema,
            &mut Path::default(),
            &path.0,
            self.clone(),
            &mut |_path, value| {
                res = Ok(value);
            },
        );
        res
    }

    #[track_caller]
    fn select_values_and_paths<F>(&self, schema: &Schema, path: &Path, mut f: F)
    where
        F: FnMut(&Path, Value),
    {
        iterate_path(schema, &mut Path::default(), &path.0, self.clone(), &mut f)
    }

    #[track_caller]
    fn select_values_and_paths_mut<F>(&mut self, schema: &Schema, path: &Path, mut f: F)
    where
        F: FnMut(&Path, ValueMut<'_>),
    {
        let mut builder = self.detach().edit();
        let root = builder.root_mut();
        iterate_path_mut(schema, &mut Path::default(), &path.0, root, &mut f);
        *self = builder.seal().root_handle();
    }

    #[track_caller]
    fn is_valid_id_input(&self) -> bool {
        // https://spec.graphql.org/October2021/#sec-ID.Input-Coercion
        match self.kind() {
            // Any string and integer values are accepted
            JsonKind::String => true,
            JsonKind::Number => self.as_i64().is_some() || self.as_u64().is_some(),
            _ => false,
        }
    }

    #[track_caller]
    fn is_valid_float_input(&self) -> bool {
        // https://spec.graphql.org/draft/#sec-Float.Input-Coercion
        // When expected as an input type, both integer and float input values are accepted.
        // All other input values, including strings with numeric content, must raise a request
        // error indicating an incorrect type.
        self.kind() == JsonKind::Number && self.as_f64().is_some()
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
        self.kind() == JsonKind::Object
            && self
                .get(TYPENAME)
                .and_then(|v| v.as_str().map(|s| s.into_owned()))
                .is_none_or(|typename| {
                    typename == maybe_type || schema.is_subtype(maybe_type, &typename)
                })
    }

    fn as_i32(&self) -> Option<i32> {
        self.as_i64()?.to_i32()
    }

    fn as_object(&self) -> Option<Object> {
        (self.kind() == JsonKind::Object).then(|| wrap_object(self.clone()))
    }

    fn as_array(&self) -> Option<Vec<Value>> {
        (self.kind() == JsonKind::Array).then(|| self.array_iter().collect())
    }

    fn is_object(&self) -> bool {
        self.kind() == JsonKind::Object
    }

    fn is_array(&self) -> bool {
        self.kind() == JsonKind::Array
    }

    fn is_string(&self) -> bool {
        self.kind() == JsonKind::String
    }

    fn is_number(&self) -> bool {
        self.kind() == JsonKind::Number
    }

    fn is_boolean(&self) -> bool {
        self.kind() == JsonKind::Bool
    }

    fn as_str_owned(&self) -> Option<String> {
        self.as_str().map(|value| value.into_owned())
    }
}

/// A freshly built empty array.
fn empty_array() -> Value {
    let mut builder = DocumentBuilder::new();
    builder.remove(0usize);
    // `DocumentBuilder::new()` starts as an empty object; there is no
    // direct "empty array" constructor, so build one via a document that
    // parses to `[]` instead.
    apollo_json::Document::parse(b"[]".to_vec())
        .expect("`[]` is valid JSON")
        .root_handle()
}

/// Overwrites the value a cursor points at, by replacing it in its parent.
/// `ValueMut` has no way to replace itself directly (only its own
/// children), so this walks back up isn't possible from the leaf cursor
/// alone; instead every caller here builds the cursor via `child_mut` from
/// a still-reachable parent and re-navigates the last segment. Kept as a
/// free function so the two write sites (`from_path`, `insert`) share the
/// one place documenting the constraint.
fn set_cursor_to(cursor: &mut ValueMut<'_>, value: Value) {
    // `ValueMut` addresses children, not itself, so replacing "this" value
    // is expressed as clearing every existing child and re-populating from
    // `value` — equivalent to a whole-value replacement for the shapes this
    // module produces (object, array, or scalar wrapped as a single-key
    // object then unwrapped by the caller). For scalars and containers
    // alike, merging `value` into an emptied cursor reaches the same
    // result deep_merge would for a fresh target.
    while cursor.value().len().unwrap_or(0) > 0 {
        if cursor.value().kind() == JsonKind::Array {
            cursor.remove(0);
        } else {
            let Some(key) = cursor.value().member_at(0).map(|(key, _)| key.into_owned()) else {
                break;
            };
            cursor.remove(key.as_str());
        }
    }
    match value.kind() {
        JsonKind::Object => {
            for (key, child) in value.object_iter() {
                let _ = cursor.set(key.as_str(), to_new_value(child));
            }
        }
        JsonKind::Array => {
            for child in value.array_iter() {
                let _ = cursor.push(to_new_value(child));
            }
        }
        JsonKind::Null => {}
        _ => {
            // A bare scalar has no children to populate through a cursor;
            // callers addressing a scalar leaf go through the parent's
            // `set` directly instead of this helper.
        }
    }
}

fn merge_builder_root(builder: &mut DocumentBuilder, other: ValueRefLike) {
    merge_children(&mut RootTarget(builder), other);
}

fn type_aware_merge_builder_root<'a>(
    builder: &mut DocumentBuilder,
    other: ValueRefLike<'a>,
    schema: &Schema,
) {
    type_aware_merge_children(&mut RootTarget(builder), other, schema);
}

/// A read-only, allocation-free view of an apollo-json value; alias kept
/// local so the merge helpers below read the same regardless of whether
/// they came from a [`Value`] or a live cursor.
type ValueRefLike<'a> = apollo_json::ValueRef<'a>;

/// Unifies `DocumentBuilder` and `ValueMut` under the handful of
/// operations the merge walk needs, so the recursive merge logic is
/// written once instead of duplicated for the root and for nested cursors.
trait MergeTarget {
    fn peek(&self) -> apollo_json::ValueRef<'_>;
    fn descend(&mut self, segment: PathSeg<'_>) -> Option<ValueMut<'_>>;
    fn write(&mut self, segment: PathSeg<'_>, value: NewValue);
    fn append(&mut self, value: NewValue);
}

enum PathSeg<'a> {
    Key(&'a str),
    Index(usize),
}

impl<'a> From<PathSeg<'a>> for apollo_json::PathSegment<'a> {
    fn from(seg: PathSeg<'a>) -> Self {
        match seg {
            PathSeg::Key(k) => apollo_json::PathSegment::Key(k),
            PathSeg::Index(i) => apollo_json::PathSegment::Index(i),
        }
    }
}

struct RootTarget<'b>(&'b mut DocumentBuilder);

impl MergeTarget for RootTarget<'_> {
    fn peek(&self) -> apollo_json::ValueRef<'_> {
        self.0.value()
    }

    fn descend(&mut self, segment: PathSeg<'_>) -> Option<ValueMut<'_>> {
        self.0.get_mut(segment).ok()
    }

    fn write(&mut self, segment: PathSeg<'_>, value: NewValue) {
        let _ = self.0.set(segment, value);
    }

    fn append(&mut self, value: NewValue) {
        let _ = self.0.push(value);
    }
}

impl MergeTarget for ValueMut<'_> {
    fn peek(&self) -> apollo_json::ValueRef<'_> {
        self.value()
    }

    fn descend(&mut self, segment: PathSeg<'_>) -> Option<ValueMut<'_>> {
        self.child_mut(segment).ok()
    }

    fn write(&mut self, segment: PathSeg<'_>, value: NewValue) {
        let _ = self.set(segment, value);
    }

    fn append(&mut self, value: NewValue) {
        let _ = self.push(value);
    }
}

fn merge_children<T: MergeTarget>(target: &mut T, other: apollo_json::ValueRef<'_>) {
    match other.kind() {
        JsonKind::Object => {
            for (key, incoming) in other.object_iter() {
                merge_one(target, PathSeg::Key(&key), incoming);
            }
        }
        JsonKind::Array => {
            let target_len = target.peek().len().unwrap_or(0);
            for (i, incoming) in other.array_iter().enumerate() {
                if i < target_len {
                    merge_one(target, PathSeg::Index(i), incoming);
                } else {
                    target.append(to_new_value_ref(incoming));
                }
            }
        }
        _ => unreachable!("caller matched Object/Array shapes"),
    }
}

fn merge_one<T: MergeTarget>(target: &mut T, segment: PathSeg<'_>, incoming: apollo_json::ValueRef<'_>) {
    let existing_kind = match &segment {
        PathSeg::Key(key) => target.peek().get(key).map(|v| v.kind()),
        PathSeg::Index(i) => target.peek().index(*i).map(|v| v.kind()),
    };
    match (existing_kind, incoming.kind()) {
        (Some(JsonKind::Object), JsonKind::Object) | (Some(JsonKind::Array), JsonKind::Array) => {
            if let Some(mut child) = target.descend(segment) {
                merge_children(&mut child, incoming);
            }
        }
        (_, JsonKind::Null) => {}
        (Some(JsonKind::Object), JsonKind::Array) => {
            failfast_debug!("trying to replace an object with an array");
        }
        (Some(JsonKind::Array), JsonKind::Object) => {
            failfast_debug!("trying to replace an array with an object");
        }
        _ => target.write(segment, to_new_value_ref(incoming)),
    }
}

fn type_aware_merge_children<T: MergeTarget>(
    target: &mut T,
    other: apollo_json::ValueRef<'_>,
    schema: &Schema,
) {
    match other.kind() {
        JsonKind::Object => {
            for (key, incoming) in other.object_iter() {
                if key == TYPENAME
                    && incoming.kind() == JsonKind::String
                    && let Some(existing) = target.peek().get(&key)
                    && existing.kind() == JsonKind::String
                    && let (Some(type1), Some(type2)) = (existing.as_str(), incoming.as_str())
                    && schema.is_subtype(&type2, &type1)
                {
                    // type1 is a subtype of type2: keep the more specific
                    // `__typename` already present rather than overwriting it.
                    continue;
                }
                type_aware_merge_one(target, PathSeg::Key(&key), incoming, schema);
            }
        }
        JsonKind::Array => {
            let target_len = target.peek().len().unwrap_or(0);
            for (i, incoming) in other.array_iter().enumerate() {
                if i < target_len {
                    type_aware_merge_one(target, PathSeg::Index(i), incoming, schema);
                } else {
                    target.append(to_new_value_ref(incoming));
                }
            }
        }
        _ => unreachable!("caller matched Object/Array shapes"),
    }
}

fn type_aware_merge_one<T: MergeTarget>(
    target: &mut T,
    segment: PathSeg<'_>,
    incoming: apollo_json::ValueRef<'_>,
    schema: &Schema,
) {
    let existing_kind = match &segment {
        PathSeg::Key(key) => target.peek().get(key).map(|v| v.kind()),
        PathSeg::Index(i) => target.peek().index(*i).map(|v| v.kind()),
    };
    match (existing_kind, incoming.kind()) {
        (Some(JsonKind::Object), JsonKind::Object) | (Some(JsonKind::Array), JsonKind::Array) => {
            if let Some(mut child) = target.descend(segment) {
                type_aware_merge_children(&mut child, incoming, schema);
            }
        }
        (_, JsonKind::Null) => {}
        (Some(JsonKind::Object), JsonKind::Array) => {
            failfast_debug!("trying to replace an object with an array");
        }
        (Some(JsonKind::Array), JsonKind::Object) => {
            failfast_debug!("trying to replace an array with an object");
        }
        _ => target.write(segment, to_new_value_ref(incoming)),
    }
}

fn to_new_value_ref(value: apollo_json::ValueRef<'_>) -> NewValue {
    match value.kind() {
        JsonKind::Null => NewValue::Null,
        JsonKind::Bool => NewValue::Bool(value.as_bool().unwrap_or_default()),
        JsonKind::Number => value
            .as_i64()
            .map(NewValue::Int)
            .or_else(|| value.as_f64().map(NewValue::Float))
            .unwrap_or(NewValue::Null),
        JsonKind::String => NewValue::String(value.as_str().unwrap_or_default().into_owned()),
        JsonKind::Array | JsonKind::Object => NewValue::Node(value_from_ref(value)),
    }
}

/// Materializes an owned [`Value`] handle from a borrowed [`ValueRef`],
/// sharing the source arena by reference rather than copying — the same
/// adoption apollo-json's own `merge` uses for containers.
fn value_from_ref(value: apollo_json::ValueRef<'_>) -> Value {
    // `ValueRef` has no direct "upgrade to owned" method (the crate favors
    // `Value::get`/`index`/`array_iter`/`object_iter`, which already return
    // owned handles), so round-trip through a document root: build a
    // single-entry container and adopt the subtree by reference, which
    // costs one arena node rather than a deep copy.
    let mut builder = DocumentBuilder::new();
    builder
        .set("v", to_new_value_ref_leaf(value))
        .expect("fresh object root accepts any key");
    builder
        .seal()
        .root_handle()
        .get("v")
        .expect("just inserted")
}

fn to_new_value_ref_leaf(value: apollo_json::ValueRef<'_>) -> NewValue {
    match value.kind() {
        JsonKind::Null => NewValue::Null,
        JsonKind::Bool => NewValue::Bool(value.as_bool().unwrap_or_default()),
        JsonKind::Number => value
            .as_i64()
            .map(NewValue::Int)
            .or_else(|| value.as_f64().map(NewValue::Float))
            .unwrap_or(NewValue::Null),
        JsonKind::String => NewValue::String(value.as_str().unwrap_or_default().into_owned()),
        JsonKind::Array => {
            let mut builder = DocumentBuilder::new();
            builder.remove(0usize);
            let doc = apollo_json::Document::parse(b"[]".to_vec()).expect("`[]` is valid JSON");
            let mut inner = doc.edit();
            for item in value.array_iter() {
                let _ = inner.push(to_new_value_ref_leaf(item));
            }
            NewValue::Node(inner.seal().root_handle())
        }
        JsonKind::Object => {
            let mut builder = DocumentBuilder::new();
            for (key, child) in value.object_iter() {
                let _ = builder.set(key.as_ref(), to_new_value_ref_leaf(child));
            }
            NewValue::Node(builder.seal().root_handle())
        }
    }
}

fn filter_type_conditions(value: Value, type_conditions: &Option<TypeConditions>) -> Value {
    if let Some(tc) = type_conditions {
        match value.kind() {
            JsonKind::Object => {
                if let Some(type_name) = value.get("__typename").and_then(|v| v.as_str_owned())
                    && !tc.iter().any(|tc| tc.as_str() == type_name.as_str())
                {
                    return Value::default();
                }
            }
            JsonKind::Array => {
                let mut builder = DocumentBuilder::new();
                builder.remove(0usize);
                let doc = apollo_json::Document::parse(b"[]".to_vec()).expect("`[]` is valid JSON");
                let mut inner = doc.edit();
                for item in value.array_iter() {
                    let filtered = filter_type_conditions(item, type_conditions);
                    let _ = inner.push(to_new_value(filtered));
                }
                let _ = builder;
                return inner.seal().root_handle();
            }
            _ => {}
        }
    }
    value
}

fn iterate_path<F>(schema: &Schema, parent: &mut Path, path: &[PathElement], data: Value, f: &mut F)
where
    F: FnMut(&Path, Value),
{
    match path.first() {
        None => f(parent, data),
        Some(PathElement::Flatten(type_conditions)) => {
            if data.kind() == JsonKind::Array {
                for (i, value) in data.array_iter().enumerate() {
                    if let Some(tc) = type_conditions {
                        if !tc.is_empty() {
                            if value.kind() == JsonKind::Object
                                && let Some(type_name) =
                                    value.get("__typename").and_then(|v| v.as_str_owned())
                                && tc.iter().any(|tc| tc.as_str() == type_name.as_str())
                            {
                                parent.push(PathElement::Index(i));
                                iterate_path(schema, parent, &path[1..], value.clone(), f);
                                parent.pop();
                            }

                            if value.kind() == JsonKind::Array {
                                for (i, value) in value.array_iter().enumerate() {
                                    if value.kind() == JsonKind::Object
                                        && let Some(type_name) =
                                            value.get("__typename").and_then(|v| v.as_str_owned())
                                        && tc.iter().any(|tc| tc.as_str() == type_name.as_str())
                                    {
                                        parent.push(PathElement::Index(i));
                                        iterate_path(schema, parent, &path[1..], value, f);
                                        parent.pop();
                                    }
                                }
                            }
                        }
                    } else {
                        parent.push(PathElement::Index(i));
                        iterate_path(schema, parent, &path[1..], value, f);
                        parent.pop();
                    }
                }
            }
        }
        Some(PathElement::Index(i)) => {
            if data.kind() == JsonKind::Array
                && let Some(value) = data.index(*i)
            {
                parent.push(PathElement::Index(*i));
                iterate_path(schema, parent, &path[1..], value, f);
                parent.pop();
            }
        }
        Some(PathElement::Key(k, type_conditions)) => {
            if let Some(tc) = type_conditions {
                if !tc.is_empty() {
                    if data.kind() == JsonKind::Object {
                        if let Some(value) = data.get(k.as_str())
                            && let Some(type_name) =
                                value.get("__typename").and_then(|v| v.as_str_owned())
                            && tc.iter().any(|tc| tc.as_str() == type_name.as_str())
                        {
                            parent.push(PathElement::Key(k.to_string(), None));
                            iterate_path(schema, parent, &path[1..], value, f);
                            parent.pop();
                        }
                    } else if data.kind() == JsonKind::Array {
                        for (i, value) in data.array_iter().enumerate() {
                            if value.kind() == JsonKind::Object
                                && let Some(type_name) =
                                    value.get("__typename").and_then(|v| v.as_str_owned())
                                && tc.iter().any(|tc| tc.as_str() == type_name.as_str())
                            {
                                parent.push(PathElement::Index(i));
                                iterate_path(schema, parent, path, value, f);
                                parent.pop();
                            }
                        }
                    }
                }
            } else if data.kind() == JsonKind::Object {
                if let Some(value) = data.get(k.as_str()) {
                    parent.push(PathElement::Key(k.to_string(), None));
                    iterate_path(schema, parent, &path[1..], value, f);
                    parent.pop();
                }
            } else if data.kind() == JsonKind::Array {
                for (i, value) in data.array_iter().enumerate() {
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
            } else if data.kind() == JsonKind::Array {
                for (i, value) in data.array_iter().enumerate() {
                    parent.push(PathElement::Index(i));
                    iterate_path(schema, parent, path, value, f);
                    parent.pop();
                }
            }
        }
    }
}

fn iterate_path_mut<F>(
    schema: &Schema,
    parent: &mut Path,
    path: &[PathElement],
    mut data: ValueMut<'_>,
    f: &mut F,
) where
    F: FnMut(&Path, ValueMut<'_>),
{
    match path.first() {
        None => f(parent, data),
        Some(PathElement::Flatten(type_conditions)) => {
            if data.value().kind() == JsonKind::Array {
                let len = data.value().len().unwrap_or(0);
                for i in 0..len {
                    let matches = match type_conditions {
                        Some(tc) if !tc.is_empty() => data
                            .value()
                            .index(i)
                            .is_some_and(|value| value_matches_type_conditions(value, tc)),
                        Some(_) => false,
                        None => true,
                    };
                    if matches && let Ok(child) = data.child_mut(i) {
                        parent.push(PathElement::Index(i));
                        iterate_path_mut(schema, parent, &path[1..], child, f);
                        parent.pop();
                    }
                }
            }
        }
        Some(PathElement::Index(i)) => {
            if data.value().kind() == JsonKind::Array
                && let Ok(child) = data.child_mut(*i)
            {
                parent.push(PathElement::Index(*i));
                iterate_path_mut(schema, parent, &path[1..], child, f);
                parent.pop();
            }
        }
        Some(PathElement::Key(k, type_conditions)) => {
            if let Some(tc) = type_conditions {
                if !tc.is_empty() {
                    if data.value().kind() == JsonKind::Object {
                        let matches = data
                            .value()
                            .get(k.as_str())
                            .is_some_and(|value| value_matches_type_conditions(value, tc));
                        if matches && let Ok(child) = data.child_mut(k.as_str()) {
                            parent.push(PathElement::Key(k.to_string(), None));
                            iterate_path_mut(schema, parent, &path[1..], child, f);
                            parent.pop();
                        }
                    } else if data.value().kind() == JsonKind::Array {
                        let len = data.value().len().unwrap_or(0);
                        for i in 0..len {
                            let matches = data
                                .value()
                                .index(i)
                                .is_some_and(|value| value_matches_type_conditions(value, tc));
                            if matches && let Ok(child) = data.child_mut(i) {
                                parent.push(PathElement::Index(i));
                                iterate_path_mut(schema, parent, path, child, f);
                                parent.pop();
                            }
                        }
                    }
                }
            } else if data.value().kind() == JsonKind::Object {
                if let Ok(child) = data.child_mut(k.as_str()) {
                    parent.push(PathElement::Key(k.to_string(), None));
                    iterate_path_mut(schema, parent, &path[1..], child, f);
                    parent.pop();
                }
            } else if data.value().kind() == JsonKind::Array {
                let len = data.value().len().unwrap_or(0);
                for i in 0..len {
                    if let Ok(child) = data.child_mut(i) {
                        parent.push(PathElement::Index(i));
                        iterate_path_mut(schema, parent, path, child, f);
                        parent.pop();
                    }
                }
            }
        }
        Some(PathElement::Fragment(name)) => {
            if is_value_ref_object_of_type(data.value(), schema, name) {
                iterate_path_mut(schema, parent, &path[1..], data, f);
            } else if data.value().kind() == JsonKind::Array {
                let len = data.value().len().unwrap_or(0);
                for i in 0..len {
                    if let Ok(child) = data.child_mut(i) {
                        parent.push(PathElement::Index(i));
                        iterate_path_mut(schema, parent, path, child, f);
                        parent.pop();
                    }
                }
            }
        }
    }
}

fn value_matches_type_conditions(value: apollo_json::ValueRef<'_>, tc: &[String]) -> bool {
    value.kind() == JsonKind::Object
        && value
            .get("__typename")
            .and_then(|v| v.as_str())
            .is_some_and(|type_name| tc.iter().any(|tc| tc.as_str() == type_name.as_ref()))
}

fn is_value_ref_object_of_type(value: apollo_json::ValueRef<'_>, schema: &Schema, maybe_type: &str) -> bool {
    value.kind() == JsonKind::Object
        && value
            .get(TYPENAME)
            .and_then(|v| v.as_str().map(|s| s.into_owned()))
            .is_none_or(|typename| typename == maybe_type || schema.is_subtype(maybe_type, &typename))
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
    Flatten(Option<TypeConditions>),

    /// An index path element.
    Index(usize),

    /// A fragment application
    #[serde(
        deserialize_with = "deserialize_fragment",
        serialize_with = "serialize_fragment"
    )]
    Fragment(String),

    /// A key path element.
    #[serde(deserialize_with = "deserialize_key", serialize_with = "serialize_key")]
    Key(String, Option<TypeConditions>),
}

type TypeConditions = Vec<String>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponsePathElement<'a> {
    /// An index path element.
    Index(usize),

    /// A key path element.
    Key(&'a str),
}

fn deserialize_flatten<'de, D>(deserializer: D) -> Result<Option<TypeConditions>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(FlattenVisitor)
}

struct FlattenVisitor;

impl serde::de::Visitor<'_> for FlattenVisitor {
    type Value = Option<TypeConditions>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "a string that is '@', potentially followed by type conditions"
        )
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let (path_element, type_conditions) = split_path_element_and_type_conditions(s);
        if path_element == "@" {
            Ok(type_conditions)
        } else {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(s),
                &self,
            ))
        }
    }
}

fn serialize_flatten<S>(
    type_conditions: &Option<TypeConditions>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let tc_string = if let Some(c) = type_conditions {
        format!("|[{}]", c.join(","))
    } else {
        "".to_string()
    };
    let res = format!("@{tc_string}");
    serializer.serialize_str(res.as_str())
}

fn deserialize_key<'de, D>(deserializer: D) -> Result<(String, Option<TypeConditions>), D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(KeyVisitor)
}

struct KeyVisitor;

impl serde::de::Visitor<'_> for KeyVisitor {
    type Value = (String, Option<TypeConditions>);

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "a string, potentially followed by type conditions"
        )
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(split_path_element_and_type_conditions(s))
    }
}

fn serialize_key<S>(
    key: &String,
    type_conditions: &Option<TypeConditions>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let tc_string = if let Some(c) = type_conditions {
        format!("|[{}]", c.join(","))
    } else {
        "".to_string()
    };
    let res = format!("{key}{tc_string}");
    serializer.serialize_str(res.as_str())
}

fn deserialize_fragment<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(FragmentVisitor)
}

struct FragmentVisitor;

impl serde::de::Visitor<'_> for FragmentVisitor {
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

fn flatten_from_str(s: &str) -> Result<PathElement, String> {
    let (path_element, type_conditions) = split_path_element_and_type_conditions(s);
    if path_element != "@" {
        return Err("invalid flatten".to_string());
    }
    Ok(PathElement::Flatten(type_conditions))
}

fn key_from_str(s: &str) -> Result<PathElement, String> {
    let (key, type_conditions) = split_path_element_and_type_conditions(s);
    Ok(PathElement::Key(key, type_conditions))
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
                    } else if s.contains('@') {
                        flatten_from_str(s).unwrap_or(PathElement::Flatten(None))
                    } else {
                        s.strip_prefix(FRAGMENT_PREFIX).map_or_else(
                            || key_from_str(s).unwrap_or(PathElement::Key(s.to_string(), None)),
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
                    ResponsePathElement::Key(s) => PathElement::Key(s.to_string(), None),
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
            PathElement::Key(key, type_conditions) => {
                let mut tc = String::new();
                if let Some(c) = type_conditions {
                    tc = format!("|[{}]", c.join(","));
                };
                Some(format!("{key}{tc}"))
            }
            _ => None,
        })
    }

    pub fn starts_with(&self, other: &Path) -> bool {
        self.0.starts_with(&other.0[..])
    }

    // Removes the empty key if at root (used for TypedConditions)
    pub fn remove_empty_key_root(&self) -> Self {
        if let Some(PathElement::Key(k, type_conditions)) = self.0.first()
            && k.is_empty()
            && type_conditions.is_none()
        {
            return Path(self.iter().skip(1).cloned().collect());
        }

        self.clone()
    }

    // Checks whether self and other are equal if PathElement::Flatten and PathElement::Index are
    // treated as equal
    pub fn equal_if_flattened(&self, other: &Path) -> bool {
        if self.len() != other.len() {
            return false;
        }

        for (elem1, elem2) in self.iter().zip(other.iter()) {
            let equal_elements = match (elem1, elem2) {
                (PathElement::Index(_), PathElement::Flatten(_)) => true,
                (PathElement::Flatten(_), PathElement::Index(_)) => true,
                (elem1, elem2) => elem1 == elem2,
            };
            if !equal_elements {
                return false;
            }
        }

        true
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
                    } else if s.contains('@') {
                        flatten_from_str(s).unwrap()
                    } else {
                        s.strip_prefix(FRAGMENT_PREFIX).map_or_else(
                            || key_from_str(s).unwrap_or(PathElement::Key(s.to_string(), None)),
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
                PathElement::Key(key, type_conditions) => {
                    write!(f, "{key}")?;
                    if let Some(c) = type_conditions {
                        write!(f, "|[{}]", c.join(","))?;
                    };
                }
                PathElement::Flatten(type_conditions) => {
                    write!(f, "@")?;
                    if let Some(c) = type_conditions {
                        write!(f, "|[{}]", c.join(","))?;
                    };
                }
                PathElement::Fragment(name) => {
                    write!(f, "{FRAGMENT_PREFIX}{name}")?;
                }
            }
        }
        Ok(())
    }
}

/// A document whose root is `value`. A container passed as
/// [`NewValue::Node`] is adopted by reference, sharing its arena rather than
/// being copied.
fn rooted_document(value: impl Into<NewValue>) -> apollo_json::Document {
    let mut builder = DocumentBuilder::new();
    builder
        .set_path(&[], value)
        .expect("an empty path replaces the builder root");
    builder.seal()
}

/// A standalone [`Value`] holding `value`.
fn rooted_value(value: impl Into<NewValue>) -> Value {
    rooted_document(value).root_handle()
}

/// A JSON `null`.
#[allow(dead_code)]
pub(crate) fn null() -> Value {
    Value::default()
}

/// A JSON string.
#[allow(dead_code)]
pub(crate) fn string(value: impl Into<String>) -> Value {
    rooted_value(NewValue::String(value.into()))
}

/// A JSON boolean.
#[allow(dead_code)]
pub(crate) fn bool_value(value: bool) -> Value {
    rooted_value(NewValue::Bool(value))
}

/// A JSON number holding a signed integer.
#[allow(dead_code)]
pub(crate) fn from_i64(value: i64) -> Value {
    rooted_value(NewValue::Int(value))
}

/// A JSON number holding an unsigned integer, keeping every digit of values
/// above [`i64::MAX`].
#[allow(dead_code)]
pub(crate) fn from_u64(value: u64) -> Value {
    match i64::try_from(value) {
        Ok(value) => from_i64(value),
        Err(_) => apollo_json::Document::parse(value.to_string().into_bytes())
            .expect("a decimal integer literal is valid JSON")
            .root_handle(),
    }
}

/// A JSON number holding a float. Infinity and NaN have no JSON form and come
/// back as `null`.
#[allow(dead_code)]
pub(crate) fn from_f64(value: f64) -> Value {
    if value.is_finite() {
        rooted_value(NewValue::Float(value))
    } else {
        null()
    }
}

/// A JSON array of `items`, each adopted by reference.
#[allow(dead_code)]
pub(crate) fn array(items: impl IntoIterator<Item = Value>) -> Value {
    let mut builder = empty_array().detach().edit();
    for item in items {
        builder
            .push(item)
            .expect("the builder root is an array, which accepts elements");
    }
    builder.seal().root_handle()
}

/// A JSON object of `entries`, each value adopted by reference. A repeated key
/// keeps the value that came last.
#[allow(dead_code)]
pub(crate) fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    let mut builder = DocumentBuilder::new();
    for (key, value) in entries {
        builder
            .set(key.as_str(), value)
            .expect("a fresh object root accepts any key");
    }
    builder.seal().root_handle()
}

/// Converts to `serde_json_bytes::Value` for the call sites that still speak
/// the legacy type. This walks and copies the whole value, so it belongs on
/// cold paths only — never on the response path.
#[allow(dead_code)]
pub(crate) fn to_legacy(value: &Value) -> serde_json_bytes::Value {
    rooted_document(value.clone()).to_legacy()
}

/// Converts from `serde_json_bytes::Value`. This walks and copies the whole
/// value, so it belongs on cold paths only — never on the response path.
#[allow(dead_code)]
pub(crate) fn from_legacy(value: &serde_json_bytes::Value) -> Value {
    apollo_json::Document::from_legacy(value).root_handle()
}

/// The members of `object` as a `serde_json_bytes` map, for the call sites that
/// still speak the legacy type. This walks and copies every member, so it
/// belongs on cold paths only — never on the response path.
pub(crate) fn object_to_legacy(object: &Object) -> LegacyMap {
    object
        .iter()
        .map(|(key, value)| (key.into(), to_legacy(&value)))
        .collect()
}

/// An [`Object`] holding the members of a `serde_json_bytes` map. This walks and
/// copies every member, so it belongs on cold paths only — never on the response
/// path.
pub(crate) fn object_from_legacy(map: &LegacyMap) -> Object {
    map.iter()
        .map(|(key, value)| (key.as_str().to_string(), from_legacy(value)))
        .collect()
}

/// The `serde_json_bytes` spelling of a JSON object, as the legacy call sites
/// name it.
pub(crate) type LegacyMap =
    serde_json_bytes::Map<serde_json_bytes::ByteString, serde_json_bytes::Value>;

/// Serializes any [`Serialize`] type into a [`Value`], the counterpart of
/// [`apollo_json::from_value`]. Nested [`Value`]s are adopted by reference.
///
/// # Errors
/// Returns [`apollo_json::JsonError`] when the type serializes something JSON
/// cannot hold, such as a non-string map key.
#[allow(dead_code)]
pub(crate) fn to_value<T>(value: &T) -> Result<Value, apollo_json::JsonError>
where
    T: Serialize + ?Sized,
{
    apollo_json::to_document(value).map(|document| document.root_handle())
}

impl Object {
    /// The value at `key`, inserting `default()` first when the key is absent.
    #[allow(dead_code)]
    pub(crate) fn get_or_insert_with(
        &mut self,
        key: impl Into<String>,
        default: impl FnOnce() -> Value,
    ) -> Value {
        let key = key.into();
        if let Some(existing) = self.0.get(&key) {
            return existing;
        }
        self.insert(key.clone(), default());
        self.0.get(&key).expect("just inserted at that key")
    }

    /// Inserts `value` at `key` only when the key is absent, reporting whether
    /// it inserted.
    #[allow(dead_code)]
    pub(crate) fn insert_if_absent(
        &mut self,
        key: impl Into<String>,
        value: impl Into<NewValue>,
    ) -> bool {
        let key = key.into();
        if self.0.get(&key).is_some() {
            return false;
        }
        self.insert(key, value);
        true
    }

    /// Removes `key`, returning it together with its value.
    #[allow(dead_code)]
    pub(crate) fn remove_entry(&mut self, key: &str) -> Option<(String, Value)> {
        self.remove(key).map(|value| (key.to_string(), value))
    }

    /// Wraps an object-shaped value, handing `value` back unchanged when it
    /// holds any other shape. The `From<Value>` conversion panics instead;
    /// reach for this where the shape is not guaranteed.
    #[allow(dead_code)]
    pub(crate) fn try_from_value(value: Value) -> Result<Object, Value> {
        if value.kind() == JsonKind::Object {
            Ok(wrap_object(value))
        } else {
            Err(value)
        }
    }

    /// Reorders the members by key, ascending. Nested objects keep their own
    /// order.
    #[allow(dead_code)]
    pub(crate) fn sort_keys(&mut self) {
        let mut entries: Vec<(String, Value)> = self.0.object_iter().collect();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        self.0 = object(entries);
    }
}

impl std::hash::Hash for Object {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.0, state);
    }
}

impl FromIterator<(String, Value)> for Object {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(entries: T) -> Self {
        wrap_object(object(entries))
    }
}

impl Extend<(String, Value)> for Object {
    fn extend<T: IntoIterator<Item = (String, Value)>>(&mut self, entries: T) {
        let mut builder = self.0.detach().edit();
        for (key, value) in entries {
            builder
                .set(key.as_str(), value)
                .expect("the root of a freshly detached object always accepts a key");
        }
        self.0 = builder.seal().root_handle();
    }
}

impl IntoIterator for Object {
    type Item = (String, Value);
    type IntoIter = std::vec::IntoIter<(String, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.object_iter().collect::<Vec<_>>().into_iter()
    }
}

impl IntoIterator for &Object {
    type Item = (String, Value);
    type IntoIter = std::vec::IntoIter<(String, Value)>;

    /// Collects the members into a `Vec`; [`Object::iter`] walks them without
    /// one.
    fn into_iter(self) -> Self::IntoIter {
        self.0.object_iter().collect::<Vec<_>>().into_iter()
    }
}

impl From<Object> for Value {
    fn from(object: Object) -> Self {
        object.into_value()
    }
}

/// Copies a value read out of a document under construction into a standalone
/// handle. A builder's arena is not shareable, so the subtree is copied rather
/// than adopted by reference — reach for this only where a value has to survive
/// the next edit at the cursor it came from.
pub(crate) fn owned_copy(value: apollo_json::ValueRef<'_>) -> Value {
    value_from_ref(value)
}

/// The value as an [`Object`], or a message naming the expected shape.
macro_rules! ensure_object {
    ($value:expr) => {{
        $crate::json_ext::Object::try_from_value($value)
            .map_err(|_| "invalid type, expected an object")
    }};
}

/// The value's elements, or a message naming the expected shape.
macro_rules! ensure_array {
    ($value:expr) => {{
        let value = &$value;
        $crate::json_ext::ValueExt::as_array(value).ok_or("invalid type, expected an array")
    }};
}

/// Removes `$key` from `$object`. An absent key and an explicit `null` are both
/// `None`; with a [`JsonKind`](apollo_json::JsonKind) given, any other shape is
/// an error naming the key.
macro_rules! extract_key_value_from_object {
    ($object:expr, $key:literal, $kind:expr) => {{
        match $object.remove($key) {
            None => Ok(None),
            Some(value) if value.is_null() => Ok(None),
            Some(value) if value.kind() == $kind => Ok(Some(value)),
            Some(_) => Err(concat!("invalid type for key: ", $key)),
        }
    }};
    ($object:expr, $key:literal) => {{
        match $object.remove($key) {
            None => None,
            Some(value) if value.is_null() => None,
            Some(value) => Some(value),
        }
    }};
}

/// Builds a [`Value`] from `serde_json_bytes::json!` syntax, for test fixtures.
///
/// Import it under the name the fixtures already use:
/// `use crate::json_ext::json_value as json;`
#[cfg(test)]
macro_rules! json_value {
    ($($tokens:tt)*) => {
        $crate::json_ext::from_legacy(&::serde_json_bytes::json!($($tokens)*))
    };
}

#[cfg(test)]
pub(crate) use json_value;

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an apollo-json `Value` from a `serde_json_bytes::json!` fixture,
    /// bridging the legacy macro into this crate's representation for tests.
    fn value(v: serde_json_bytes::Value) -> Value {
        apollo_json::Document::from_legacy(&v).root_handle()
    }

    macro_rules! json {
        ($($json:tt)+) => {
            value(serde_json_bytes::json!($($json)+))
        };
    }

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
        Schema::parse(
            r#"
           schema
             @link(url: "https://specs.apollo.dev/link/v1.0")
             @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
           {
             query: Query
           }

           directive @join__graph(name: String!, url: String!) on ENUM_VALUE
           directive @link( url: String as: String for: link__Purpose import: [link__Import]) repeatable on SCHEMA
           scalar link__Import

           enum join__Graph {
             FAKE @join__graph(name:"fake" url: "http://localhost:4001/fake")
           }

           enum link__Purpose {
             SECURITY
             EXECUTION
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

    fn select_values(schema: &Schema, path: &Path, data: &Value) -> Result<Vec<Value>, FetchError> {
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
        assert_eq!(result, vec![json!(2)]);
    }

    #[test]
    fn test_get_at_path_flatmap() {
        let schema = test_schema();
        let json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/@");
        let result = select_values(&schema, &path, &json).unwrap();
        assert_eq!(result, vec![json!({"prop1":1}), json!({"prop1":2})]);
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
                json!({"prop3":1}),
                json!({"prop3":2}),
                json!({"prop3":3}),
                json!({"prop3":4}),
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
    fn interface_typename_merging() {
        let schema = Schema::parse(
                r#"
            schema
                @link(url: "https://specs.apollo.dev/link/v1.0")
                @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
            {
                query: Query
            }
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE

            scalar link__Import
            scalar join__FieldSet

            enum link__Purpose {
                SECURITY
                EXECUTION
            }

            enum join__Graph {
                TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
            }

            interface I {
                s: String
            }

            type C implements I {
                s: String
            }

            type Query {
                i: I
            }
        "#,
            &Default::default(),
        )
        .expect("valid schema");
        let mut response1 = json!({
            "__typename": "C"
        });
        let response2 = json!({
            "__typename": "I",
            "s": "data"
        });

        response1.type_aware_deep_merge(response2, &schema);

        assert_eq!(
            response1,
            json!({
                "__typename": "C",
                "s": "data"
            })
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
        assert!(
            !json!({"baz":{"bar":2,"foo":1}}).eq_and_ordered(&json!({"baz":{"foo":1,"bar":2}}))
        );
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
                json!({
                    "__typename": "A",
                    "x": 0,
                }),
                json!({
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
                PathElement::Key("k".to_string(), None),
                PathElement::Fragment("T".to_string()),
                PathElement::Flatten(None),
                PathElement::Key("arr".to_string(), None),
                PathElement::Index(3),
            ]
        );

        assert_eq!(
            serde_json::to_string(&path).unwrap(),
            "[\"k\",\"... on T\",\"@\",\"arr\",3]",
        );
    }
}
