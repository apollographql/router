use std::collections::HashMap;

use apollo_compiler::ExecutableDocument;
use serde::Deserialize;
use serde::Serialize;

use crate::spec::query::DeferStats;
use crate::spec::Schema;
use crate::spec::Selection;
use crate::spec::SpecError;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Fragments {
    pub(crate) map: HashMap<String, Fragment>,
}

impl Fragments {
    pub(crate) fn from_hir(
        doc: &ExecutableDocument,
        schema: &Schema,
        defer_stats: &mut DeferStats,
    ) -> Result<Self, SpecError> {
        let map = doc
            .fragments
            .iter()
            .map(|(name, fragment)| {
                let type_condition = fragment.type_condition();
                let fragment = Fragment {
                    type_condition: type_condition.as_str().to_owned(),
                    selection_set: fragment
                        .selection_set
                        .selections
                        .iter()
                        .filter_map(|selection| {
                            Selection::from_hir(selection, type_condition, schema, 0, defer_stats)
                                .transpose()
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                };
                Ok((name.as_str().to_owned(), fragment))
            })
            .collect::<Result<_, _>>()?;
        Ok(Fragments { map })
    }
}

impl Fragments {
    pub(crate) fn get(&self, key: impl AsRef<str>) -> Option<&Fragment> {
        self.map.get(key.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Fragment {
    pub(crate) type_condition: String,
    pub(crate) selection_set: Vec<Selection>,
}
