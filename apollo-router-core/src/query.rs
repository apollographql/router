use crate::prelude::graphql::*;
use apollo_parser::ast;
use derivative::Derivative;
use std::collections::HashMap;

#[derive(Debug, Derivative)]
#[derivative(PartialEq, Hash, Eq)]
pub struct Query {
    string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    fragments: HashMap<String, Vec<Selection>>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    operations: Vec<Operation>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    object_types: HashMap<String, ObjectType>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    interfaces: HashMap<String, Interface>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    operation_type_map: HashMap<OperationType, String>,
}

impl Query {
    /// Returns a reference to the underlying query string.
    pub fn as_str(&self) -> &str {
        self.string.as_str()
    }

    /// Re-format the response value to match this query.
    ///
    /// This will discard unrequested fields and re-order the output to match the order of the
    /// query.
    #[tracing::instrument(level = "trace")]
    pub fn format_response(&self, response: &mut Response, operation_name: Option<&str>) {
        let data = std::mem::take(&mut response.data);
        match data {
            Value::Object(init) => {
                let output = self.operations.iter().fold(init, |mut input, operation| {
                    if operation_name.is_none() || operation.name.as_deref() == operation_name {
                        let mut output = Object::default();
                        self.apply_selection_set(&operation.selection_set, &mut input, &mut output);
                        output
                    } else {
                        input
                    }
                });
                response.data = output.into();
            }
            _ => {
                failfast_debug!("Invalid type for data in response.");
            }
        }
    }

