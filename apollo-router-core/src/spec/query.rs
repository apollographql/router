use crate::prelude::graphql::*;
use apollo_parser::ast;
use derivative::Derivative;
use std::collections::{HashMap, HashSet};
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
    #[tracing::instrument(skip_all, level = "trace")]
    pub fn format_response(
        &self,
        response: &mut Response,
        operation_name: Option<&str>,
        schema: &Schema,
    ) {
        let data = std::mem::take(&mut response.data);
        match data {
            Value::Object(init) => {
                let output = self.operations.iter().fold(init, |mut input, operation| {
                    if operation_name.is_none() || operation.name.as_deref() == operation_name {
                        let mut output = Object::default();
                        self.apply_selection_set(
                            &operation.selection_set,
                            &mut input,
                            &mut output,
                            schema,
                        );
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

    #[tracing::instrument(skip_all, level = "trace")]
    pub fn parse(query: impl Into<String>) -> Option<Self> {
        let string = query.into();

        let parser = apollo_parser::Parser::new(string.as_str());
        let tree = parser.parse();
        let errors = tree
            .errors()
            .map(|err| format!("{:?}", err))
            .collect::<Vec<_>>();

        if !errors.is_empty() {
            failfast_debug!("Parsing error(s): {}", errors.join(", "));
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
        })
    }

    fn apply_selection_set(
        &self,
        selection_set: &[Selection],
        input: &mut Object,
        output: &mut Object,
        schema: &Schema,
    ) {
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    selection_set,
                } => {
                    if let Some(input_value) = input.remove(name.as_str()) {
                        if let Some(selection_set) = selection_set {
                            match input_value {
                                Value::Object(mut input_object) => {
                                    let mut output_object = Object::default();
                                    self.apply_selection_set(
                                        selection_set,
                                        &mut input_object,
                                        &mut output_object,
                                        schema,
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
                                                    schema,
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
                    self.apply_selection_set(selection_set, input, output, schema);
                }
                Selection::FragmentSpread { name } => {
                    if let Some(selection_set) = self
                        .fragments
                        .get(name)
                        .or_else(|| schema.fragments.get(name))
                    {
                        self.apply_selection_set(selection_set, input, output, schema);
                    } else {
                        failfast_debug!("Missing fragment named: {}", name);
                    }
                }
            }
        }
    }

    /// Validate a [`Request`]'s variables against this [`Query`] using a provided [`Schema`].
    #[tracing::instrument(skip_all, level = "trace")]
    pub fn validate_variables(&self, request: &Request, schema: &Schema) -> Result<(), Response> {
        let operation_name = request.operation_name.as_deref();
        let operation_variable_types =
            self.operations
                .iter()
                .fold(HashMap::new(), |mut acc, operation| {
                    if operation_name.is_none() || operation.name.as_deref() == operation_name {
                        acc.extend(operation.variables.iter().map(|(k, v)| (k.as_str(), v)))
                    }
                    acc
                });

        if LevelFilter::current() >= LevelFilter::DEBUG {
            let known_variables = operation_variable_types.keys().cloned().collect();
            let provided_variables = request
                .variables
                .keys()
                .map(|k| k.as_str())
                .collect::<HashSet<_>>();
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
                let value = request.variables.get(*name).unwrap_or(&Value::Null);
                ty.validate_value(value, schema).err().map(|_| {
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
    use serde_json_bytes::bjson;
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

    macro_rules! assert_format_response {
        ($schema:expr, $query:expr, $response:expr, $operation:expr, $expected:expr $(,)?) => {{
            let schema: Schema = $schema.parse().expect("could not parse schema");
            let query = Query::parse($query).expect("could not parse query");
            let mut response = Response::builder().data($response.clone()).build();
            query.format_response(&mut response, $operation, &schema);
            assert_eq_and_ordered!(response.data, $expected);
        }};
    }

    #[test]
    fn reformat_response_data_field() {
        assert_format_response!(
            "",
            "{
                foo
                stuff{bar}
                array{bar}
                baz
                alias:baz
                alias_obj:baz_obj{bar}
                alias_array:baz_array{bar}
            }",
            bjson! {{
                "foo": "1",
                "stuff": {"bar": "2"},
                "array": [{"bar": "3", "baz": "4"}, {"bar": "5", "baz": "6"}],
                "baz": "7",
                "alias": "7",
                "alias_obj": {"bar": "8"},
                "alias_array": [{"bar": "9", "baz": "10"}, {"bar": "11", "baz": "12"}],
                "other": "13",
            }},
            None,
            bjson! {{
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
        assert_format_response!(
            "",
            "{... on Stuff { stuff{bar}}}",
            bjson! {{"stuff": {"bar": "2"}}},
            None,
            bjson! {{
                "stuff": {
                    "bar": "2",
                },
            }},
        );
    }

    #[test]
    fn reformat_response_data_fragment_spread() {
        assert_format_response!(
            "fragment baz on Baz {baz}",
            "{...foo ...bar ...baz} fragment foo on Foo {foo} fragment bar on Bar {bar}",
            bjson! {{"foo": "1", "bar": "2", "baz": "3"}},
            None,
            bjson! {{
                "foo": "1",
                "bar": "2",
                "baz": "3",
            }},
        );
    }

    #[test]
    fn reformat_response_data_best_effort() {
        assert_format_response!(
            "",
            "{foo stuff{bar baz} ...fragment array{bar baz} other{bar}}",
            bjson! {{
                "foo": "1",
                "stuff": {"baz": "2"},
                "array": [
                    {"baz": "3"},
                    "4",
                    {"bar": "5"},
                ],
                "other": "6",
            }},
            None,
            bjson! {{
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
        let schema = "";
        let query = "query MyOperation { foo }";
        let response = bjson! {{
            "foo": "1",
            "other": "2",
        }};
        assert_format_response!(
            schema,
            query,
            response,
            Some("OtherOperation"),
            bjson! {{
                "foo": "1",
                "other": "2",
            }},
        );
        assert_format_response!(
            schema,
            query,
            response,
            Some("MyOperation"),
            bjson! {{
                "foo": "1",
            }},
        );
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
            let query = Query::parse(&request.query).expect("could not parse query");
            query.validate_variables(&request, &schema)
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
        assert_validation!("", "query($foo:Boolean){x}", bjson!({}));
        assert_validation_error!("", "query($foo:Boolean!){x}", bjson!({}));
        assert_validation!("", "query($foo:Boolean!){x}", bjson!({"foo":true}));
        assert_validation!("", "query($foo:Boolean!){x}", bjson!({"foo":"true"}));
        assert_validation_error!("", "query($foo:Boolean!){x}", bjson!({"foo":"str"}));
        assert_validation!("", "query($foo:Int){x}", bjson!({}));
        assert_validation!("", "query($foo:Int){x}", bjson!({"foo":2}));
        assert_validation_error!("", "query($foo:Int){x}", bjson!({"foo":2.0}));
        assert_validation_error!("", "query($foo:Int){x}", bjson!({"foo":"str"}));
        assert_validation!("", "query($foo:Int){x}", bjson!({"foo":"2"}));
        assert_validation_error!("", "query($foo:Int){x}", bjson!({"foo":true}));
        assert_validation_error!("", "query($foo:Int){x}", bjson!({"foo":{}}));
        assert_validation_error!(
            "",
            "query($foo:Int){x}",
            bjson!({ "foo": i32::MAX as i64 + 1 })
        );
        assert_validation_error!(
            "",
            "query($foo:Int){x}",
            bjson!({ "foo": i32::MIN as i64 - 1 })
        );
        assert_validation!("", "query($foo:Int){x}", bjson!({ "foo": i32::MAX }));
        assert_validation!("", "query($foo:Int){x}", bjson!({ "foo": i32::MIN }));
        assert_validation!("", "query($foo:ID){x}", bjson!({"foo": "1"}));
        assert_validation!("", "query($foo:ID){x}", bjson!({"foo": 1}));
        assert_validation_error!("", "query($foo:ID){x}", bjson!({"foo": true}));
        assert_validation_error!("", "query($foo:ID){x}", bjson!({"foo": {}}));
        assert_validation!("", "query($foo:String){x}", bjson!({"foo": "str"}));
        assert_validation!("", "query($foo:Float){x}", bjson!({"foo":2.0}));
        assert_validation!("", "query($foo:Float){x}", bjson!({"foo":"2.0"}));
        assert_validation_error!("", "query($foo:Float){x}", bjson!({"foo":2}));
        assert_validation_error!("", "query($foo:Int!){x}", bjson!({}));
        assert_validation!("", "query($foo:[Int]){x}", bjson!({}));
        assert_validation_error!("", "query($foo:[Int]){x}", bjson!({"foo":1}));
        assert_validation_error!("", "query($foo:[Int]){x}", bjson!({"foo":"str"}));
        assert_validation_error!("", "query($foo:[Int]){x}", bjson!({"foo":{}}));
        assert_validation_error!("", "query($foo:[Int]!){x}", bjson!({}));
        assert_validation!("", "query($foo:[Int]!){x}", bjson!({"foo":[]}));
        assert_validation!("", "query($foo:[Int]){x}", bjson!({"foo":[1,2,3]}));
        assert_validation_error!("", "query($foo:[Int]){x}", bjson!({"foo":["f","o","o"]}));
        assert_validation!("", "query($foo:[Int]){x}", bjson!({"foo":["1","2","3"]}));
        assert_validation!("", "query($foo:[String]){x}", bjson!({"foo":["1","2","3"]}));
        assert_validation_error!("", "query($foo:[String]){x}", bjson!({"foo":[1,2,3]}));
        assert_validation!("", "query($foo:[Int!]){x}", bjson!({"foo":[1,2,3]}));
        assert_validation_error!("", "query($foo:[Int!]){x}", bjson!({"foo":[1,null,3]}));
        assert_validation!("", "query($foo:[Int]){x}", bjson!({"foo":[1,null,3]}));
        assert_validation!("type Foo{}", "query($foo:Foo){x}", bjson!({}));
        assert_validation!("type Foo{}", "query($foo:Foo){x}", bjson!({"foo":{}}));
        assert_validation_error!("type Foo{}", "query($foo:Foo){x}", bjson!({"foo":1}));
        assert_validation_error!("type Foo{}", "query($foo:Foo){x}", bjson!({"foo":"str"}));
        assert_validation_error!("type Foo{x:Int!}", "query($foo:Foo){x}", bjson!({"foo":{}}));
        assert_validation!(
            "type Foo{x:Int!}",
            "query($foo:Foo){x}",
            bjson!({"foo":{"x":1}})
        );
        assert_validation!(
            "type Foo implements Bar interface Bar{x:Int!}",
            "query($foo:Foo){x}",
            bjson!({"foo":{"x":1}}),
        );
        assert_validation_error!(
            "type Foo implements Bar interface Bar{x:Int!}",
            "query($foo:Foo){x}",
            bjson!({"foo":{"x":"str"}}),
        );
        assert_validation_error!(
            "type Foo implements Bar interface Bar{x:Int!}",
            "query($foo:Foo){x}",
            bjson!({"foo":{}}),
        );
        assert_validation!("scalar Foo", "query($foo:Foo!){x}", bjson!({"foo":{}}));
        assert_validation!("scalar Foo", "query($foo:Foo!){x}", bjson!({"foo":1}));
        assert_validation_error!("scalar Foo", "query($foo:Foo!){x}", bjson!({}));
        assert_validation!(
            "type Foo{bar:Bar!} type Bar{x:Int!}",
            "query($foo:Foo){x}",
            bjson!({"foo":{"bar":{"x":1}}})
        );
    }
}
