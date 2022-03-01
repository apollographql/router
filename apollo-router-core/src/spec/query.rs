use crate::{fetch::OperationKind, prelude::graphql::*};
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
        if let Value::Object(mut input) = data {
            let operation = match operation_name {
                Some(name) => self
                    .operations
                    .iter()
                    // we should have an error if the only operation is anonymous but the query specifies a name
                    .find(|op| op.name.is_some() && op.name.as_deref().unwrap() == name),
                None => self.operations.get(0),
            };

            if let Some(operation) = operation {
                let mut output = Object::default();

                response.data =
                    match self.apply_root_selection_set(operation, &mut input, &mut output, schema)
                    {
                        Ok(()) => output.into(),
                        Err(InvalidValue) => Value::Null,
                    }
            } else {
                failfast_debug!("can't find operation for {:?}", operation_name);
            }
        } else {
            failfast_debug!("Invalid type for data in response.");
        }
    }

    #[tracing::instrument(skip_all, level = "trace")]
    pub fn parse(query: impl Into<String>, schema: &Schema) -> Option<Self> {
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
        let fragments = Fragments::from_ast(&document, schema)?;

        let operations = document
            .definitions()
            .filter_map(|definition| {
                if let ast::Definition::OperationDefinition(operation) = definition {
                    Operation::from_ast(operation, schema)
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

    fn format_value(
        &self,
        field_type: &FieldType,
        input: Value,
        selection_set: &[Selection],
        schema: &Schema,
    ) -> Result<Value, InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            FieldType::NonNull(inner_type) => {
                match self.format_value(inner_type, input, selection_set, schema) {
                    Err(_) => Err(InvalidValue),
                    Ok(Value::Null) => Err(InvalidValue),
                    Ok(value) => Ok(value),
                }
            }

            // if the list contains nonnullable types, we will receive a Err(InvalidValue)
            // and should replace the entire list with null
            // if the types are nullable, the inner call to filter_errors will take care
            // of setting the current entry to null
            FieldType::List(inner_type) => match input {
                Value::Array(input_array) => {
                    match input_array
                        .into_iter()
                        .map(|element| {
                            self.format_value(inner_type, element, selection_set, schema)
                        })
                        .collect()
                    {
                        Err(InvalidValue) => Ok(Value::Null),
                        Ok(value) => Ok(value),
                    }
                }
                _ => Ok(Value::Null),
            },

            FieldType::Named(type_name) => {
                // we cannot know about the expected format of custom scalars
                // so we must pass them directly to the client
                if schema.custom_scalars.contains(type_name) {
                    return Ok(input);
                } else if let Some(enum_type) = schema.enums.get(type_name) {
                    return match input.as_str() {
                        Some(s) => {
                            if enum_type.contains(s) {
                                Ok(input)
                            } else {
                                Ok(Value::Null)
                            }
                        }
                        None => Ok(Value::Null),
                    };
                }

                match input {
                    Value::Object(mut input_object) => {
                        let mut output_object = Object::default();

                        match self.apply_selection_set(
                            selection_set,
                            &mut input_object,
                            &mut output_object,
                            schema,
                        ) {
                            Ok(()) => Ok(Value::Object(output_object)),
                            Err(InvalidValue) => Ok(Value::Null),
                        }
                    }
                    _ => Ok(Value::Null),
                }
            }

            // the rest of the possible types just need to validate the expected value
            FieldType::Int => {
                let opt = if input.is_i64() {
                    input.as_i64().and_then(|i| i32::try_from(i).ok())
                } else if input.is_u64() {
                    input.as_i64().and_then(|i| i32::try_from(i).ok())
                } else {
                    None
                };

                // if the value is invalid, we do not insert it in the output object
                // which is equivalent to inserting null
                if opt.is_some() {
                    return Ok(input);
                }
                Ok(Value::Null)
            }
            FieldType::Float => {
                if input.as_f64().is_some() {
                    return Ok(input);
                }
                Ok(Value::Null)
            }
            FieldType::Boolean => {
                if input.as_bool().is_some() {
                    return Ok(input);
                }
                Ok(Value::Null)
            }
            FieldType::String => {
                if input.as_str().is_some() {
                    return Ok(input);
                }
                Ok(Value::Null)
            }
            FieldType::Id => {
                if input.is_string() || input.is_i64() || input.is_u64() || input.is_f64() {
                    return Ok(input);
                }
                Ok(Value::Null)
            }
        }
    }

    fn apply_selection_set(
        &self,
        selection_set: &[Selection],
        input: &mut Object,
        output: &mut Object,
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    selection_set,
                    field_type,
                } => {
                    if let Some((field_name, input_value)) = input.remove_entry(name.as_str()) {
                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let value =
                            self.format_value(field_type, input_value, selection_set, schema)?;
                        output.insert(field_name, value);
                    } else if field_type.is_non_null() {
                        return Err(InvalidValue);
                    }
                }
                Selection::InlineFragment {
                    fragment:
                        Fragment {
                            type_condition,
                            selection_set,
                        },
                } => {
                    if let Some(typename) = input.get("__typename") {
                        if typename.as_str() == Some(type_condition.as_str()) {
                            self.apply_selection_set(selection_set, input, output, schema)?;
                        }
                    } else {
                        return Err(InvalidValue);
                    }
                }
                Selection::FragmentSpread { name } => {
                    if let Some(fragment) = self
                        .fragments
                        .get(name)
                        .or_else(|| schema.fragments.get(name))
                    {
                        if let Some(typename) = input.get("__typename") {
                            if typename.as_str() == Some(fragment.type_condition.as_str()) {
                                self.apply_selection_set(
                                    &fragment.selection_set,
                                    input,
                                    output,
                                    schema,
                                )?;
                            }
                        } else {
                            return Err(InvalidValue);
                        }
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("Missing fragment named: {}", name);
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_root_selection_set(
        &self,
        operation: &Operation,
        input: &mut Object,
        output: &mut Object,
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        for selection in &operation.selection_set {
            match selection {
                Selection::Field {
                    name,
                    selection_set,
                    field_type,
                } => {
                    if let Some((field_name, input_value)) = input.remove_entry(name.as_str()) {
                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let value =
                            self.format_value(field_type, input_value, selection_set, schema)?;
                        output.insert(field_name, value);
                    } else if field_type.is_non_null() {
                        return Err(InvalidValue);
                    }
                }
                Selection::InlineFragment {
                    fragment:
                        Fragment {
                            type_condition,
                            selection_set,
                        },
                } => {
                    // top level objects will not provide a __typename field
                    match (type_condition.as_str(), operation.kind) {
                        ("Query", OperationKind::Query) | ("Mutation", OperationKind::Mutation) => {
                        }
                        _ => {
                            return Err(InvalidValue);
                        }
                    }
                    self.apply_selection_set(selection_set, input, output, schema)?;
                }
                Selection::FragmentSpread { name } => {
                    if let Some(fragment) = self
                        .fragments
                        .get(name)
                        .or_else(|| schema.fragments.get(name))
                    {
                        // top level objects will not provide a __typename field
                        match (fragment.type_condition.as_str(), operation.kind) {
                            ("Query", OperationKind::Query)
                            | ("Mutation", OperationKind::Mutation) => {}
                            _ => {
                                return Err(InvalidValue);
                            }
                        }
                        self.apply_selection_set(&fragment.selection_set, input, output, schema)?;
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("Missing fragment named: {}", name);
                    }
                }
            }
        }

        Ok(())
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
    kind: OperationKind,
    selection_set: Vec<Selection>,
    variables: HashMap<String, FieldType>,
}

impl Operation {
    // Spec: https://spec.graphql.org/draft/#sec-Language.Operations
    fn from_ast(operation: ast::OperationDefinition, schema: &Schema) -> Option<Self> {
        let name = operation.name().map(|x| x.text().to_string());

        let kind = operation
            .operation_type()
            .and_then(|op| {
                op.query_token()
                    .map(|_| OperationKind::Query)
                    .or_else(|| op.mutation_token().map(|_| OperationKind::Mutation))
                    .or_else(|| op.subscription_token().map(|_| OperationKind::Subscription))
            })
            .unwrap_or(OperationKind::Query);

        let current_field_type = match kind {
            OperationKind::Query => FieldType::Named("Query".to_string()),
            OperationKind::Mutation => FieldType::Named("Mutation".to_string()),
            OperationKind::Subscription => return None,
        };

        let selection_set = operation
            .selection_set()
            .expect("the node SelectionSet is not optional in the spec; qed")
            .selections()
            .map(|selection| Selection::from_ast(selection, &current_field_type, schema))
            .collect::<Option<_>>()?;

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

        Some(Operation {
            selection_set,
            name,
            variables,
            kind,
        })
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
    use serde_json_bytes::json;
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
            let query = Query::parse($query, &schema).expect("could not parse query");
            let mut response = Response::builder().data($response.clone()).build();
            query.format_response(&mut response, $operation, &schema);
            assert_eq_and_ordered!(response.data, $expected);
        }};
    }

    #[test]
    fn reformat_response_data_field() {
        assert_format_response!(
            "type Query {
                foo: String
                stuff: Bar
                array: [Bar]
                baz: String
            }
            type Bar {
                bar: String
                baz: String
            }",
            "query Test {
                foo
                stuff{bar __typename }
                array{bar}
                baz
                alias:baz
                alias_obj:stuff{bar}
                alias_array:array{bar}
            }",
            json! {{
                "foo": "1",
                "stuff": {"bar": "2", "__typename": "Bar"},
                "array": [{"bar": "3", "baz": "4"}, {"bar": "5", "baz": "6"}],
                "baz": "7",
                "alias": "7",
                "alias_obj": {"bar": "8"},
                "alias_array": [{"bar": "9", "baz": "10"}, {"bar": "11", "baz": "12"}],
                "other": "13",
            }},
            Some("Test"),
            json! {{
                "foo": "1",
                "stuff": {
                    "bar": "2",
                    "__typename": "Bar",
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
        let schema = "type Query {
            get: Test
            getStuff: Stuff
          }
  
          type Stuff {
              stuff: Bar
          }
          type Bar {
              bar: String
          }
          type Thing {
              id: String
          }
          union Test = Stuff | Thing";
        let query = "{ get { ... on Stuff { stuff{bar}} ... on Thing { id }} }";
        assert_format_response!(
            schema,
            query,
            json! {
                {"get": {"__typename": "Stuff", "id": "1", "stuff": {"bar": "2"}}}
            },
            None,
            json! {{
                "get": {
                    "stuff": {
                        "bar": "2",
                    },
                }
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {
                {"get": {"__typename": "Thing", "id": "1", "stuff": {"bar": "2"}}}
            },
            None,
            json! {{
                "get": {
                    "id": "1",
                }
            }},
        );

        // when using a fragment on an operation exported by a subgraph,
        // we might not get a __typename field, we should instead be able
        // to know the type in advance
        assert_format_response!(
            schema,
            "{ getStuff { ... on Stuff { stuff{bar}} ... on Thing { id }} }",
            json! {
                {"getStuff": { "stuff": {"bar": "2"}}}
            },
            None,
            json! {{
                "get": {
                    "stuff": {"bar": "2"},
                }
            }},
        );
    }

    #[test]
    fn reformat_response_data_fragment_spread() {
        let schema = "type Query {
          thing: Thing    
        }

        type Foo {
            foo: String
        }
        type Bar {
            bar: String
        }
        type Baz {
            baz: String
        }
        union Thing = Foo
        extend union Thing = Bar | Baz

        fragment baz on Baz {baz}";
        let query = "query { thing {...foo ...bar ...baz} } fragment foo on Foo {foo} fragment bar on Bar {bar}";

        assert_format_response!(
            schema,
            query,
            json! {
                {"thing": {"__typename": "Foo", "foo": "1", "bar": "2", "baz": "3"}}
            },
            None,
            json! {
                {"thing": {"foo": "1"}}
            },
        );
        assert_format_response!(
            schema,
            query,
            json! {
                {"thing": {"__typename": "Bar", "foo": "1", "bar": "2", "baz": "3"}}
            },
            None,
            json! {
                {"thing": {"bar": "2"} }
            },
        );
        assert_format_response!(
            schema,
            query,
            json! {
                {"thing": {"__typename": "Baz", "foo": "1", "bar": "2", "baz": "3"}}
            },
            None,
            json! {
                {"thing": {"baz": "3"} }
            },
        );
    }

    #[test]
    fn reformat_response_data_best_effort() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                foo: String
                stuff: Baz
                array: [Element]
                other: Bar
            }

            type Baz {
                bar: String
                baz: String
            }

            type Bar {
                bar: String
            }

            union Element = Baz | Bar | String
            ",
            "{get {foo stuff{bar baz} ...fragment array{bar baz} other{bar}}}",
            json! {{
                "get": {
                    "foo": "1",
                    "stuff": {"baz": "2"},
                    "array": [
                        {"baz": "3"},
                        "4",
                        {"bar": "5"},
                    ],
                    "other": "6",
                },
                "should_be_removed": {
                    "aaa": 2
                },
            }},
            None,
            json! {{
                "get": {
                    "foo": "1",
                    "stuff": {
                        "baz": "2",
                    },
                    "array": [
                        {},
                        null,
                        {},
                    ],
                    "other": null,
                },
            }},
        );
    }

    #[test]
    fn reformat_matching_operation() {
        let schema = "
        type Query {
            foo: String
            other: String
        }
        ";
        let query = "query MyOperation { foo }";
        let response = json! {{
            "foo": "1",
            "other": "2",
        }};
        // an invalid operation name should fail
        assert_format_response!(schema, query, response, Some("OtherOperation"), Value::Null,);
        assert_format_response!(
            schema,
            query,
            response,
            Some("MyOperation"),
            json! {{
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
            let query = Query::parse(
                request
                    .query
                    .as_ref()
                    .expect("query has been added right above; qed"),
                &schema,
            )
            .expect("could not parse query");
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
        let schema = "type Query { x: String }";
        assert_validation!(schema, "query($foo:Boolean){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({}));
        assert_validation!(schema, "query($foo:Boolean!){x}", json!({"foo":true}));
        assert_validation!(schema, "query($foo:Boolean!){x}", json!({"foo":"true"}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo":"str"}));
        assert_validation!(schema, "query($foo:Int){x}", json!({}));
        assert_validation!(schema, "query($foo:Int){x}", json!({"foo":2}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":2.0}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":"str"}));
        assert_validation!(schema, "query($foo:Int){x}", json!({"foo":"2"}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":{}}));
        assert_validation_error!(
            schema,
            "query($foo:Int){x}",
            json!({ "foo": i32::MAX as i64 + 1 })
        );
        assert_validation_error!(
            schema,
            "query($foo:Int){x}",
            json!({ "foo": i32::MIN as i64 - 1 })
        );
        assert_validation!(schema, "query($foo:Int){x}", json!({ "foo": i32::MAX }));
        assert_validation!(schema, "query($foo:Int){x}", json!({ "foo": i32::MIN }));
        assert_validation!(schema, "query($foo:ID){x}", json!({"foo": "1"}));
        assert_validation!(schema, "query($foo:ID){x}", json!({"foo": 1}));
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": true}));
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": {}}));
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "str"}));
        assert_validation!(schema, "query($foo:Float){x}", json!({"foo":2.0}));
        assert_validation!(schema, "query($foo:Float){x}", json!({"foo":"2.0"}));
        assert_validation_error!(schema, "query($foo:Float){x}", json!({"foo":2}));
        assert_validation_error!(schema, "query($foo:Int!){x}", json!({}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":1}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":"str"}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":{}}));
        assert_validation_error!(schema, "query($foo:[Int]!){x}", json!({}));
        assert_validation!(schema, "query($foo:[Int]!){x}", json!({"foo":[]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":["f","o","o"]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":["1","2","3"]}));
        assert_validation!(
            schema,
            "query($foo:[String]){x}",
            json!({"foo":["1","2","3"]})
        );
        assert_validation_error!(schema, "query($foo:[String]){x}", json!({"foo":[1,2,3]}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!(schema, "query($foo:[Int!]){x}", json!({"foo":[1,null,3]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":[1,null,3]}));
        assert_validation!(
            "type Foo{} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({})
        );
        assert_validation!(
            "type Foo{} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}})
        );
        assert_validation_error!(
            "type Foo{} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":1})
        );
        assert_validation_error!(
            "type Foo{} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":"str"})
        );
        assert_validation_error!(
            "type Foo{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}})
        );
        assert_validation!(
            "type Foo{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"x":1}})
        );
        assert_validation!(
            "type Foo implements Bar interface Bar{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"x":1}}),
        );
        assert_validation_error!(
            "type Foo implements Bar interface Bar{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"x":"str"}}),
        );
        assert_validation_error!(
            "type Foo implements Bar interface Bar{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}}),
        );
        assert_validation!(
            "scalar Foo type Query { x: String }",
            "query($foo:Foo!){x}",
            json!({"foo":{}})
        );
        assert_validation!(
            "scalar Foo type Query { x: String }",
            "query($foo:Foo!){x}",
            json!({"foo":1})
        );
        assert_validation_error!(
            "scalar Foo type Query { x: String }",
            "query($foo:Foo!){x}",
            json!({})
        );
        assert_validation!(
            "type Foo{bar:Bar!} type Bar{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"bar":{"x":1}}})
        );
    }

    #[test]
    fn filter_root_errors() {
        let schema = "type Query {
            getInt: Int
            getNonNullString: String!
        }";
        let query = "query MyOperation { getInt }";
        let response = json! {{
            "getInt": "1",
            "other": "2",
        }};

        assert_format_response!(
            schema,
            query,
            response,
            Some("MyOperation"),
            json! {{
                "getInt": null,
            }},
        );

        let query = "query { getNonNullString }";
        let response = json! {{
            "getNonNullString": 1,
        }};

        assert_format_response!(schema, query, response, None, Value::Null,);
    }

    #[test]
    fn filter_object_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            name: String
        }";
        let query = "query  { me { id name } }";

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "name": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": 1,
                    "name": 1,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": null,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": { },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name": 1,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        // a non null field not present in the query should not be an error
        assert_format_response!(
            schema,
            "query  { me { name } }",
            json! {{
                "me": {
                    "name": "a",
                },
            }},
            None,
            json! {{
                "me": {
                    "name": "a",
                },
            }},
        );

        // if a field appears multiple times, selection should be deduplicated
        assert_format_response!(
            schema,
            "query  { me { id id } }",
            json! {{
                "me": {
                    "id": "a",
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                },
            }},
        );
    }

    #[test]
    fn filter_list_errors() {
        let schema = "type Query {
            list: TestList
        }

        type TestList {
            l1: [String]
            l2: [String!]
            l3: [String]!
            l4: [String!]!
        }";

        assert_format_response!(
            schema,
            "query { list { l1 } }",
            json! {{
                "list": {
                    "l1": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                    "name": 1,
                },
            }},
            None,
            json! {{
                "list": {
                    "l1": ["abc", null, null, null, "def"],
                },
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l1 } }",
            json! {{
                "list": {
                    "l1": "abc",
                },
            }},
            None,
            json! {{
                "list": {
                    "l1": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l2 } }",
            json! {{
                "list": {
                    "l2": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                    "name": 1,
                },
            }},
            None,
            json! {{
                "list": {
                    "l2": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l2 } }",
            json! {{
                "list": {
                    "l2": ["abc", "def"],
                    "name": 1,
                },
            }},
            None,
            json! {{
                "list": {
                    "l2": ["abc", "def"],
                },
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l3 } }",
            json! {{
                "list": {
                    "l3": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                    "name": 1,
                },
            }},
            None,
            json! {{
                "list": {
                    "l3": ["abc", null, null, null, "def"],
                },
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l3 } }",
            json! {{
                "list": {
                    "l3": 1,
                },
            }},
            None,
            json! {{
                "list":null,
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l4 } }",
            json! {{
                "list": {
                    "l4": ["abc", 1, { "foo": "bar"}, ["aaa"], "def"],
                },
            }},
            None,
            json! {{
                "list": null,
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l4 } }",
            json! {{
                "list": {
                    "l4": ["abc", "def"],
                },
            }},
            None,
            json! {{
                "list": {
                    "l4": ["abc", "def"],
                },
            }},
        );

        assert_format_response!(
            schema,
            "query { list { l4 } }",
            json! {{
                "list": {
                    "l4": 1,
                },
            }},
            None,
            json! {{
                "list": null,
            }},
        );
    }

    #[test]
    fn filter_nested_object_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            name: String
            reviews1: [Review]
            reviews2: [Review!]
            reviews3: [Review!]!
        }
        
        type Review {
            text1: String
            text2: String!
        }
        ";

        let query_review1_text1 = "query  { me { id reviews1 { text1 } } }";
        assert_format_response!(
            schema,
            query_review1_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ { } ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review1_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text1": null } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ { "text1": null } ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review1_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text1": 1 } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ { "text1": null } ],
                },
            }},
        );

        let query_review1_text2 = "query  { me { id reviews1 { text2 } } }";
        assert_format_response!(
            schema,
            query_review1_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ null ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review1_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text2": null } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ null ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review1_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews1": [ { "text2": 1 } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews1": [ null ],
                },
            }},
        );

        // reviews2: [Review!]
        let query_review2_text1 = "query  { me { id reviews2 { text1 } } }";
        assert_format_response!(
            schema,
            query_review2_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews2": [ { } ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review2_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text1": null } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews2": [ { "text1": null } ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review2_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text1": 1 } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews2": [ { "text1": null } ],
                },
            }},
        );

        let query_review2_text2 = "query  { me { id reviews2 { text2 } } }";
        assert_format_response!(
            schema,
            query_review2_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews2": null,
                },
            }},
        );
        assert_format_response!(
            schema,
            query_review2_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text2": null } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews2": null,
                },
            }},
        );
        assert_format_response!(
            schema,
            query_review2_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews2": [ { "text2": 1 } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews2": null,
                },
            }},
        );

        //reviews3: [Review!]!
        let query_review3_text1 = "query  { me { id reviews3 { text1 } } }";
        assert_format_response!(
            schema,
            query_review3_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews3": [ { } ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review3_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text1": null } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews3": [ { "text1": null } ],
                },
            }},
        );

        assert_format_response!(
            schema,
            query_review3_text1,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text1": 1 } ],
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "reviews3": [ { "text1": null } ],
                },
            }},
        );

        let query_review3_text2 = "query  { me { id reviews3 { text2 } } }";
        assert_format_response!(
            schema,
            query_review3_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { } ],
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );
        assert_format_response!(
            schema,
            query_review3_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text2": null } ],
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );
        assert_format_response!(
            schema,
            query_review3_text2,
            json! {{
                "me": {
                    "id": "a",
                    "name": 1,
                    "reviews3": [ { "text2": 1 } ],
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );
    }

    #[test]
    fn filter_alias_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            name: String
        }";
        let query = "query  { me { id identifiant:id } }";

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                    "identifiant": "b",
                },
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "identifiant": "b",
                },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                    "identifiant": 1,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                    "identifiant": null,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        let query2 = "query  { me { name name2:name } }";

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name": "a",
                    "name2": "b",
                },
            }},
            None,
            json! {{
                "me": {
                    "name": "a",
                    "name2": "b",
                },
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name": "a",
                    "name2": 1,
                },
            }},
            None,
            json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }},
            None,
            json! {{
                "me": {
                    "name": "a",
                    "name2": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name": "a",
                },
            }},
            None,
            json! {{
                "me": {
                    "name": "a",
                },
            }},
        );
    }

    #[test]
    fn filter_scalar_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            a: A
            b: A!
        }
        
        scalar A
        ";

        let query = "query  { me { id a } }";

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "id": "a",
                    "a": {
                        "field": 1234,
                    },
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "a": {
                        "field": 1234,
                    },
                },
            }},
        );

        let query2 = "query  { me { id b } }";

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "id": "a",
                    "b": "hello",
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "b": "hello",
                },
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "id": "a",
                    "b": {
                        "field": 1234,
                    },
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "b": {
                        "field": 1234,
                    },
                },
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "id": "a",
                    "b": null,
                }
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "id": "a",
                }
            }},
            None,
            json! {{
                "me": null,
            }},
        );
    }

    #[test]
    fn filter_enum_errors() {
        let schema = "type Query {
            me: User
        }

        type User {
            id: String!
            a: A
            b: A!
        }

        enum A {
            X
            Y
            Z
        }";

        let query_a = "query  { me { id a } }";

        assert_format_response!(
            schema,
            query_a,
            json! {{
                "me": {
                    "id": "a",
                    "a": "X",
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "a": "X",
                },
            }},
        );

        assert_format_response!(
            schema,
            query_a,
            json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "a": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            query_a,
            json! {{
                "me": {
                    "id": "a",
                    "a": null,
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "a": null,
                },
            }},
        );

        let query_b = "query  { me { id b } }";

        assert_format_response!(
            schema,
            query_b,
            json! {{
                "me": {
                    "id": "a",
                    "b": "X",
                }
            }},
            None,
            json! {{
                "me": {
                    "id": "a",
                    "b": "X",
                },
            }},
        );

        assert_format_response!(
            schema,
            query_b,
            json! {{
                "me": {
                    "id": "a",
                    "a": "hello",
                }
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query_b,
            json! {{
                "me": {
                    "id": "a",
                    "a": null,
                }
            }},
            None,
            json! {{
                "me": null,
            }},
        );
    }

    #[test]
    fn filter_interface_errors() {
        let schema = "type Query {
            me: NamedEntity
        }

        interface NamedEntity {
            name: String
            name2: String!
        }

        type User implements NamedEntity {
            name: String
            surname: String!
        }

        type User2 implements NamedEntity {
            name: String
            surname: String!
        }
        ";

        let query = "query  { me { name } }";

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name": "a",
                },
            }},
            None,
            json! {{
                "me": {
                    "name": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": { },
            }},
            None,
            json! {{
                "me": { },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name": null,
                },
            }},
            None,
            json! {{
                "me": {
                    "name": null,
                },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name": 1,
                },
            }},
            None,
            json! {{
                "me": {
                    "name": null,
                },
            }},
        );

        let query2 = "query  { me { name2 } }";

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name2": "a",
                },
            }},
            None,
            json! {{
                "me": {
                    "name2": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": { },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name2": null,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {{
                "me": {
                    "name2": 1,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );
    }

    #[test]
    fn filter_extended_interface_errors() {
        let schema = "type Query {
            me: NamedEntity
        }

        interface NamedEntity {
            name: String
        }

        type User implements NamedEntity {
            name: String
        }

        type User2 implements NamedEntity {
            name: String
        }

        extend interface NamedEntity {
            name2: String!
        }

        extend type User {
            name2: String!
        }

        extend type User2 {
            name2: String!
        }
        ";

        let query = "query  { me { name2 } }";

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name2": "a",
                },
            }},
            None,
            json! {{
                "me": {
                    "name2": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name2": null,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": { },
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {{
                "me": {
                    "name2": 1,
                },
            }},
            None,
            json! {{
                "me": null,
            }},
        );
    }

    #[test]
    fn filter_errors_top_level_fragment() {
        let schema = "type Query {
            get: Thing   
          }
  
          type Thing {
              name: String
              name2: String!
          }";

        let query = "{ ...frag } fragment frag on Query { __typename get { name } }";
        assert_format_response!(
            schema,
            query,
            json! {
                { "get": {"name": "a", "other": "b"} }
            },
            None,
            json! {{
                "get": {
                    "name": "a",
                }
            }},
        );

        assert_format_response!(
            schema,
            query,
            json! {
                { "get": {"name": null, "other": "b"} }
            },
            None,
            json! {{
                "get": {
                    "name": null,
                }
            }},
        );

        let query2 = "{ ...frag2 } fragment frag2 on Query { __typename get { name2 } }";
        assert_format_response!(
            schema,
            query2,
            json! {
                { "get": {"name2": "a", "other": "b"} }
            },
            None,
            json! {{
                "get": {
                    "name2": "a",
                }
            }},
        );

        assert_format_response!(
            schema,
            query2,
            json! {
                { "get": {"name2": null, "other": "b"} }
            },
            None,
            json! {{
                "get": null
            }},
        );

        let query3 = "{ ... on Query { __typename get { name } } }";
        assert_format_response!(
            schema,
            query3,
            json! {
                { "get": {"name": "a", "other": "b"} }
            },
            None,
            json! {{
                "get": {
                    "name": "a",
                }
            }},
        );

        assert_format_response!(
            schema,
            query3,
            json! {
                { "get": {"name": null, "other": "b"} }
            },
            None,
            json! {{
                "get": {
                    "name": null,
                }
            }},
        );

        let query4 = "{ ... on Query { __typename get { name2 } } }";
        assert_format_response!(
            schema,
            query4,
            json! {
                { "get": {"name2": "a", "other": "b"} }
            },
            None,
            json! {{
                "get": {
                    "name2": "a",
                }
            }},
        );

        assert_format_response!(
            schema,
            query4,
            json! {
                { "get": {"name2": null, "other": "b"} }
            },
            None,
            json! {{
                "get": null,
            }},
        );
    }
}
