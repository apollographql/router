use crate::selection::Selection;
use apollo_parser::ast;
use std::collections::HashMap;

#[derive(Debug)]
pub(crate) struct Fragments {
    map: HashMap<String, Vec<Selection>>,
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
                        .expect("the node SelectionSet is not optional in the spec; qed");

                    Some((name, selection_set.selections().map(Into::into).collect()))
                }
                _ => None,
            })
            .collect();
        Fragments { map }
    }
}

impl Fragments {
    pub(crate) fn get(&self, key: impl AsRef<str>) -> Option<&[Selection]> {
        self.map.get(key.as_ref()).map(|x| x.as_slice())
    }
}
