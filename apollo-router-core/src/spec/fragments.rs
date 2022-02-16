use crate::*;
use apollo_parser::ast;
use std::collections::HashMap;

#[derive(Debug)]
pub(crate) struct Fragments {
    map: HashMap<String, Fragment>,
}

impl From<&ast::Document> for Fragments {
    fn from(document: &ast::Document) -> Self {
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
                    let selection_set = fragment_definition
                        .selection_set()
                        .expect("the node SelectionSet is not optional in the spec; qed")
                        .selections()
                        .map(Into::into)
                        .collect();
                    let type_condition = fragment_definition
                        .type_condition()
                        .expect("Fragments must specify the type they apply to; qed")
                        .named_type()
                        .expect("Fragments must specify the type they apply to; qed")
                        .name()
                        .expect("the node Name is not optional in the spec; qed")
                        .text()
                        .to_string();

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
        Fragments { map }
    }
}

impl Fragments {
    pub(crate) fn get(&self, key: impl AsRef<str>) -> Option<&Fragment> {
        self.map.get(key.as_ref())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Fragment {
    pub(crate) type_condition: String,
    pub(crate) selection_set: Vec<Selection>,
}
