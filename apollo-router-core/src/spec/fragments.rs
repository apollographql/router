use crate::*;
use apollo_parser::ast;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub(crate) struct Fragments {
    map: HashMap<String, Fragment>,
}

impl Fragments {
    pub(crate) fn from_ast(document: &ast::Document, schema: &Schema) -> Option<Self> {
        let map = document
            .definitions()
            .filter_map(|definition| match definition {
                // Spec: https://spec.graphql.org/draft/#FragmentDefinition
                ast::Definition::FragmentDefinition(fragment_definition) => {
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

                    let mut known_selections = HashSet::new();
                    let mut selection_set = Vec::new();
                    for selection in fragment_definition
                        .selection_set()
                        .expect("the node SelectionSet is not optional in the spec; qed")
                        .selections()
                    {
                        let selection = Selection::from_ast(
                            selection,
                            &FieldType::Named(type_condition.clone()),
                            schema,
                        )?;
                        if !known_selections.contains(&selection) {
                            known_selections.insert(selection.clone());
                            selection_set.push(selection);
                        }
                    }

                    Some((
                        name,
                        Fragment {
                            type_condition,
                            selection_set,
                        },
                    ))
                }
                _ => None,
            })
            .collect();
        Some(Fragments { map })
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
}