    pub fn parse(query: impl Into<String>, schema: &Schema) -> Option<Self> {
        let string = query.into();
        let mut fragments = schema
            .fragments()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<HashMap<String, Vec<_>>>();

        let parser = apollo_parser::Parser::new(string.as_str());
        let tree = parser.parse();

        if !tree.errors().is_empty() {
            failfast_debug!(
                "Parsing error(s): {}",
                tree.errors()
                    .iter()
                    .map(|err| format!("{:?}", err))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            return None;
        }

        let document = tree.document();
        fragments.extend(Self::fragments(&document));

        let operations = document
            .definitions()
            .filter_map(|definition| {
                if let ast::Definition::OperationDefinition(operation) = definition {
                    Some(operation.into())
                } else {
                    None
                }
            })
            .collect();

        macro_rules! implement_object_type_or_interface_map {
            ($ty:ty, $ty_extension:ty, $ast_ty:path, $ast_extension_ty:path $(,)?) => {{
                let mut map = document
                    .definitions()
                    .filter_map(|definition| {
                        if let $ast_ty(definition) = definition {
                            let instance = <$ty>::from(definition);
                            Some((instance.name.clone(), instance))
                        } else {
                            None
                        }
                    })
                    .collect::<HashMap<String, $ty>>();

                document
                    .definitions()
                    .filter_map(|definition| {
                        if let $ast_extension_ty(extension) = definition {
                            Some(<$ty_extension>::from(extension))
                        } else {
                            None
                        }
                    })
                    .for_each(|extension| {
                        if let Some(instance) = map.get_mut(&extension.name) {
                            instance.fields.extend(extension.fields);
                            instance.interfaces.extend(extension.interfaces);
                        } else {
                            failfast_debug!(
                                concat!(
                                    stringify!($ty_extension),
                                    " exists for {:?} but ",
                                    stringify!($ty),
                                    " could not be found."
                                ),
                                extension.name,
                            );
                        }
                    });

                map
            }};
        }

        let object_types = implement_object_type_or_interface_map!(
            ObjectType,
            ObjectTypeExtension,
            ast::Definition::ObjectTypeDefinition,
            ast::Definition::ObjectTypeExtension,
        );

        let interfaces = implement_object_type_or_interface_map!(
            Interface,
            InterfaceExtension,
            ast::Definition::InterfaceTypeDefinition,
            ast::Definition::InterfaceTypeExtension,
        );

        let operation_type_map = document
            .definitions()
            .filter_map(|definition| match definition {
                ast::Definition::SchemaDefinition(definition) => {
                    Some(definition.root_operation_type_definitions())
                }
                ast::Definition::SchemaExtension(extension) => {
                    Some(extension.root_operation_type_definitions())
                }
                _ => None,
            })
            .flatten()
            .map(|definition| {
                // Spec: https://spec.graphql.org/draft/#sec-Schema
                let type_name = definition
                    .named_type()
                    .expect("the node NamedType is not optional in the spec; qed")
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let operation_type = OperationType::from(
                    definition
                        .operation_type()
                        .expect("the node NamedType is not optional in the spec; qed"),
                );
                (operation_type, type_name)
            })
            .collect();

        Some(Query {
            string,
            fragments,
            operations,
            object_types,
            interfaces,
            operation_type_map,
        })
    }

    fn fragments(document: &ast::Document) -> HashMap<String, Vec<Selection>> {
        document
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
            .collect()
    }

    fn apply_selection_set(
        &self,
        selection_set: &[Selection],
        input: &mut Object,
        output: &mut Object,
    ) {
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    selection_set,
                } => {
                    if let Some(input_value) = input.remove(name) {
                        if let Some(selection_set) = selection_set {
                            match input_value {
                                Value::Object(mut input_object) => {
                                    let mut output_object = Object::default();
                                    self.apply_selection_set(
                                        selection_set,
                                        &mut input_object,
                                        &mut output_object,
                                    );
                                    output.insert(name.to_string(), output_object.into());
                                }
                                Value::Array(input_array) => {
                                    let output_array = input_array
                                        .into_iter()
                                        .enumerate()
                                        .map(|(i, mut element)| {
                                            if let Some(input_object) = element.as_object_mut() {
                                                let mut output_object = Object::default();
                                                self.apply_selection_set(
                                                    selection_set,
                                                    input_object,
                                                    &mut output_object,
                                                );
                                                output_object.into()
                                            } else {
                                                failfast_debug!(
                                                    "Array element is not an object: {}[{}]",
                                                    name,
                                                    i,
                                                );
                                                element
                                            }
                                        })
                                        .collect::<Value>();
                                    output.insert(name.to_string(), output_array);
                                }
                                _ => {
                                    output.insert(name.clone(), input_value);
                                    failfast_debug!(
                                        "Field is not an object nor an array of object: {}",
                                        name,
                                    );
                                }
                            }
                        } else {
                            output.insert(name.to_string(), input_value);
                        }
                    } else {
                        failfast_debug!("Missing field: {}", name);
                    }
                }
                Selection::InlineFragment { selection_set } => {
                    self.apply_selection_set(selection_set, input, output);
                }
                Selection::FragmentSpread { name } => {
                    if let Some(selection_set) = self.fragments.get(name) {
                        self.apply_selection_set(selection_set, input, output);
                    } else {
                        failfast_debug!("Missing fragment named: {}", name);
                    }
                }
            }
        }
    }

    pub(crate) fn get_root_object_type(&self, operation: impl AsRef<str>) -> Option<ObjectType> {
        todo!()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Selection {
    Field {
        name: String,
        selection_set: Option<Vec<Selection>>,
    },
    InlineFragment {
        selection_set: Vec<Selection>,
    },
    FragmentSpread {
        name: String,
    },
}

impl From<ast::Selection> for Selection {
    fn from(selection: ast::Selection) -> Self {
        match selection {
            // Spec: https://spec.graphql.org/draft/#Field
            ast::Selection::Field(field) => {
                let name = field
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let alias = field.alias().map(|x| x.name().unwrap().text().to_string());
                let name = alias.unwrap_or(name);
                let selection_set = field
                    .selection_set()
                    .map(|x| x.selections().into_iter().map(Into::into).collect());

                Self::Field {
                    name,
                    selection_set,
                }
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            ast::Selection::InlineFragment(inline_fragment) => {
                let selection_set = inline_fragment
                    .selection_set()
                    .expect("the node SelectionSet is not optional in the spec; qed")
                    .selections()
                    .into_iter()
                    .map(Into::into)
                    .collect();

                Self::InlineFragment { selection_set }
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

                Self::FragmentSpread { name }
            }
        }
    }
}

#[derive(Debug)]
struct Operation {
    name: Option<String>,
    selection_set: Vec<Selection>,
}

impl From<ast::OperationDefinition> for Operation {
    // Spec: https://spec.graphql.org/draft/#sec-Language.Operations
    fn from(operation: ast::OperationDefinition) -> Self {
        let name = operation.name().map(|x| x.text().to_string());
        let selection_set = operation
            .selection_set()
            .expect("the node SelectionSet is not optional in the spec; qed")
            .selections()
            .map(Into::into)
            .collect();

        Operation {
            selection_set,
            name,
        }
    }
}

macro_rules! implement_object_type_or_interface {
    ($visibility:vis $name:ident, $ast_ty:ty) => {
        #[derive(Debug)]
        $visibility struct $name {
            name: String,
            fields: HashMap<String, FieldType>,
            interfaces: Vec<String>,
        }

        impl From<$ast_ty> for $name {
            fn from(definition: $ast_ty) -> Self {
                let name = definition
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let fields = definition
                    .fields_definition()
                    .iter()
                    .flat_map(|x| x.field_definitions())
                    .map(|x| {
                        let name = x
                            .name()
                            .expect("the node Name is not optional in the spec; qed")
                            .text()
                            .to_string();
                        let ty = x
                            .ty()
                            .expect("the node Type is not optional in the spec; qed")
                            .into();
                        (name, ty)
                    })
                    .collect();
                let interfaces = definition
                    .implements_interfaces()
                    .iter()
                    .flat_map(|x| x.named_types())
                    .map(|x| {
                        x.name()
                            .expect("neither Name neither NamedType are optionals; qed")
                            .text()
                            .to_string()
                    })
                    .collect();

                $name {
                    name,
                    fields,
                    interfaces,
                }
            }
        }
    };
}

// Spec: https://spec.graphql.org/draft/#sec-Objects
implement_object_type_or_interface!(pub(crate) ObjectType, ast::ObjectTypeDefinition);
// Spec: https://spec.graphql.org/draft/#sec-Object-Extensions
implement_object_type_or_interface!(ObjectTypeExtension, ast::ObjectTypeExtension);
// Spec: https://spec.graphql.org/draft/#sec-Interfaces
implement_object_type_or_interface!(Interface, ast::InterfaceTypeDefinition);
// Spec: https://spec.graphql.org/draft/#sec-Interface-Extensions
implement_object_type_or_interface!(InterfaceExtension, ast::InterfaceTypeExtension);

// Primitives are taken from scalars: https://spec.graphql.org/draft/#sec-Scalars
// TODO: custom scalars defined in https://spec.graphql.org/draft/#ScalarTypeDefinition
#[derive(Debug)]
enum FieldType {
    Named(String),
    List(Box<FieldType>),
    NonNull(Box<FieldType>),
    String,
    Int,
    Float,
    Id,
    Boolean,
}

impl From<ast::Type> for FieldType {
    // Spec: https://spec.graphql.org/draft/#sec-Type-References
    fn from(ty: ast::Type) -> Self {
        match ty {
            ast::Type::NamedType(named) => named.into(),
            ast::Type::ListType(list) => list.into(),
            ast::Type::NonNullType(non_null) => non_null.into(),
        }
    }
}

impl From<ast::NamedType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#NamedType
    fn from(named: ast::NamedType) -> Self {
        let name = named
            .name()
            .expect("the node Name is not optional in the spec; qed")
            .text()
            .to_string();
        match name.as_str() {
            "String" => Self::String,
            "Int" => Self::Int,
            "Float" => Self::Float,
            "ID" => Self::Id,
            "Boolean" => Self::Boolean,
            _ => Self::Named(name),
        }
    }
}

impl From<ast::ListType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#ListType
    fn from(list: ast::ListType) -> Self {
        Self::List(Box::new(
            list.ty()
                .expect("the node Type is not optional in the spec; qed")
                .into(),
        ))
    }
}

