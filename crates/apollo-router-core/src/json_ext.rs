use crate::prelude::graphql::*;
use serde::{Deserialize, Serialize};
use serde_json::map::Entry;
use serde_json::Map;
pub use serde_json::Value;
use std::fmt;

/// A JSON object.
pub type Object = Map<String, Value>;

/// Extension trait for [`serde_json::Value`].
pub trait ValueExt {
    /// Get a reference to the value(s) at a particular path.
    #[track_caller]
    fn get_at_path<'a, 'b>(&'a self, path: &'b Path) -> Result<Vec<&'a Self>, JsonExtError>;

    /// Get a mutable reference to the value(s) at a particular path.
    #[track_caller]
    fn get_at_path_mut<'a, 'b>(
        &'a mut self,
        path: &'b Path,
    ) -> Result<Vec<&'a mut Self>, JsonExtError>;

    /// Deep merge the JSON objects, array and override the values in `&mut self` if they already
    /// exists.
    #[track_caller]
    fn deep_merge(&mut self, other: &Self);

    /// Returns `true` if the set is a subset of another, i.e., `other` contains at least all the
    /// values in `self`.
    #[track_caller]
    fn is_subset(&self, superset: &Value) -> bool;
}

impl ValueExt for Value {
    fn get_at_path<'a, 'b>(&'a self, path: &'b Path) -> Result<Vec<&'a Self>, JsonExtError> {
        let mut current = vec![self];

        for path_element in path.iter() {
            current = match path_element {
                PathElement::Flatten => current
                    .into_iter()
                    .map(|x| x.as_array().ok_or(JsonExtError::InvalidFlatten))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
                PathElement::Index(index) if !(current.len() == 1 && current[0].is_array()) => {
                    vec![current
                        .into_iter()
                        .nth(*index)
                        .ok_or(JsonExtError::PathNotFound)?]
                }
                path_element => current
                    .into_iter()
                    .map(|value| match (path_element, value) {
                        (PathElement::Key(key), value) if value.is_object() => value
                            .as_object()
                            .unwrap()
                            .get(key)
                            .ok_or(JsonExtError::PathNotFound),
                        (PathElement::Key(_), _) => Err(JsonExtError::PathNotFound),
                        (PathElement::Index(i), value) => value
                            .as_array()
                            .ok_or(JsonExtError::PathNotFound)?
                            .get(*i)
                            .ok_or(JsonExtError::PathNotFound),
                        (PathElement::Flatten, _) => unreachable!(),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }

        Ok(current)
    }

    fn get_at_path_mut<'a, 'b>(
        &'a mut self,
        path: &'b Path,
    ) -> Result<Vec<&'a mut Self>, JsonExtError> {
        let mut current = vec![self];

        for path_element in path.iter() {
            current = match path_element {
                PathElement::Flatten => current
                    .into_iter()
                    .map(|x| x.as_array_mut().ok_or(JsonExtError::InvalidFlatten))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
                PathElement::Index(index) if !(current.len() == 1 && current[0].is_array()) => {
                    vec![current
                        .into_iter()
                        .nth(*index)
                        .ok_or(JsonExtError::PathNotFound)?]
                }
                path_element => current
                    .into_iter()
                    .map(|value| match (path_element, value) {
                        (PathElement::Key(key), value) if value.is_object() => value
                            .as_object_mut()
                            .unwrap()
                            .get_mut(key)
                            .ok_or(JsonExtError::PathNotFound),
                        (PathElement::Key(_), _) => Err(JsonExtError::PathNotFound),
                        (PathElement::Index(i), value) => value
                            .as_array_mut()
                            .ok_or(JsonExtError::PathNotFound)?
                            .get_mut(*i)
                            .ok_or(JsonExtError::PathNotFound),
                        (PathElement::Flatten, _) => unreachable!(),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }

        Ok(current)
    }

    fn deep_merge(&mut self, other: &Self) {
        match (self, other) {
            (Value::Object(a), Value::Object(b)) => {
                for (key, value) in b.iter() {
                    match a.entry(key) {
                        Entry::Vacant(e) => {
                            e.insert(value.to_owned());
                        }
                        Entry::Occupied(e) => {
                            e.into_mut().deep_merge(value);
                        }
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                for (index, value) in a.iter_mut().enumerate() {
                    if let Some(b) = b.get(index) {
                        value.deep_merge(b);
                    }
                }
            }
            (a, b) => {
                *a = b.to_owned();
            }
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
pub struct Path(Vec<PathElement>);

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
        let mut json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/1/prop1");
        let result = json.get_at_path(&path).unwrap();
        assert_eq!(result, vec![&Value::Number(2.into())]);
        let result_mut = json.get_at_path_mut(&path).unwrap();
        assert_eq!(result_mut, vec![&mut Value::Number(2.into())]);
    }

    #[test]
    fn test_get_at_path_flatmap() {
        let mut json = json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}});
        let path = Path::from("obj/arr/@");
        let result = json.get_at_path(&path).unwrap();
        assert_eq!(result, vec![&json!({"prop1":1}), &json!({"prop1":2})]);
        let result_mut = json.get_at_path_mut(&path).unwrap();
        assert_eq!(
            result_mut,
            vec![&mut json!({"prop1":1}), &mut json!({"prop1":2})]
        );
    }

    #[test]
    fn test_get_at_path_flatmap_nested() {
        let mut json = json!({
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
        let result = json.get_at_path(&path).unwrap();
        assert_eq!(
            result,
            vec![
                &json!({"prop3":1}),
                &json!({"prop3":2}),
                &json!({"prop3":3}),
                &json!({"prop3":4}),
            ],
        );
        let result_mut = json.get_at_path_mut(&path).unwrap();
        assert_eq!(
            result_mut,
            vec![
                &mut json!({"prop3":1}),
                &mut json!({"prop3":2}),
                &mut json!({"prop3":3}),
                &mut json!({"prop3":4}),
            ],
        );
    }

    #[test]
    fn test_deep_merge() {
        let mut json = json!({"obj":{"arr":[{"prop1":1},{"prop2":2}]}});
        json.deep_merge(&json!({"obj":{"arr":[{"prop1":2,"prop3":3},{"prop4":4}]}}));
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
}
