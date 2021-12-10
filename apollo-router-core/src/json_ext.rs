use crate::prelude::graphql::*;
use serde::{Deserialize, Serialize};
use serde_json::map::Entry;
use serde_json::Map;
pub use serde_json::Value;
use std::cmp::min;
use std::fmt;

/// A JSON object.
pub type Object = Map<String, Value>;

/// Extension trait for [`serde_json::Value`].
pub trait ValueExt {
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
    #[track_caller]
    fn from_path(path: &Path, value: Value) -> Value;
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
                for (b_value, a_value) in b.drain(..).zip(a.iter_mut()) {
                    a_value.deep_merge(b_value);
                }

                a.extend(b.into_iter());
            }
            (_, Value::Null) => {}
            (Value::Object(_), Value::Array(_)) => {
                failfast_debug!("trying to replace an object with an array");
            }
            (Value::Array(_), Value::Object(_)) => {
                failfast_debug!("trying to replace an array with an object");
            }
            (a, b) => {
                *a = b;
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

    /// Insert a specific value at a subpath.
    ///
    /// This will create objects, arrays and null nodes as needed if they
    /// are not present: the resulting Value is meant to be merged with an
    /// existing one that contains those nodes.
    #[track_caller]
    fn from_path(path: &Path, value: Value) -> Value {
        let mut res_value = Value::default();
        let mut current_node = &mut res_value;

        for p in path.iter() {
            match p {
                PathElement::Flatten => {
                    let a = Vec::new();
                    *current_node = Value::Array(a);
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
                    m.insert(k.to_string(), Value::default());

                    *current_node = Value::Object(m);
                    current_node = current_node
                        .as_object_mut()
                        .expect("current_node was just set to a Value::Object")
                        .get_mut(k)
                        .expect("the value at that key was just inserted");
                }
            }
        }

        *current_node = value;
        res_value
    }
}

/// selects all values matching a Path
///
/// this will return the values and their specific path in the Value:
/// a Path with a flatten node can match multiple values
pub(crate) fn select_values_and_paths<'a>(
    path: &'a Path,
    data: &'a Value,
) -> Result<Vec<(Path, &'a Value)>, FetchError> {
    let mut res = Vec::new();
    match iterate_path(&Path::default(), &path.0, data, &mut res) {
        Some(err) => Err(err),
        None => Ok(res),
    }
}

#[cfg(test)]
pub(crate) fn select_values<'a>(
    path: &'a Path,
    data: &'a Value,
) -> Result<Vec<&'a Value>, FetchError> {
    Ok(select_values_and_paths(path, data)?
        .into_iter()
        .map(|r| r.1)
        .collect())
}

fn iterate_path<'a>(
    parent: &Path,
    path: &'a [PathElement],
    data: &'a Value,
    results: &mut Vec<(Path, &'a Value)>,
) -> Option<FetchError> {
    match path.get(0) {
        None => {
            results.push((parent.clone(), data));
            None
        }
        Some(PathElement::Flatten) => match data.as_array() {
            None => Some(FetchError::ExecutionInvalidContent {
                reason: "not an array".to_string(),
            }),
            Some(array) => {
                for (i, value) in array.iter().enumerate() {
                    if let Some(err) = iterate_path(
                        &parent.join(Path::from(i.to_string())),
                        &path[1..],
                        value,
                        results,
                    ) {
                        return Some(err);
                    }
                }
                None
            }
        },
        Some(PathElement::Index(i)) => {
            if let Value::Array(a) = data {
                if let Some(value) = a.get(*i) {
                    iterate_path(
                        &parent.join(Path::from(i.to_string())),
                        &path[1..],
                        value,
                        results,
                    )
                } else {
                    Some(FetchError::ExecutionPathNotFound {
                        reason: format!("index {} not found", i),
                    })
                }
            } else {
                Some(FetchError::ExecutionInvalidContent {
                    reason: "not an array".to_string(),
                })
            }
        }
        Some(PathElement::Key(k)) => {
            if let Value::Object(o) = data {
                if let Some(value) = o.get(k) {
                    iterate_path(&parent.join(Path::from(k)), &path[1..], value, results)
                } else {
                    Some(FetchError::ExecutionPathNotFound {
                        reason: format!("key {} not found", k),
                    })
                }
            } else {
                Some(FetchError::ExecutionInvalidContent {
                    reason: "not an object".to_string(),
                })
            }
        }
    }
}

/// A GraphQL path element that is composes of strings or numbers.
/// e.g `/book/3/name`
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

    /// A key path element.
    Key(String),
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

/// A path into the result document.
///
/// This can be composed of strings and numbers
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Default)]
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
                        PathElement::Key(s.to_string())
                    }
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
            Some(Path(self.iter().cloned().take(self.len() - 1).collect()))
        }
    }

    pub fn join(&self, other: impl AsRef<Self>) -> Self {
        let other = other.as_ref();
        let mut new = Vec::with_capacity(self.len() + other.len());
        new.extend(self.iter().cloned());
        new.extend(other.iter().cloned());
        Path(new)
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
                        PathElement::Key(s.to_string())
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
                PathElement::Index(index) => write!(f, "{}", index)?,
                PathElement::Key(key) => write!(f, "{}", key)?,
                PathElement::Flatten => write!(f, "@")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn test_get_at_path() {
        let json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/1/prop1");
        let result = select_values(&path, &json).unwrap();
        assert_eq!(result, vec![&Value::Number(2.into())]);
    }

    #[test]
    fn test_get_at_path_flatmap() {
        let json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/@");
        let result = select_values(&path, &json).unwrap();
        assert_eq!(result, vec![&json!({"prop1":1}), &json!({"prop1":2})]);
    }

    #[test]
    fn test_get_at_path_flatmap_nested() {
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
        let result = select_values(&path, &json).unwrap();
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
        let path = Path::from("obj/arr/@");
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
}