impl From<ast::NonNullType> for FieldType {
    // Spec: https://spec.graphql.org/draft/#NonNullType
    fn from(non_null: ast::NonNullType) -> Self {
        if let Some(list) = non_null.list_type() {
            Self::NonNull(Box::new(list.into()))
        } else if let Some(named) = non_null.named_type() {
            Self::NonNull(Box::new(named.into()))
        } else {
            unreachable!("either the NamedType node is provided, either the ListType node; qed")
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum OperationType {
    Query,
    Mutation,
    Subscription,
}

impl From<ast::OperationType> for OperationType {
    // Spec: https://spec.graphql.org/draft/#OperationType
    fn from(operation_type: ast::OperationType) -> Self {
        if operation_type.query_token().is_some() {
            Self::Query
        } else if operation_type.mutation_token().is_some() {
            Self::Mutation
        } else if operation_type.subscription_token().is_some() {
            Self::Subscription
        } else {
            unreachable!(
                "either the `query` token is provided, either the `mutation` token, \
                either the `subscription` token; qed"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_log::test;

    macro_rules! assert_eq_and_ordered {
        ($a:expr, $b:expr $(,)?) => {
            assert_eq!($a, $b,);
            assert!(
                $a.eq_and_ordered(&$b),
                "assertion failed: objects are not ordered the same:\
                \n  left: `{:?}`\n right: `{:?}`",
                $a,
                $b,
            );
        };
    }

    #[test]
    fn reformat_response_data_field() {
        let schema: Schema = "".parse().unwrap();
        let query = Query::parse(
            r#"{
                foo
                stuff{bar}
                array{bar}
                baz
                alias:baz
                alias_obj:baz_obj{bar}
                alias_array:baz_array{bar}
            }"#,
            &schema,
        )
        .unwrap();
        let mut response = Response::builder()
            .data(json! {{
                "foo": "1",
                "stuff": {"bar": "2"},
                "array": [{"bar": "3", "baz": "4"}, {"bar": "5", "baz": "6"}],
                "baz": "7",
                "alias": "7",
                "alias_obj": {"bar": "8"},
                "alias_array": [{"bar": "9", "baz": "10"}, {"bar": "11", "baz": "12"}],
                "other": "13",
            }})
            .build();
        query.format_response(&mut response, None);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "foo": "1",
                "stuff": {
                    "bar": "2",
                },
                "array": [
                    {"bar": "3"},
                    {"bar": "5"},
                ],
                "baz": "7",
                "alias": "7",
                "alias_obj": {
                    "bar": "8",
                },
                "alias_array": [
                    {"bar": "9"},
                    {"bar": "11"},
                ],
            }},
        );
    }

    #[test]
    fn reformat_response_data_inline_fragment() {
        let schema: Schema = "".parse().unwrap();
        let query = Query::parse(r#"{... on Stuff { stuff{bar}}}"#, &schema).unwrap();
        let mut response = Response::builder()
            .data(json! {{"stuff": {"bar": "2"}}})
            .build();
        query.format_response(&mut response, None);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "stuff": {
                    "bar": "2",
                },
            }},
        );
    }

