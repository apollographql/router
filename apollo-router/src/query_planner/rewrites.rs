//! Declares data structure for the "data rewrites" that the query planner can include in some `FetchNode`,
//! and implements those rewrites.
//!
//! Note that on the typescript side, the query planner currently declare the rewrites that applies
//! to "inputs" and those applying to "outputs" separatly. This is due to simplify the current
//! implementation on the typescript side (for ... reasons), but it does not simplify anything to make that
//! distinction here. All this means is that, as of this writing, some kind of rewrites will only
//! every appear on the input side, while other will only appear on outputs, but it does not hurt
//! to be future-proof by supporting all types of rewrites on both "sides".

use apollo_compiler::NodeStr;
use serde::Deserialize;
use serde::Serialize;

use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::spec::Schema;

/// Given a path, separates the last element of path and the rest of it and return them as a pair.
/// This will return `None` if the path is empty.
fn split_path_last_element(path: &Path) -> Option<(Path, &PathElement)> {
    // If we have a `last()`, then we have a `parent()` too, so unwrapping shoud be safe.
    path.last().map(|last| (path.parent().unwrap(), last))
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", tag = "kind")]
pub(crate) enum DataRewrite {
    ValueSetter(DataValueSetter),
    KeyRenamer(DataKeyRenamer),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DataValueSetter {
    pub(crate) path: Path,
    pub(crate) set_value_to: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DataKeyRenamer {
    pub(crate) path: Path,
    pub(crate) rename_key_to: NodeStr,
}

impl DataRewrite {
    fn maybe_apply(&self, schema: &Schema, data: &mut Value) {
        match self {
            DataRewrite::ValueSetter(setter) => {
                // The `path` of rewrites can only be either `Key` or `Fragment`, and so far
                // we only ever rewrite the value of fields, so the last element will be a
                // `Key` and we ignore other cases (in theory, it could be `Fragment` needs
                // to be supported someday if we ever need to rewrite full object values,
                // but that can be added then).
                if let Some((parent, PathElement::Key(k, _))) =
                    split_path_last_element(&setter.path)
                {
                    data.select_values_and_paths_mut(schema, &parent, |_path, obj| {
                        if let Some(value) = obj.get_mut(k) {
                            *value = setter.set_value_to.clone()
                        }
                    });
                }
            }
            DataRewrite::KeyRenamer(renamer) => {
                // As the name implies, this only applies to renaming "keys", so we're
                // guaranteed the last element is one and can ignore other cases.
                if let Some((parent, PathElement::Key(k, _))) =
                    split_path_last_element(&renamer.path)
                {
                    data.select_values_and_paths_mut(schema, &parent, |_path, selected| {
                        if let Some(obj) = selected.as_object_mut() {
                            if let Some(value) = obj.remove(k.as_str()) {
                                obj.insert(renamer.rename_key_to.as_str(), value);
                            }
                        }

                        if let Some(arr) = selected.as_array_mut() {
                            for item in arr {
                                if let Some(obj) = item.as_object_mut() {
                                    if let Some(value) = obj.remove(k.as_str()) {
                                        obj.insert(renamer.rename_key_to.as_str(), value);
                                    }
                                }
                            }
                        }
                    });
                }
            }
        }
    }
}

/// Modifies he provided `value` by applying any of the rewrites provided that match.
pub(crate) fn apply_rewrites(
    schema: &Schema,
    value: &mut Value,
    maybe_rewrites: &Option<Vec<DataRewrite>>,
) {
    if let Some(rewrites) = maybe_rewrites {
        for rewrite in rewrites {
            rewrite.maybe_apply(schema, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    // The schema is not used for the tests
    // but we need a valid one
    const SCHEMA: &str = r#"
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
    "#;

    #[test]
    fn test_key_renamer_object() {
        let mut data = json!({
            "data": {
                "__typename": "TestType",
                "testField__alias_0": {
                    "__typename": "TestField",
                    "field":"thisisatest"
                }
            }
        });

        let dr = DataRewrite::KeyRenamer(DataKeyRenamer {
            path: "data/testField__alias_0".into(),
            rename_key_to: "testField".into(),
        });

        dr.maybe_apply(
            &Schema::parse_test(SCHEMA, &Default::default()).unwrap(),
            &mut data,
        );

        assert_eq!(
            json! {{
                "data": {
                    "__typename": "TestType",
                    "testField": {
                        "__typename": "TestField",
                        "field":"thisisatest"
                    }
                }
            }},
            data
        );
    }

    #[test]
    fn test_key_renamer_array() {
        let mut data = json!(
            {
                "data": [{
                    "__typename": "TestType",
                    "testField__alias_0": {
                        "__typename": "TestField",
                        "field":"thisisatest"
                    }
                }]
            }
        );

        let dr = DataRewrite::KeyRenamer(DataKeyRenamer {
            path: "data/testField__alias_0".into(),
            rename_key_to: "testField".into(),
        });

        dr.maybe_apply(
            &Schema::parse_test(SCHEMA, &Default::default()).unwrap(),
            &mut data,
        );

        assert_eq!(
            json! {{
                "data": [{
                    "__typename": "TestType",
                    "testField": {
                        "__typename": "TestField",
                        "field":"thisisatest"
                    }
                }]
            }},
            data
        );
    }
}
