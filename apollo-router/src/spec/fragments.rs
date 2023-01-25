use std::collections::HashMap;

use apollo_compiler::ApolloCompiler;
use apollo_compiler::HirDatabase;
use apollo_parser::ast;
use serde::Deserialize;
use serde::Serialize;

use super::parse_include_hir;
use super::parse_skip_hir;
use crate::spec::parse_include;
use crate::spec::parse_skip;
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

    pub(crate) fn from_ast(document: &ast::Document, schema: &Schema) -> Result<Self, SpecError> {
        let map = document
            .definitions()
            .filter_map(|definition| match definition {
                // Spec: https://spec.graphql.org/draft/#FragmentDefinition
                ast::Definition::FragmentDefinition(fragment_definition) => {
                    Some(fragment_definition)
                }
                _ => None,
            })
            .map(|fragment_definition| {
                let name = fragment_definition
                    .fragment_name()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "the node FragmentName is not optional in the spec".to_string(),
                        )
                    })?
                    .name()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "the node FragmentName is not optional in the spec".to_string(),
                        )
                    })?
                    .text()
                    .to_string();

                let type_condition = fragment_definition
                    .type_condition()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "Fragments must specify the type they apply to".to_string(),
                        )
                    })?
                    .named_type()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "Fragments must specify the type they apply to".to_string(),
                        )
                    })?
                    .name()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "Fragments must specify the type they apply to".to_string(),
                        )
                    })?
                    .text()
                    .to_string();

                let selection_set = fragment_definition
                    .selection_set()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "the node SelectionSet is not optional in the spec".to_string(),
                        )
                    })?
                    .selections()
                    .map(|selection| {
                        Selection::from_ast(
                            selection,
                            &FieldType::Named(type_condition.clone()),
                            schema,
                            0,
                        )
                    })
                    .collect::<Result<Vec<Option<_>>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<Selection>>();

                let skip = fragment_definition
                    .directives()
                    .map(|directives| {
                        for directive in directives.directives() {
                            if let Some(skip) = parse_skip(&directive) {
                                return skip;
                            }
                        }
                        Skip::No
                    })
                    .unwrap_or(Skip::No);
                let include = fragment_definition
                    .directives()
                    .map(|directives| {
                        for directive in directives.directives() {
                            if let Some(include) = parse_include(&directive) {
                                return include;
                            }
                        }
                        Include::Yes
                    })
                    .unwrap_or(Include::Yes);

                Ok(Some((
                    name,
                    Fragment {
                        type_condition,
                        selection_set,
                        skip,
                        include,
                    },
                )))
            })
            .collect::<Result<Vec<_>, SpecError>>()?
            .into_iter()
            .flatten()
            .collect();
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