    #[test]
    fn reformat_response_data_fragment_spread() {
        let schema: Schema = "fragment baz on Baz {baz}".parse().unwrap();
        let query = Query::parse(
            r#"{...foo ...bar ...baz} fragment foo on Foo {foo} fragment bar on Bar {bar}"#,
            &schema,
        )
        .unwrap();
        let mut response = Response::builder()
            .data(json! {{"foo": "1", "bar": "2", "baz": "3"}})
            .build();
        query.format_response(&mut response, None);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "foo": "1",
                "bar": "2",
                "baz": "3",
            }},
        );
    }

    #[test]
    fn reformat_response_data_best_effort() {
        let schema: Schema = "".parse().unwrap();
        let query = Query::parse(
            r#"{foo stuff{bar baz} ...fragment array{bar baz} other{bar}}"#,
            &schema,
        )
        .unwrap();
        let mut response = Response::builder()
            .data(json! {{
                "foo": "1",
                "stuff": {"baz": "2"},
                "array": [
                    {"baz": "3"},
                    "4",
                    {"bar": "5"},
                ],
                "other": "6",
            }})
            .build();
        query.format_response(&mut response, None);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "foo": "1",
                "stuff": {
                    "baz": "2",
                },
                "array": [
                    {"baz": "3"},
                    "4",
                    {"bar": "5"},
                ],
                "other": "6",
            }},
        );
    }

    #[test]
    fn reformat_matching_operation() {
        let schema: Schema = "".parse().unwrap();
        let query = Query::parse(
            r#"query MyOperation {
                foo
            }"#,
            &schema,
        )
        .unwrap();
        let mut response = Response::builder()
            .data(json! {{
                "foo": "1",
                "other": "2",
            }})
            .build();
        let untouched = response.clone();
        query.format_response(&mut response, Some("OtherOperation"));
        assert_eq_and_ordered!(response.data, untouched.data);
        query.format_response(&mut response, Some("MyOperation"));
        assert_eq_and_ordered!(response.data, json! {{ "foo": "1" }});
    }
}
