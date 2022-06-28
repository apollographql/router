use std::collections::HashMap;

use apollo_parser::ast;

use crate::*;

#[derive(Debug, Default)]
pub(crate) struct Fragments {
    map: HashMap<String, Fragment>,
}

impl Fragments {
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
                    .expect("the node FragmentName is not optional in the spec; qed")
                    .name()
                    .unwrap()
                    .text()
                    .to_string();

                let type_condition = fragment_definition
                    .type_condition()
                    .expect("Fragments must specify the type they apply to; qed")
                    .named_type()
                    .expect("Fragments must specify the type they apply to; qed")
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();

                let selection_set = fragment_definition
                    .selection_set()
                    .expect("the node SelectionSet is not optional in the spec; qed")
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Fragment {
    pub(crate) type_condition: String,
    pub(crate) selection_set: Vec<Selection>,
    pub(crate) skip: Skip,
    pub(crate) include: Include,
}
