//! Declares data structure for the "data rewrites" that the query planner can include in some `FetchNode`,
//! and implements those rewrites.
//!
//! Note that on the typescript side, the query planner currently declare the rewrites that applies
//! to "inputs" and those applying to "outputs" separatly. This is due to simplify the current
//! implementation on the typescript side (for ... reasons), but it does not simplify anything to make that
//! distinction here. All this means is that, as of this writing, some kind of rewrites will only
//! every appear on the input side, while other will only appear on outputs, but it does not hurt
//! to be future-proof by supporting all types of rewrites on both "sides".

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
    pub(crate) rename_key_to: String,
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
                // TODO[igni]
                if let Some((parent, PathElement::Key(k))) = split_path_last_element(&setter.path) {
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
                // TODO[igni]
                if let Some((parent, PathElement::Key(k))) = split_path_last_element(&renamer.path)
                {
                    data.select_values_and_paths_mut(schema, &parent, |_path, selected| {
                        if let Some(obj) = selected.as_object_mut() {
                            if let Some(value) = obj.remove(k.as_str()) {
                                obj.insert(renamer.rename_key_to.clone(), value);
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
