//! Declares data structure for the "data rewrites" that the query planner can include in some `FetchNode`,
//! and implements those rewrites.
//!
//! Note that on the typescript side, the query planner currently declare the rewrites that applies
//! to "inputs" and those applying to "outputs" separatly. This is due to simplify the current
//! implementation on the typescript side (for ... reasons), but it does not simplify anything to make that
//! distinction here. All this means is that, as of this writing, some kind of rewrites will only
//! every appear on the input side, while other will only appear on outputs, but it does not hurt
//! to be future-proof by supporting all types of rewrites on both "sides".

use apollo_compiler::Name;
use apollo_json::JsonKind;
use apollo_json::ValueMut;
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
    // If we have a `last()`, then we have a `parent()` too, so unwrapping should be safe.
    path.last().map(|last| (path.parent().unwrap(), last))
}

/// Moves the member at `from` to `to`, leaving a non-object or an object
/// without `from` untouched. A `to` that already exists keeps its position and
/// takes the moved value.
fn rename_member(object: &mut ValueMut<'_>, from: &str, to: &str) {
    let Some(value) = object
        .value()
        .get(from)
        .map(|member| member.to_document().root_handle())
    else {
        return;
    };
    object.remove(from);
    let _ = object.set(to, value);
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
    /// Rebuilt on deserialize: rewrites live inside the internally tagged
    /// [`PlanNode`](crate::query_planner::PlanNode) tree, which the
    /// distributed query-plan cache reads back through serde -- and serde's
    /// buffering for tagged enums cannot capture an arena value.
    #[serde(with = "crate::json_ext::value_rebuild")]
    pub(crate) set_value_to: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DataKeyRenamer {
    pub(crate) path: Path,
    pub(crate) rename_key_to: Name,
}

impl DataRewrite {
    pub(crate) fn maybe_apply(&self, schema: &Schema, data: &mut Value) {
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
                    data.select_values_and_paths_mut(schema, &parent, |_path, mut obj| {
                        if obj.value().get(k.as_str()).is_some() {
                            let _ = obj.set(k.as_str(), setter.set_value_to.clone());
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
                    let rename_key_to = renamer.rename_key_to.as_str();
                    data.select_values_and_paths_mut(schema, &parent, |_path, mut selected| {
                        match selected.value().kind() {
                            JsonKind::Array => {
                                let len = selected.value().len().unwrap_or(0);
                                for index in 0..len {
                                    if let Ok(mut item) = selected.child_mut(index) {
                                        rename_member(&mut item, k, rename_key_to);
                                    }
                                }
                            }
                            _ => rename_member(&mut selected, k, rename_key_to),
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
    /// A rewrite travels through the distributed query-plan cache as JSON
    /// read back by serde. This fails if `set_value_to` loses its rebuild
    /// adapter: serde buffers internally tagged enums, and buffering cannot
    /// capture an arena value.
    #[test]
    fn rewrites_round_trip_through_serde() {
        let rewrite = DataRewrite::ValueSetter(DataValueSetter {
            path: Path::from("a/b"),
            set_value_to: crate::json_ext::json_value!({"kept": [1, {"deep": "yes"}]}),
        });
        let bytes = serde_json::to_string(&rewrite).expect("serializes");
        let back: DataRewrite = serde_json::from_str(&bytes).expect("deserializes");
        assert_eq!(rewrite, back);
    }

    use apollo_compiler::name;

    use super::*;

    /// Builds a [`Value`] from a `serde_json_bytes::json!` fixture.
    macro_rules! json {
        ($($json:tt)+) => {
            apollo_json::Document::from_legacy(&serde_json_bytes::json!($($json)+)).root_handle()
        };
    }

    // The schema is not used for the tests
    // but we need a valid one
    const SCHEMA: &str = include_str!("../testdata/minimal_supergraph.graphql");

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
            rename_key_to: name!("testField"),
        });

        dr.maybe_apply(
            &Schema::parse(SCHEMA, &Default::default()).unwrap(),
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
            rename_key_to: name!("testField"),
        });

        dr.maybe_apply(
            &Schema::parse(SCHEMA, &Default::default()).unwrap(),
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
