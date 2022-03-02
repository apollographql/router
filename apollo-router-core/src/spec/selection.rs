use std::collections::HashSet;

use crate::{FieldType, Fragment, Schema};
use apollo_parser::ast;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Selection {
    Field {
        name: String,
        selection_set: Option<Vec<Selection>>,
        field_type: FieldType,
    },
    InlineFragment {
        fragment: Fragment,
        known_type: bool,
    },
    FragmentSpread {
        name: String,
        known_type: Option<String>,
    },
}

impl Selection {
    pub(crate) fn from_ast(
        selection: ast::Selection,
        current_type: &FieldType,
        schema: &Schema,
    ) -> Option<Self> {
        match selection {
            // Spec: https://spec.graphql.org/draft/#Field
            ast::Selection::Field(field) => {
                let field_name = field
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();

                let field_type = if field_name.as_str() == "__typename" {
                    FieldType::String
                } else {
                    current_type
                        .inner_type_name()
                        .and_then(|name| {
                            //looking into object types
                            schema
                                .object_types
                                .get(name)
                                .and_then(|ty| ty.field(&field_name))
                                // otherwise, it might be an interface
                                .or_else(|| {
                                    schema
                                        .interfaces
                                        .get(name)
                                        .and_then(|ty| ty.field(&field_name))
                                })
                        })?
                        .clone()
                };

                let alias = field.alias().map(|x| x.name().unwrap().text().to_string());
                let name = alias.unwrap_or(field_name);

                let selection_set = if field_type.is_builtin_scalar() {
                    None
                } else {
                    field.selection_set().and_then(|x| {
                        let mut known_selections = HashSet::new();
                        let mut selection_set = Vec::new();
                        for selection in x.selections() {
                            let selection = Selection::from_ast(selection, &field_type, schema)?;
                            if !known_selections.contains(&selection) {
                                known_selections.insert(selection.clone());
                                selection_set.push(selection);
                            }
                        }

                        Some(selection_set)
                    })
                };

                Some(Self::Field {
                    name,
                    selection_set,
                    field_type,
                })
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            ast::Selection::InlineFragment(inline_fragment) => {
                let type_condition = inline_fragment
                    .type_condition()
                    .expect("Fragments must specify the type they apply to; qed")
                    .named_type()
                    .expect("Fragments must specify the type they apply to; qed")
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();

                let fragment_type = FieldType::Named(type_condition.clone());

                let mut known_selections = HashSet::new();
                let mut selection_set = Vec::new();
                for selection in inline_fragment
                    .selection_set()
                    .expect("the node SelectionSet is not optional in the spec; qed")
                    .selections()
                {
                    let selection = Selection::from_ast(selection, &fragment_type, schema)?;
                    if !known_selections.contains(&selection) {
                        known_selections.insert(selection.clone());
                        selection_set.push(selection);
                    }
                }

                Some(Self::InlineFragment {
                    fragment: Fragment {
                        type_condition,
                        selection_set,
                    },
                    known_type: current_type == &fragment_type,
                })
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            ast::Selection::FragmentSpread(fragment_spread) => {
                let name = fragment_spread
                    .fragment_name()
                    .expect("the node FragmentName is not optional in the spec; qed")
                    .name()
                    .unwrap()
                    .text()
                    .to_string();

                Some(Self::FragmentSpread {
                    name,
                    known_type: if let FieldType::Named(type_name) = current_type {
                        Some(type_name.clone())
                    } else {
                        None
                    },
                })
            }
        }
    }
}
