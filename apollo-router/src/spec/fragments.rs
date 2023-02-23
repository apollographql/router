use std::collections::HashMap;

use apollo_compiler::ApolloCompiler;
use apollo_compiler::HirDatabase;
use serde::Deserialize;
use serde::Serialize;

use super::parse_include_hir;
use super::parse_skip_hir;
use crate::spec::FieldType;
use crate::spec::Include;
use crate::spec::Schema;
use crate::spec::Selection;
use crate::spec::Skip;
use crate::spec::SpecError;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Fragments {
    pub(crate) map: HashMap<String, Fragment>,
}

impl Fragments {
    pub(crate) fn from_hir(compiler: &ApolloCompiler, schema: &Schema) -> Result<Self, SpecError> {
        let map = compiler
            .db
            .all_fragments()
            .iter()
            .map(|(name, fragment)| {
                let type_condition = fragment.type_condition().to_owned();
                let current_type = FieldType::Named(type_condition.clone());
                let fragment = Fragment {
                    type_condition,
                    selection_set: fragment
                        .selection_set()
                        .selection()
                        .iter()
                        .filter_map(|selection| {
                            Selection::from_hir(selection, &current_type, schema, 0).transpose()
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    skip: fragment
                        .directives()
                        .iter()
                        .find_map(parse_skip_hir)
                        .unwrap_or(Skip::No),
                    include: fragment
                        .directives()
                        .iter()
                        .find_map(parse_include_hir)
                        .unwrap_or(Include::Yes),
                };
                Ok((name.clone(), fragment))
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
    pub(crate) skip: Skip,
    pub(crate) include: Include,
}
