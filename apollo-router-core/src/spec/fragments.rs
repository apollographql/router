use crate::*;
use apollo_parser::ast;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct Fragments {
    pub(crate) map: HashMap<String, Fragment>,
}

impl Fragments {
    pub(crate) fn from_ast(document: &ast::Document, schema: &Schema) -> Option<Self> {
        let mut map = HashMap::new();
        let current_path = Path::default();

        for definition in document
            .definitions()
            .filter(|d| matches!(d, ast::Definition::FragmentDefinition(_)))
        {
            let mut deferred_queries: HashMap<Path, Selection> = HashMap::new();
            match definition {
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

                    let selection_set = fragment_definition
                        .selection_set()
                        .expect("the node SelectionSet is not optional in the spec; qed")
                        .selections()
                        .filter_map(|selection| {
                            Selection::from_ast(
                                &current_path,
                                &mut deferred_queries,
                                selection,
                                &FieldType::Named(type_condition.clone()),
                                schema,
                                &map,
                                0,
                            )
                        })
                        .collect();

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

                    map.insert(
                        name,
                        Fragment {
                            type_condition,
                            selection_set,
                            skip,
                            include,
                            deferred_queries,
                        },
                    );
                }
                _ => unreachable!(),
            }
        }

        Some(Fragments { map })
    }
}

impl Fragments {
    pub(crate) fn get(&self, key: impl AsRef<str>) -> Option<&Fragment> {
        self.map.get(key.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fragment {
    pub(crate) type_condition: String,
    pub(crate) selection_set: Vec<Selection>,
    pub(crate) skip: Skip,
    pub(crate) include: Include,
    pub(crate) deferred_queries: HashMap<Path, Selection>,
}
