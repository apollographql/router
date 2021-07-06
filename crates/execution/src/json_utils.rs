use serde_json::map::Entry;
use serde_json::Value;

use crate::{Path, PathElement};

pub(crate) fn deep_merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in b.iter() {
                match a.entry(key) {
                    Entry::Vacant(e) => {
                        e.insert(value.to_owned());
                    }
                    Entry::Occupied(e) => {
                        deep_merge(e.into_mut(), value);
                    }
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (index, value) in a.iter_mut().enumerate() {
                if let Some(b) = b.get(index) {
                    deep_merge(value, b);
                }
            }
        }
        (_, _) => {}
    }
}

#[allow(unused)]
pub(crate) fn is_subset(subset: &Value, superset: &Value) -> bool {
    match (subset, superset) {
        (Value::Object(subset), Value::Object(superset)) => subset.iter().all(|(key, value)| {
            if let Some(other) = superset.get(key) {
                is_subset(value, other)
            } else {
                false
            }
        }),
        (Value::Array(subset), Value::Array(superset)) => {
            subset.len() == superset.len()
                && subset.iter().enumerate().all(|(index, value)| {
                    if let Some(other) = superset.get(index) {
                        is_subset(value, other)
                    } else {
                        false
                    }
                })
        }
        (Value::String(subset), Value::String(superset)) => subset == superset,
        (Value::Number(subset), Value::Number(superset)) => subset == superset,
        (Value::Bool(subset), Value::Bool(superset)) => subset == superset,

        (Value::Null, Value::Null) => true,
        (_, _) => false,
    }
}

pub(crate) trait JsonUtils {
    /// Get a reference to the value at a particular path.
    /// Note that a flatmap path element will return an array if that is the value at that path.
    /// It does not actually do any flatmapping, which is instead handled by Traverser::stream.
    fn get_at_path(&self, path: &Path) -> Option<&Value>;

    /// Get a reference to the value at a particular path.
    /// Note that a flatmap path element will return an array if that is the value at that path.
    /// It does not actually do any flatmapping, which is instead handled by Traverser::stream.
    fn get_at_path_mut(&mut self, path: &Path) -> Option<&mut Value>;
}
impl JsonUtils for Option<Value> {
    fn get_at_path(&self, path: &Path) -> Option<&Value> {
        let mut current = self.as_ref();
        for path_element in &path.path {
            current = match (path_element, current) {
                (PathElement::Index(index), Some(Value::Array(array))) => array.get(*index),
                (PathElement::Key(key), Some(Value::Object(object))) => object.get(key.as_str()),
                (PathElement::Flatmap, Some(array)) if { array.is_array() } => Some(array),
                (_, _) => None,
            }
        }
        current
    }

    fn get_at_path_mut(&mut self, path: &Path) -> Option<&mut Value> {
        let mut current = self.as_mut();
        for path_element in &path.path {
            current = match (path_element, current) {
                (PathElement::Index(index), Some(Value::Array(array))) => array.get_mut(*index),
                (PathElement::Key(key), Some(Value::Object(object))) => {
                    object.get_mut(key.as_str())
                }
                (PathElement::Flatmap, Some(array)) if { array.is_array() } => Some(array),
                (_, _) => None,
            }
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serde_json::Value;

    use crate::json_utils::{deep_merge, is_subset, JsonUtils};
    use crate::Path;

    #[test]
    fn test_get_at_path() {
        let mut json = Some(json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}}));
        let path = Path::parse("obj/arr/1/prop1".into());
        let result = json.get_at_path(&path);
        assert_eq!(result, Some(&Value::Number(2.into())));
        let result_mut = json.get_at_path_mut(&path);
        assert_eq!(result_mut, Some(&mut Value::Number(2.into())));
    }

    #[test]
    fn test_get_at_path_flatmap() {
        let mut json = Some(json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}}));
        let path = Path::parse("obj/arr/@".into());
        let result = json.get_at_path(&path);
        assert_eq!(result, Some(&json!([{"prop1":1},{"prop1":2}])));
        let result_mut = json.get_at_path_mut(&path);
        assert_eq!(result_mut, Some(&mut json!([{"prop1":1},{"prop1":2}])));
    }

    #[test]
    fn test_deep_merge() {
        let mut json = &mut json!({"obj":{"arr":[{"prop1":1},{"prop2":2}]}});
        deep_merge(
            &mut json,
            &json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}}),
        );
        assert_eq!(
            json,
            &mut json!({"obj":{"arr":[{"prop1":1, "prop3":3},{"prop2":2, "prop4":4}]}})
        );
    }

    #[test]
    fn test_is_subset_eq() {
        assert!(is_subset(
            &json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
            &json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
        ));
    }

    #[test]
    fn test_is_subset_missing_pop() {
        assert!(is_subset(
            &json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}}),
            &json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}})
        ));
    }

    #[test]
    fn test_is_subset_array_lengths_differ() {
        assert!(!is_subset(
            &json!({"obj":{"arr":[{"prop1":1}]}}),
            &json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}})
        ));
    }

    #[test]
    fn test_is_subset_extra_prop() {
        assert!(!is_subset(
            &json!({"obj":{"arr":[{"prop1":1,"prop3":3},{"prop4":4}]}}),
            &json!({"obj":{"arr":[{"prop1":1},{"prop4":4}]}})
        ));
    }
}
