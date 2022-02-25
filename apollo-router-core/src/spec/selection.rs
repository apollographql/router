use crate::{FieldType, Fragment, ObjectType, Schema};
use apollo_parser::ast;

#[derive(Debug, Clone)]
pub(crate) enum Selection {
    Field {
        name: String,
        selection_set: Option<Vec<Selection>>,
        field_type: FieldType,
    },
    InlineFragment {
        fragment: Fragment,
    },
    FragmentSpread {
        name: String,
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
                println!("Selection::from_ast[{}] field {}", line!(), field);
                let name = field
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let alias = field.alias().map(|x| x.name().unwrap().text().to_string());
                let name = alias.unwrap_or(name);

                let current_object_type = current_type
                    .inner_type_name()
                    .and_then(|name| schema.object_types.get(name))?;

                let field_type = current_object_type.field(&name)?;

                let selection_set = if field_type.is_builtin_scalar() {
                    None
                } else {
                    field.selection_set().and_then(|x| {
                        x.selections()
                            .into_iter()
                            .map(|selection| Selection::from_ast(selection, &field_type, schema))
                            .collect()
                    })
                };

                Some(Self::Field {
                    name,
                    selection_set,
                    field_type: field_type.clone(),
                })
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            ast::Selection::InlineFragment(inline_fragment) => {
                println!(
                    "Selection::from_ast[{}] inline {}",
                    line!(),
                    inline_fragment
                );

                let type_condition = inline_fragment
                    .type_condition()
                    .expect("Fragments must specify the type they apply to; qed")
                    .named_type()
                    .expect("Fragments must specify the type they apply to; qed")
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();

                let current_type = FieldType::Named(type_condition.clone());

                let selection_set = inline_fragment
                    .selection_set()
                    .expect("the node SelectionSet is not optional in the spec; qed")
                    .selections()
                    .into_iter()
                    .map(|selection| Selection::from_ast(selection, &current_type, schema))
                    .collect::<Option<Vec<_>>>()?;
                println!(
                    "Selection::from_ast[{}] selection_set {:?}",
                    line!(),
                    selection_set
                );

                Some(Self::InlineFragment {
                    fragment: Fragment {
                        type_condition,
                        selection_set,
                    },
                })
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            ast::Selection::FragmentSpread(fragment_spread) => {
                println!(
                    "Selection::from_ast[{}] spread {}",
                    line!(),
                    fragment_spread
                );
                let name = fragment_spread
                    .fragment_name()
                    .expect("the node FragmentName is not optional in the spec; qed")
                    .name()
                    .unwrap()
                    .text()
                    .to_string();

                Some(Self::FragmentSpread { name })
            }
        }
    }

    pub(crate) fn from_operation_ast(
        selection: ast::Selection,
        current_object_type: &ObjectType,
        schema: &Schema,
    ) -> Option<Self> {
        match selection {
            // Spec: https://spec.graphql.org/draft/#Field
            ast::Selection::Field(field) => {
                let original_name = field
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let alias = field.alias().map(|x| x.name().unwrap().text().to_string());
                let name = alias.unwrap_or(original_name.clone());

                println!(
                    "Selection::from_operation_ast: looking for name {} (alias? {}) in {:?}",
                    original_name, name, current_object_type
                );
                let field_type = current_object_type.field(&original_name)?;

                let selection_set = if field_type.is_builtin_scalar() {
                    None
                } else {
                    field.selection_set().and_then(|x| {
                        println!(
                            "Selection::from_operation_ast[{}] field={}, selections = {}",
                            line!(),
                            field,
                            x
                        );
                        x.selections()
                            .into_iter()
                            .map(|selection| {
                                println!("=======>will test selection: {}", selection);
                                let res = Selection::from_ast(selection, &field_type, schema);
                                println!("<=======res: {:?}", res);
                                res
                            })
                            .collect()
                    })
                };

                println!("======> generated selection_set: {:?}", selection_set);

                Some(Self::Field {
                    name,
                    selection_set,
                    field_type: field_type.clone(),
                })
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            ast::Selection::InlineFragment(inline_fragment) => {
                println!(
                    "Selection::from_operation_ast[{}] got inline fragment: {:?}",
                    line!(),
                    inline_fragment
                );
                //FIXME: there should be no fragment there right?
                None
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            ast::Selection::FragmentSpread(fragment) => {
                println!(
                    "Selection::from_operation_ast[{}] got fragment spread: {:?}",
                    line!(),
                    fragment
                );
                //FIXME: there should be no fragment there right?
                None
            }
        }
    }
}
