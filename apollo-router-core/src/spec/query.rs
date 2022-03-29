use crate::{fetch::OperationKind, prelude::graphql::*};
use apollo_parser::ast;
use derivative::Derivative;
use serde_json_bytes::ByteString;
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
        variables: Object,
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

                let all_variables = if operation.variables.is_empty() {
                    variables
                } else {
                    operation
                        .variables
                        .iter()
                        .filter_map(|(k, (_, opt))| opt.as_ref().map(|v| (k, v)))
                        .chain(variables.iter())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };

                response.data = match self.apply_root_selection_set(
                    operation,
                    &all_variables,
                    &mut input,
                    &mut output,
                    schema,
                ) {
                    Ok(()) => output.into(),
                    Err(InvalidValue) => Value::Null,
                }
            } else {
                failfast_debug!("can't find operation for {:?}", operation_name);
            }
        } else {
            failfast_debug!("invalid type for data in response.");
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
            failfast_debug!("parsing error(s): {}", errors.join(", "));
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

        Some(Query {
            string,
            fragments,
            operations,
        })
    }

    fn format_value(
        &self,
        field_type: &FieldType,
        variables: &Object,
        input: &mut Value,
        output: &mut Value,
        selection_set: &[Selection],
        schema: &Schema,
    ) -> Result<(), InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            FieldType::NonNull(inner_type) => {
                match self.format_value(inner_type, variables, input, output, selection_set, schema)
                {
                    Err(_) => Err(InvalidValue),
                    Ok(_) => {
                        if output.is_null() {
                            Err(InvalidValue)
                        } else {
                            Ok(())
                        }
                    }
                }
            }

            // if the list contains nonnullable types, we will receive a Err(InvalidValue)
            // and should replace the entire list with null
            // if the types are nullable, the inner call to filter_errors will take care
            // of setting the current entry to null
            FieldType::List(inner_type) => match input {
                Value::Array(input_array) => {
                    if output.is_null() {
                        *output = Value::Array(
                            std::iter::repeat(Value::Null)
                                .take(input_array.len())
                                .collect(),
                        );
                    }
                    let output_array = output.as_array_mut().ok_or(InvalidValue)?;
                    match input_array
                        .iter_mut()
                        .enumerate()
                        .try_for_each(|(i, element)| {
                            self.format_value(
                                inner_type,
                                variables,
                                element,
                                &mut output_array[i],
                                selection_set,
                                schema,
                            )
                        }) {
                        Err(InvalidValue) => {
                            *output = Value::Null;
                            Ok(())
                        }
                        Ok(()) => Ok(()),
                    }
                }
                _ => Ok(()),
            },

            FieldType::Named(type_name) => {
                // we cannot know about the expected format of custom scalars
                // so we must pass them directly to the client
                if schema.custom_scalars.contains(type_name) {
                    *output = input.take();
                    return Ok(());
                } else if let Some(enum_type) = schema.enums.get(type_name) {
                    return match input.as_str() {
                        Some(s) => {
                            if enum_type.contains(s) {
                                *output = input.take();
                                Ok(())
                            } else {
                                *output = Value::Null;
                                Ok(())
                            }
                        }
                        None => {
                            *output = Value::Null;
                            Ok(())
                        }
                    };
                }

                match input {
                    Value::Object(ref mut input_object) => {
                        if output.is_null() {
                            *output = Value::Object(Object::default());
                        }
                        let output_object = output.as_object_mut().ok_or(InvalidValue)?;

                        match self.apply_selection_set(
                            selection_set,
                            variables,
                            input_object,
                            output_object,
                            schema,
                        ) {
                            Ok(()) => Ok(()),
                            Err(InvalidValue) => {
                                *output = Value::Null;
                                Ok(())
                            }
                        }
                    }
                    _ => {
                        *output = Value::Null;
                        Ok(())
                    }
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
                    *output = input.take();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Float => {
                if input.as_f64().is_some() {
                    *output = input.take();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Boolean => {
                if input.as_bool().is_some() {
                    *output = input.take();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::String => {
                if input.as_str().is_some() {
                    *output = input.take();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Id => {
                if input.is_string() || input.is_i64() || input.is_u64() || input.is_f64() {
                    *output = input.take();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
        }
    }

    fn apply_selection_set(
        &self,
        selection_set: &[Selection],
        variables: &Object,
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
                    skip,
                    include,
                } => {
                    if skip
                        .should_skip(variables)
                        // validate_variables should have already checked that
                        // the variable is present and it is of the correct type
                        .unwrap_or(false)
                    {
                        continue;
                    }

                    if !include
                        .should_include(variables)
                        // validate_variables should have already checked that
                        // the variable is present and it is of the correct type
                        .unwrap_or(true)
                    {
                        continue;
                    }

                    if let Some(input_value) = input.get_mut(name.as_str()) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was moved into output using take(), replacing
                        // the input value with Null
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null
                        if input_value.is_null() && output.contains_key(name.as_str()) {
                            continue;
                        }
                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let output_value = output.entry((*name).clone()).or_insert(Value::Null);
                        self.format_value(
                            field_type,
                            variables,
                            input_value,
                            output_value,
                            selection_set,
                            schema,
                        )?;
                    } else {
                        if !output.contains_key(name.as_str()) {
                            output.insert((*name).clone(), Value::Null);
                        }
                        if field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    }
                }
                Selection::InlineFragment {
                    fragment:
                        Fragment {
                            type_condition,
                            selection_set,
                            skip,
                            include,
                        },
                    known_type,
                } => {
                    if skip
                        .should_skip(variables)
                        // validate_variables should have already checked that
                        // the variable is present and it is of the correct type
                        .unwrap_or(false)
                    {
                        continue;
                    }

                    if !include
                        .should_include(variables)
                        // validate_variables should have already checked that
                        // the variable is present and it is of the correct type
                        .unwrap_or(true)
                    {
                        continue;
                    }

                    // known_type = true means that from the query's shape, we know
                    // we should get the right type here. But in the case we get a
                    // __typename field and it does not match, we should not apply
                    // that fragment
                    if input
                        .get("__typename")
                        .map(|val| val.as_str() == Some(type_condition.as_str()))
                        .unwrap_or(*known_type)
                    {
                        self.apply_selection_set(selection_set, variables, input, output, schema)?;
                    }
                }
                Selection::FragmentSpread {
                    name,
                    known_type,
                    skip,
                    include,
                } => {
                    if skip
                        .should_skip(variables)
                        // validate_variables should have already checked that
                        // the variable is present and it is of the correct type
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    if !include
                        .should_include(variables)
                        // validate_variables should have already checked that
                        // the variable is present and it is of the correct type
                        .unwrap_or(true)
                    {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
                        if fragment.skip.should_skip(variables).unwrap_or(false) {
                            continue;
                        }
                        if !fragment.include.should_include(variables).unwrap_or(true) {
                            continue;
                        }

                        if input
                            .get("__typename")
                            .map(|val| val.as_str() == Some(fragment.type_condition.as_str()))
                            .unwrap_or_else(|| {
                                known_type.as_deref() == Some(fragment.type_condition.as_str())
                            })
                        {
                            self.apply_selection_set(
                                &fragment.selection_set,
                                variables,
                                input,
                                output,
                                schema,
                            )?;
                        }
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("missing fragment named: {}", name);
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_root_selection_set(
        &self,
        operation: &Operation,
        variables: &Object,
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
                    skip: _,
                    include: _,
                } => {
                    if let Some(input_value) = input.get_mut(name.as_str()) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was moved into output using take(), replacing
                        // the input value with Null
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null
                        if input_value.is_null() && output.contains_key(name.as_str()) {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let output_value = output.entry((*name).clone()).or_insert(Value::Null);
                        self.format_value(
                            field_type,
                            variables,
                            input_value,
                            output_value,
                            selection_set,
                            schema,
                        )?;
                    } else if field_type.is_non_null() {
                        return Err(InvalidValue);
                    }
                }
                Selection::InlineFragment {
                    fragment:
                        Fragment {
                            type_condition,
                            selection_set,
                            skip: _,
                            include: _,
                        },
                    known_type: _,
                } => {
                    // top level objects will not provide a __typename field
                    match (type_condition.as_str(), operation.kind) {
                        ("Query", OperationKind::Query) | ("Mutation", OperationKind::Mutation) => {
                        }
                        _ => {
                            return Err(InvalidValue);
                        }
                    }
                    self.apply_selection_set(selection_set, variables, input, output, schema)?;
                }
                Selection::FragmentSpread {
                    name,
                    known_type: _,
                    skip: _,
                    include: _,
                } => {
                    if let Some(fragment) = self.fragments.get(name) {
                        // top level objects will not provide a __typename field
                        match (fragment.type_condition.as_str(), operation.kind) {
                            ("Query", OperationKind::Query)
                            | ("Mutation", OperationKind::Mutation) => {}
                            _ => {
                                return Err(InvalidValue);
                            }
                        }
                        self.apply_selection_set(
                            &fragment.selection_set,
                            variables,
                            input,
                            output,
                            schema,
                        )?;
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("missing fragment named: {}", name);
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
            .filter_map(|(name, (ty, _))| {
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
    variables: HashMap<ByteString, (FieldType, Option<Value>)>,
}

impl Operation {
    // clippy false positive due to the bytes crate
    // ref: https://rust-lang.github.io/rust-clippy/master/index.html#mutable_key_type
    #[allow(clippy::mutable_key_type)]
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

                (
                    ByteString::from(name),
                    (ty, parse_default_value(&definition)),
                )
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

fn parse_default_value(definition: &ast::VariableDefinition) -> Option<Value> {
    definition
        .default_value()
        .and_then(|v| v.value())
        .and_then(|value| parse_value(&value))
}

fn parse_value(value: &ast::Value) -> Option<Value> {
    match value {
        ast::Value::Variable(_) => None,
        ast::Value::StringValue(s) => Some(s.to_string().into()),
        ast::Value::FloatValue(f) => f.to_string().parse::<f64>().ok().map(Into::into),
        ast::Value::IntValue(i) => {
            let s = i.to_string();
            s.parse::<i64>()
                .ok()
                .map(Into::into)
                .or_else(|| s.parse::<u64>().ok().map(Into::into))
        }
        ast::Value::BooleanValue(b) => {
            match (b.true_token().is_some(), b.false_token().is_some()) {
                (true, false) => Some(Value::Bool(true)),
                (false, true) => Some(Value::Bool(false)),
                _ => None,
            }
        }
        ast::Value::NullValue(_) => Some(Value::Null),
        ast::Value::EnumValue(e) => e.name().map(|n| n.text().to_string().into()),
        ast::Value::ListValue(l) => l
            .values()
            .map(|v| parse_value(&v))
            .collect::<Option<_>>()
            .map(Value::Array),
        ast::Value::ObjectValue(o) => o
            .object_fields()
            .map(|field| match (field.name(), field.value()) {
                (Some(name), Some(value)) => {
                    parse_value(&value).map(|v| (name.text().to_string().into(), v))
                }
                _ => None,
            })
            .collect::<Option<_>>()
            .map(Value::Object),
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
            assert_format_response!(
                $schema,
                $query,
                $response,
                $operation,
                Value::Object(Object::default()),
                $expected
            );
        }};

        ($schema:expr, $query:expr, $response:expr, $operation:expr, $variables:expr, $expected:expr $(,)?) => {{
            let schema: Schema = $schema.parse().expect("could not parse schema");
            let query = Query::parse($query, &schema).expect("could not parse query");
            let mut response = Response::builder().data($response.clone()).build();
            query.format_response(
                &mut response,
                $operation,
                $variables.as_object().unwrap().clone(),
                &schema,
            );
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
    }

    #[test]
    fn inline_fragment_on_top_level_operation() {
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
                "getStuff": {
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
        extend union Thing = Bar | Baz";
        let query = "query { thing {...foo ...bar ...baz} } fragment foo on Foo {foo} fragment bar on Bar {bar} fragment baz on Baz {baz}";

        // should only select fields from Foo
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
        // should only select fields from Bar
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
        // should only select fields from Baz
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

            union Element = Baz | Bar
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
                        "bar": null,
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
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({})
        );
        assert_validation!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}})
        );
        assert_validation_error!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":1})
        );
        assert_validation_error!(
            "input Foo{ y: String } type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":"str"})
        );
        assert_validation_error!(
            "input Foo{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{}})
        );
        assert_validation!(
            "input Foo{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"x":1}})
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
            "input Foo{bar:Bar!} input Bar{x:Int!} type Query { x: String }",
            "query($foo:Foo){x}",
            json!({"foo":{"bar":{"x":1}}})
        );
        assert_validation!(
            "enum Availability{AVAILABLE} type Product{availability:Availability! name:String} type Query{products(availability: Availability!): [Product]!}",
            "query GetProductsByAvailability($availability: Availability!){products(availability: $availability) {name}}",
            json!({"availability": "AVAILABLE"})
        );

        assert_validation!(
            "input MessageInput {
                content: String
                author: String
              }
              type Receipt {
                  id: ID!
              }
              type Query{
                  send(message: MessageInput): String}",
            "query {
                send(message: {
                    content: \"Hello\"
                    author: \"Me\"
                }) {
                    id
                }}",
            json!({"availability": "AVAILABLE"})
        );

        assert_validation!(
            "input MessageInput {
                content: String
                author: String
              }
              type Receipt {
                  id: ID!
              }
              type Query{
                  send(message: MessageInput): String}",
            "query($msg: MessageInput) {
                send(message: $msg) {
                    id
                }}",
            json!({"msg":  {
                "content": "Hello",
                "author": "Me"
            }})
        );
    }

    #[test]
    fn filter_root_errors() {
        let schema = "type Query {
            getInt: Int
            getNonNullString: String!
        }";
        assert_format_response!(
            schema,
            "query MyOperation { getInt }",
            json! {{
                "getInt": "not_an_int",
                "other": "2",
            }},
            Some("MyOperation"),
            json! {{
                "getInt": null,
            }},
        );

        assert_format_response!(
            schema,
            "query { getNonNullString }",
            json! {{
                "getNonNullString": 1,
            }},
            None,
            Value::Null,
        );
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

        // name expected a string, got an int
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

        // non null id expected a string, got an int
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

        // non null id got a null
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

        // non null id was absent
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

        // non null id was absent
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

        // duplicate id field
        assert_format_response!(
            schema,
            "query  { me { id ...on User { id } } }",
            json! {{
                "me": {
                    "__typename": "User",
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

        // l1: nullable list of nullable elements
        // any error should stop at the list elements
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

        // l1 expected a list, got a string
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

        // l2: nullable list of non nullable elements
        // any element error should nullify the entire list
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

        // l3: nullable list of nullable elements
        // any element error should stop at the list elements
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

        // non null l3 expected a list, got an int, parrent element should be null
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

        // l4: non nullable list of non nullable elements
        // any element error should nullify the entire list,
        // which will nullify the parent element
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

        // nullable parent and child elements
        // child errors should stop at the child's level
        let query_review1_text1 = "query  { me { id reviews1 { text1 } } }";
        // nullable text1 was absent, should we keep the empty object, or put a text1: null here?
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
                    "reviews1": [ {
                        "text1": null,
                     } ],
                },
            }},
        );

        // nullable text1 was null
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

        // nullable text1 expected a string, got an int, so text1 is nullified
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

        // text2 is non null so errors should nullify reviews1 element
        let query_review1_text2 = "query  { me { id reviews1 { text2 } } }";
        // text2 was absent, reviews1 element should be nullified
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

        // text2 was null, reviews1 element should be nullified
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

        // text2 expected a string, got an int, text2 is nullified, reviews1 element should be nullified
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
        // reviews2 elements are non null, so any error there should nullify the entire list
        let query_review2_text1 = "query  { me { id reviews2 { text1 } } }";
        // nullable text1 was absent
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
                    "reviews2": [ {
                        "text1": null,
                    } ],
                },
            }},
        );

        // nullable text1 was null
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

        // nullable text1 expected a string, got an int
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

        // text2 is non null
        let query_review2_text2 = "query  { me { id reviews2 { text2 } } }";
        // text2 was absent, so the reviews2 element is nullified, so reviews2 is nullified
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
        // text2 was null, so the reviews2 element is nullified, so reviews2 is nullified
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
        // text2 expected a string, got an int, so the reviews2 element is nullified, so reviews2 is nullified
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
        // reviews3 is non null, and its elements are non null
        let query_review3_text1 = "query  { me { id reviews3 { text1 } } }";
        // nullable text1 was absent
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
                    "reviews3": [ {
                        "text1": null,
                    } ],
                },
            }},
        );

        // nullable text1 was null
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

        // nullable text1 expected a string, got an int
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

        // reviews3 is non null, and its elements are non null, text2 is  on null
        let query_review3_text2 = "query  { me { id reviews3 { text2 } } }";
        // text2 was absent, nulls should propagate up to the operation
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
        // text2 was null, nulls should propagate up to the operation
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
        // text2 expected a string, got an int, nulls should propagate up to the operation
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

        // both aliases got valid values
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

        // non null identifiant expected a string, got an int, the operation should be null
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

        // non null identifiant was null, the operation should be null
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

        // non null identifiant was absent, the operation should be null
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

        // both aliases got valid values
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

        // nullable name2 expected a string, got an int, name2 should be null
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

        // nullable name2 was null
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

        // nullable name2 was absent
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
                    "name2": null,
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

        // scalar a is present, no further validation
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

        // scalar a is present, no further validation
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

        // non null scalar b is present, no further validation
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

        // non null scalar b is present, no further validation
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

        // non null scalar b was null, the operation should be null
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

        // non null scalar b was absent, the operation should be null
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

        // enum a got a correct value
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

        // nullable enum a expected "X", "Y" or "Z", got another string, a should be null
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

        // nullable enum a was null
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

        // non nullable enum b got a correct value
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
        // non nullable enum b expected "X", "Y" or "Z", got another string, b and the operation should be null
        assert_format_response!(
            schema,
            query_b,
            json! {{
                "me": {
                    "id": "a",
                    "b": "hello",
                }
            }},
            None,
            json! {{
                "me": null,
            }},
        );

        // non nullable enum b was null, the operation should be null
        assert_format_response!(
            schema,
            query_b,
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
            name2: String!
        }

        type User2 implements NamedEntity {
            name: String
            name2: String!
        }
        ";

        let query = "query  { me { name } }";

        // nullable name field got a correct value
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

        // nullable name field was absent
        assert_format_response!(
            schema,
            query,
            json! {{
                "me": { },
            }},
            None,
            json! {{
                "me": {
                    "name": null,
                },
            }},
        );

        // nullable name field was null
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

        // nullable name field expected a string, got an int
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

        // non nullable name2 field got a correct value
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

        // non nullable name2 field was absent, the operation should be null
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

        // non nullable name2 field was null, the operation should be null
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

        // non nullable name2 field expected a string, got an int, name2 and the operation should be null
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

        // we should be able to handle duplicate fields even across fragments and interfaces
        assert_format_response!(
            schema,
            "query  { me { ... on User { name2 } name2 } }",
            json! {{
                "me": {
                    "__typename": "User",
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

        // non nullable name2 got a correct value
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

        // non nullable name2 was null, the operation should be null
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

        // non nullable name2 was absent, the operation should be null
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

        // non nullable name2 expected a string, got an int, the operation should be null
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

        // fragments can appear on top level queries
        let query = "{ ...frag } fragment frag on Query { __typename get { name } }";
        // nullable name got a correct value
        assert_format_response!(
            schema,
            query,
            json! {
                { "get": {"name": "a", "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": {
                    "name": "a",
                }
            }},
        );

        // nullable name was null
        assert_format_response!(
            schema,
            query,
            json! {
                { "get": {"name": null, "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": {
                    "name": null,
                }
            }},
        );

        let query2 = "{ ...frag2 } fragment frag2 on Query { __typename get { name2 } }";
        // non nullable name2 got a correct value
        assert_format_response!(
            schema,
            query2,
            json! {
                { "get": {"name2": "a", "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": {
                    "name2": "a",
                }
            }},
        );

        // non nullable name2 was null, the operation should be null
        assert_format_response!(
            schema,
            query2,
            json! {
                { "get": {"name2": null, "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": null
            }},
        );

        let query3 = "{ ... on Query { __typename get { name } } }";
        // nullable name got a correct value
        assert_format_response!(
            schema,
            query3,
            json! {
                { "get": {"name": "a", "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": {
                    "name": "a",
                }
            }},
        );

        // nullable name was null
        assert_format_response!(
            schema,
            query3,
            json! {
                { "get": {"name": null, "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": {
                    "name": null,
                }
            }},
        );

        let query4 = "{ ... on Query { __typename get { name2 } } }";
        // non nullable name2 got a correct value
        assert_format_response!(
            schema,
            query4,
            json! {
                { "get": {"name2": "a", "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": {
                    "name2": "a",
                }
            }},
        );

        // non nullable name2 was null, the operation should be null
        assert_format_response!(
            schema,
            query4,
            json! {
                { "get": {"name2": null, "other": "b"} }
            },
            None,
            json! {{
                "__typename": null,
                "get": null,
            }},
        );
    }

    #[test]
    fn merge_selections() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }
        
        type Review {
            id: String!
            body: String
        }";

        // duplicate operation name
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                }
                get {
                    name
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );

        // merge nested selection
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    review {
                        id
                    }

                    ... on Product {
                        review {
                            body
                        }
                    }
                }
                get {
                    name
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "__typename": "Review",
                        "id": "b",
                        "body": "hello",
                    }
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "id": "b",
                        "body": "hello",
                    },
                    "name": null,
                },
            }},
        );
    }

    #[test]
    fn skip() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }";

        // duplicate operation name
        assert_format_response!(
            schema,
            "query  {
                get {
                    name @skip(if: true)
                }
                get @skip(if: false) {
                    id 
                    review {
                        id
                    }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                    "review": {
                        "id": "b",
                    }
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "id": "b",
                    }
                },
            }},
        );

        // skipped non null
        assert_format_response!(
            schema,
            "query  {
                get {
                    id @skip(if: true)
                    name
                }
            }",
            json! {{
                "get": {
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "name": "Chair",
                },
            }},
        );

        // inline fragment
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ... on Product @skip(if: true) {
                        name
                    }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",

                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ... on Product @skip(if: false) {
                        name
                    }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",

                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );

        // directive on fragment spread
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test @skip(if: false)
                }
            }

            fragment test on Product {
                nom: name
                name @skip(if: true)
            }
            ",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test @skip(if: true)
                }
            }

            fragment test on Product {
                nom: name
                name @skip(if: true)
            }",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        // directive on fragment
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test
                }
            }

            fragment test on Product @skip(if: false) {
                nom: name
                name @skip(if: true)
            }
            ",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test
                }
            }

            fragment test on Product @skip(if: true) {
                nom: name
                name @skip(if: true)
            }",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        // variables
        // duplicate operation name
        assert_format_response!(
            schema,
            "query Example($shouldSkip: Boolean) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{
                "shouldSkip": true
            }},
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query Example($shouldSkip: Boolean) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{
                "shouldSkip": false
            }},
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );

        // default variable value
        assert_format_response!(
            schema,
            "query Example($shouldSkip: Boolean = true) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{ }},
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query Example($shouldSkip: Boolean = true) {
                get {
                    id
                    name @skip(if: $shouldSkip)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{
                "shouldSkip": false
            }},
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );
    }

    #[test]
    fn include() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }";

        // duplicate operation name
        assert_format_response!(
            schema,
            "query  {
                get {
                    name @include(if: false)
                }
                get @include(if: true) {
                    id 
                    review {
                        id
                    }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                    "review": {
                        "id": "b",
                    }
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "review": {
                        "id": "b",
                    }
                },
            }},
        );

        // skipped non null
        assert_format_response!(
            schema,
            "query  {
                get {
                    id @include(if: false)
                    name
                }
            }",
            json! {{
                "get": {
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "name": "Chair",
                },
            }},
        );

        // inline fragment
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ... on Product @include(if: false) {
                        name
                    }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",

                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ... on Product @include(if: true) {
                        name
                    }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",

                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );

        // directive on fragment spread
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test @include(if: true)
                }
            }

            fragment test on Product {
                nom: name
                name @skip(if: true)
            }
            ",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test @include(if: false)
                }
            }

            fragment test on Product {
                nom: name
                name @include(if: false)
            }",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        // directive on fragment
        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test
                }
            }

            fragment test on Product @include(if: true) {
                nom: name
                name @include(if: false)
            }
            ",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    id
                    ...test
                }
            }

            fragment test on Product @include(if: false) {
                nom: name
                name @include(if: false)
            }",
            json! {{
                "get": {
                    "id": "a",
                    "nom": "Chaise",
                    "name": "Chair",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        // variables
        // duplicate operation name
        assert_format_response!(
            schema,
            "query Example($shouldInclude: Boolean) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{
                "shouldInclude": false
            }},
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query Example($shouldInclude: Boolean) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{
                "shouldInclude": true
            }},
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );

        // default variable value
        assert_format_response!(
            schema,
            "query Example($shouldInclude: Boolean = false) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{ }},
            json! {{
                "get": {
                    "id": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query Example($shouldInclude: Boolean = false) {
                get {
                    id
                    name @include(if: $shouldInclude)
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
            Some("Example"),
            json! {{
                "shouldInclude": true
            }},
            json! {{
                "get": {
                    "id": "a",
                    "name": "Chair",
                },
            }},
        );
    }

    #[test]
    fn skip_and_include() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
        }";

        // combine skip and include
        // both of them must accept the field
        // ref: https://spec.graphql.org/October2021/#note-f3059
        assert_format_response!(
            schema,
            "query  {
                get {
                    a:name @skip(if:true) @include(if: true)
                    b:name @skip(if:true) @include(if: false)
                    c:name @skip(if:false) @include(if: true)
                    d:name @skip(if:false) @include(if: false)
                }

            }",
            json! {{
                "get": {
                    "a": "a",
                    "b": "b",
                    "c": "c",
                    "d": "d",
                },
            }},
            None,
            json! {{
                "get": {
                    "c": "c",
                },
            }},
        );
    }

    #[test]
    fn skip_and_include_multi_operation() {
        let schema = "type Query {
            get: Product
        }

        type Product {
            id: String!
            name: String
        }";

        // combine skip and include
        // both of them must accept the field
        // ref: https://spec.graphql.org/October2021/#note-f3059
        assert_format_response!(
            schema,
            "query  {
                get {
                    a:name @skip(if:false)
                }
                get {
                    a:name @skip(if:true)
                }
            }",
            json! {{
                "get": {
                    "a": "a",
                },
            }},
            None,
            json! {{
                "get": {
                    "a": "a",
                },
            }},
        );

        assert_format_response!(
            schema,
            "query  {
                get {
                    a:name @skip(if:true)
                }
                get {
                    a:name @skip(if:false)
                }
            }",
            json! {{
                "get": {
                    "a": "a",
                },
            }},
            None,
            json! {{
                "get": {
                    "a": "a",
                },
            }},
        );
    }
}
