use crate::prelude::graphql::*;
use apollo_parser::ast;
use derivative::Derivative;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::level_filters::LevelFilter;

#[derive(Debug, Derivative)]
#[derivative(PartialEq, Hash, Eq)]
pub struct Query {
    string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    fragments: Fragments,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    operations: Vec<Operation>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    operation_type_map: HashMap<OperationType, String>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    schema: Arc<Schema>,
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

    pub fn parse(query: impl Into<String>, schema: Arc<Schema>) -> Option<Self> {
        let string = query.into();

        let parser = apollo_parser::Parser::new(string.as_str());
        let tree = parser.parse();

        if tree.errors().next().is_some() {
            failfast_debug!(
                "Parsing error(s): {}",
                tree.errors()
                    .map(|err| format!("{:?}", err))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            return None;
        }

        let document = tree.document();
        let fragments = Fragments::from(&document);

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
            operation_type_map,
            schema,
        })
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
                    if let Some(selection_set) = self
                        .fragments
                        .get(name)
                        .or_else(|| self.schema.fragments().get(name))
                    {
                        self.apply_selection_set(selection_set, input, output);
                    } else {
                        failfast_debug!("Missing fragment named: {}", name);
                    }
                }
            }
        }
    }

    /// TODO
    pub fn validate_variable_types(&self, request: &Request) -> Result<(), Response> {
        let operation_name = request.operation_name.as_deref();
        let operation_variable_types =
            self.operations
                .iter()
                .fold(HashMap::new(), |mut acc, operation| {
                    if operation_name.is_none() || operation.name.as_deref() == operation_name {
                        acc.extend(operation.variables.iter())
                    }
                    acc
                });

        if LevelFilter::current() >= LevelFilter::DEBUG {
            let known_variables = operation_variable_types.keys().cloned().collect();
            let provided_variables = request.variables.keys().collect::<HashSet<_>>();
            let unknown_variables = provided_variables
                .difference(&known_variables)
                .collect::<Vec<_>>();
            if !unknown_variables.is_empty() {
                failfast_debug!(
                    "Received variable unknown to the query: {:?}",
                    unknown_variables,
                );
            }
        }

        let errors = operation_variable_types
            .iter()
            .filter_map(|(name, ty)| {
                let value = request.variables.get(name.as_str()).unwrap_or(&Value::Null);
                (!ty.validate_value(value, &self.schema)).then(|| {
                    FetchError::ValidationInvalidTypeVariable {
                        name: name.to_string(),
                    }
                    .to_graphql_error(None)
                })
            })
            .collect::<Vec<_>>();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Response::builder().errors(errors).build())
        }
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
    variables: HashMap<String, FieldType>,
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
        let variables = operation
            .variable_definitions()
            .iter()
            .flat_map(|x| x.variable_definitions())
            .map(|definition| {
                let name = definition
                    .variable()
                    .expect("the node Variable is not optional in the spec; qed")
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let ty = FieldType::from(
                    definition
                        .ty()
                        .expect("the node Type is not optional in the spec; qed"),
                );

                (name, ty)
            })
            .collect();

        Operation {
            selection_set,
            name,
            variables,
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
            Arc::new(schema),
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
        let query = Query::parse(r#"{... on Stuff { stuff{bar}}}"#, Arc::new(schema)).unwrap();
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
            Arc::new(schema),
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
            Arc::new(schema),
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
            Arc::new(schema),
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

    macro_rules! run_validation {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let variables = match $variables {
                Value::Object(object) => object,
                _ => unreachable!("variables must be an object"),
            };
            let schema: Schema = $schema.parse().expect("could not parse schema");
            let request = Request::builder()
                .variables(variables)
                .query($query)
                .build();
            let query =
                Query::parse(&request.query, Arc::new(schema)).expect("could not parse query");
            query.validate_variable_types(&request)
        }};
    }

    macro_rules! assert_validation {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let res = run_validation!($schema, $query, $variables);
            assert!(res.is_ok(), "validation should have succeeded: {:?}", res);
        }};
    }

    macro_rules! assert_validation_error {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let res = run_validation!($schema, $query, $variables);
            assert!(res.is_err(), "validation should have failed");
        }};
    }

    #[test]
    fn variable_validation() {
        assert_validation!("", "query($foo:Int){x}", json!({}));
        assert_validation!("", "query($foo:Int){x}", json!({"foo":2}));
        assert_validation_error!("", "query($foo:Int){x}", json!({"foo":2.0}));
        assert_validation_error!("", "query($foo:Int){x}", json!({"foo":"str"}));
        assert_validation_error!("", "query($foo:Int){x}", json!({"foo":true}));
        assert_validation_error!("", "query($foo:Int){x}", json!({"foo":{}}));
        assert_validation!("", "query($foo:ID){x}", json!({"foo": "1"}));
        assert_validation!("", "query($foo:ID){x}", json!({"foo": 1}));
        assert_validation_error!("", "query($foo:ID){x}", json!({"foo": true}));
        assert_validation_error!("", "query($foo:ID){x}", json!({"foo": {}}));
        assert_validation!("", "query($foo:String){x}", json!({"foo": "str"}));
        assert_validation!("", "query($foo:Float){x}", json!({"foo":2.0}));
        assert_validation_error!("", "query($foo:Float){x}", json!({"foo":2}));
        assert_validation_error!("", "query($foo:Int!){x}", json!({}));
        assert_validation!("", "query($foo:[Int]){x}", json!({}));
        assert_validation_error!("", "query($foo:[Int]){x}", json!({"foo":1}));
        assert_validation_error!("", "query($foo:[Int]){x}", json!({"foo":"str"}));
        assert_validation_error!("", "query($foo:[Int]){x}", json!({"foo":{}}));
        assert_validation_error!("", "query($foo:[Int]!){x}", json!({}));
        assert_validation!("", "query($foo:[Int]!){x}", json!({"foo":[]}));
        assert_validation!("", "query($foo:[Int]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!("", "query($foo:[Int]){x}", json!({"foo":["1","2","3"]}));
        assert_validation!("", "query($foo:[String]){x}", json!({"foo":["1","2","3"]}));
        assert_validation_error!("", "query($foo:[String]){x}", json!({"foo":[1,2,3]}));
        assert_validation!("", "query($foo:[Int!]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!("", "query($foo:[Int!]){x}", json!({"foo":[1,null,3]}));
        assert_validation!("", "query($foo:[Int]){x}", json!({"foo":[1,null,3]}));
        assert_validation!("type Foo{}", "query($foo:Foo){x}", json!({}));
        assert_validation!("type Foo{}", "query($foo:Foo){x}", json!({"foo":{}}));
        assert_validation_error!("type Foo{}", "query($foo:Foo){x}", json!({"foo":1}));
        assert_validation_error!("type Foo{}", "query($foo:Foo){x}", json!({"foo":"str"}));
        assert_validation_error!("type Foo{x:Int!}", "query($foo:Foo){x}", json!({"foo":{}}));
        assert_validation!(
            "type Foo{x:Int!}",
            "query($foo:Foo){x}",
            json!({"foo":{"x":1}})
        );
        assert_validation!(
            "type Foo implements Bar interface Bar{x:Int!}",
            "query($foo:Foo){x}",
            json!({"foo":{"x":1}}),
        );
        assert_validation_error!(
            "type Foo implements Bar interface Bar{x:Int!}",
            "query($foo:Foo){x}",
            json!({"foo":{"x":"str"}}),
        );
        assert_validation_error!(
            "type Foo implements Bar interface Bar{x:Int!}",
            "query($foo:Foo){x}",
            json!({"foo":{}}),
        );
        assert_validation!("scalar Foo", "query($foo:Foo!){x}", json!({"foo":{}}));
        assert_validation!("scalar Foo", "query($foo:Foo!){x}", json!({"foo":1}));
        assert_validation_error!("scalar Foo", "query($foo:Foo!){x}", json!({}));
    }
}
