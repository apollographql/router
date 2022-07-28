//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.

use std::collections::HashMap;
use std::collections::HashSet;

use apollo_parser::ast;
use derivative::Derivative;
use serde_json_bytes::ByteString;
use tracing::level_filters::LevelFilter;

use crate::error::FetchError;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::query_planner::fetch::OperationKind;
use crate::*;

const TYPENAME: &str = "__typename";

/// A GraphQL query.
#[derive(Debug, Derivative, Default)]
#[derivative(PartialEq, Hash, Eq)]
pub struct Query {
    string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    fragments: Fragments,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    operations: Vec<Operation>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) subselections: HashMap<(Option<Path>, String), Query>,
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
        if let Some(Value::Object(mut input)) = data {
            let operation = match operation_name {
                Some(name) => self
                    .operations
                    .iter()
                    // we should have an error if the only operation is anonymous but the query specifies a name
                    .find(|op| op.name.is_some() && op.name.as_deref().unwrap() == name),
                None => self.operations.get(0),
            };
            if let Some(subselection) = &response.subselection {
                // Get subselection from hashmap
                match self.subselections.get(&(
                    //FIXME: we should not have optional paths at all in the subselections map
                    response.path.clone().or_else(|| Some(Path::default())),
                    subselection.clone(),
                )) {
                    Some(subselection_query) => {
                        let mut output = Object::default();
                        let operation = &subselection_query.operations[0];
                        response.data = Some(
                            match self.apply_root_selection_set(
                                operation,
                                &variables,
                                &mut input,
                                &mut output,
                                schema,
                            ) {
                                Ok(()) => output.into(),
                                Err(InvalidValue) => Value::Null,
                            },
                        );

                        return;
                    }
                    None => failfast_debug!("can't find subselection for {:?}", subselection),
                }
            } else if let Some(operation) = operation {
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

                response.data = Some(
                    match self.apply_root_selection_set(
                        operation,
                        &all_variables,
                        &mut input,
                        &mut output,
                        schema,
                    ) {
                        Ok(()) => output.into(),
                        Err(InvalidValue) => Value::Null,
                    },
                );

                return;
            } else {
                failfast_debug!("can't find operation for {:?}", operation_name);
            }
        } else {
            failfast_debug!("invalid type for data in response.");
        }

        response.data = Some(Value::default());
    }

    pub fn parse(query: impl Into<String>, schema: &Schema) -> Result<Self, SpecError> {
        let string = query.into();

        let parser = apollo_parser::Parser::new(string.as_str());
        let tree = parser.parse();

        // Trace log recursion limit data
        let recursion_limit = tree.recursion_limit();
        tracing::trace!(?recursion_limit, "recursion limit data");

        let errors = tree
            .errors()
            .map(|err| format!("{:?}", err))
            .collect::<Vec<_>>();

        if !errors.is_empty() {
            let errors = errors.join(", ");
            failfast_debug!("parsing error(s): {}", errors);
            return Err(SpecError::ParsingError(errors));
        }

        let document = tree.document();
        let fragments = Fragments::from_ast(&document, schema)?;

        let operations: Vec<Operation> = document
            .definitions()
            .filter_map(|definition| {
                if let ast::Definition::OperationDefinition(operation) = definition {
                    Some(operation)
                } else {
                    None
                }
            })
            .map(|operation| Operation::from_ast(operation, schema))
            .collect::<Result<Vec<_>, SpecError>>()?;

        Ok(Query {
            string,
            fragments,
            operations,
            subselections: HashMap::new(),
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

            FieldType::Named(type_name) | FieldType::Introspection(type_name) => {
                // we cannot know about the expected format of custom scalars
                // so we must pass them directly to the client
                if schema.custom_scalars.contains(type_name) {
                    *output = input.take();
                    return Ok(());
                } else if let Some(enum_type) = schema.enums.get(type_name) {
                    return match input.as_str() {
                        Some(s) => {
                            if enum_type.contains(s) {
                                *output = input.clone();
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
                        if let Some(input_type) =
                            input_object.get(TYPENAME).and_then(|val| val.as_str())
                        {
                            if !schema.object_types.contains_key(input_type) {
                                *output = Value::Null;
                                return Ok(());
                            }
                        }

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
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Float => {
                if input.as_f64().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Boolean => {
                if input.as_bool().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::String => {
                if input.as_str().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            FieldType::Id => {
                if input.is_string() || input.is_i64() || input.is_u64() || input.is_f64() {
                    *output = input.clone();
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
        // For skip and include, using .unwrap_or is legit here because
        // validate_variables should have already checked that
        // the variable is present and it is of the correct type
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    alias,
                    selection_set,
                    field_type,
                    skip,
                    include,
                } => {
                    let field_name = alias.as_ref().unwrap_or(name);
                    if skip.should_skip(variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(variables).unwrap_or(true) {
                        continue;
                    }

                    if let Some(input_value) = input.get_mut(field_name.as_str()) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was moved into output using take(), replacing
                        // the input value with Null
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null
                        if input_value.is_null() && output.contains_key(field_name.as_str()) {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let output_value =
                            output.entry((*field_name).clone()).or_insert(Value::Null);
                        if field_name.as_str() == TYPENAME {
                            if input_value.is_string() {
                                *output_value = input_value.clone();
                            }
                        } else {
                            self.format_value(
                                field_type,
                                variables,
                                input_value,
                                output_value,
                                selection_set,
                                schema,
                            )?;
                        }
                    } else {
                        if !output.contains_key(field_name.as_str()) {
                            output.insert((*field_name).clone(), Value::Null);
                        }
                        if field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    skip,
                    include,
                    known_type,
                } => {
                    if skip.should_skip(variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(variables).unwrap_or(true) {
                        continue;
                    }

                    let is_apply = if let Some(input_type) =
                        input.get(TYPENAME).and_then(|val| val.as_str())
                    {
                        // check if the fragment matches the input type directly, and if not, check if the
                        // input type is a subtype of the fragment's type condition (interface, union)
                        input_type == type_condition.as_str()
                            || schema.is_subtype(type_condition, input_type)
                    } else {
                        // known_type = true means that from the query's shape, we know
                        // we should get the right type here. But in the case we get a
                        // __typename field and it does not match, we should not apply
                        // that fragment
                        // If the type condition is an interface and the current known type implements it
                        known_type
                            .as_ref()
                            .map(|k| schema.is_subtype(type_condition, k))
                            .unwrap_or_default()
                            || known_type.as_deref() == Some(type_condition.as_str())
                    };

                    if is_apply {
                        self.apply_selection_set(selection_set, variables, input, output, schema)?;
                    }
                }
                Selection::FragmentSpread {
                    name,
                    known_type,
                    skip,
                    include,
                } => {
                    if skip.should_skip(variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(variables).unwrap_or(true) {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
                        if fragment.skip.should_skip(variables).unwrap_or(false) {
                            continue;
                        }
                        if !fragment.include.should_include(variables).unwrap_or(true) {
                            continue;
                        }

                        let is_apply = if let Some(input_type) =
                            input.get(TYPENAME).and_then(|val| val.as_str())
                        {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            input_type == fragment.type_condition.as_str()
                                || schema.is_subtype(&fragment.type_condition, input_type)
                        } else {
                            // If the type condition is an interface and the current known type implements it
                            known_type
                                .as_ref()
                                .map(|k| schema.is_subtype(&fragment.type_condition, k))
                                .unwrap_or_default()
                                || known_type.as_deref() == Some(fragment.type_condition.as_str())
                        };

                        if is_apply {
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
                    alias,
                    selection_set,
                    field_type,
                    skip,
                    include,
                } => {
                    // Using .unwrap_or is legit here because
                    // validate_variables should have already checked that
                    // the variable is present and it is of the correct type
                    if skip.should_skip(variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(variables).unwrap_or(true) {
                        continue;
                    }

                    let field_name = alias.as_ref().unwrap_or(name);
                    let field_name_str = field_name.as_str();
                    if let Some(input_value) = input.get_mut(field_name_str) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was moved into output using take(), replacing
                        // the input value with Null
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null
                        if input_value.is_null() && output.contains_key(field_name_str) {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        let output_value =
                            output.entry((*field_name).clone()).or_insert(Value::Null);
                        self.format_value(
                            field_type,
                            variables,
                            input_value,
                            output_value,
                            selection_set,
                            schema,
                        )?;
                    } else if field_name_str == TYPENAME {
                        if !output.contains_key(field_name_str) {
                            output.insert(
                                field_name.clone(),
                                Value::String(operation.kind.to_string().into()),
                            );
                        }
                    } else if field_type.is_non_null() {
                        return Err(InvalidValue);
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    ..
                } => {
                    // top level objects will not provide a __typename field
                    if type_condition.as_str() != schema.root_operation_name(operation.kind) {
                        return Err(InvalidValue);
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
                        let operation_type_name = schema.root_operation_name(operation.kind);
                        let is_apply = {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            operation_type_name == fragment.type_condition.as_str()
                                || schema.is_subtype(&fragment.type_condition, operation_type_name)
                        };

                        if !is_apply {
                            return Err(InvalidValue);
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
                ty.validate_input_value(value, schema).err().map(|_| {
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

    pub fn contains_introspection(&self) -> bool {
        self.operations.iter().any(Operation::is_introspection)
    }
}

#[derive(Debug)]
pub(crate) struct Operation {
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
    fn from_ast(operation: ast::OperationDefinition, schema: &Schema) -> Result<Self, SpecError> {
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
            OperationKind::Subscription => return Err(SpecError::SubscriptionNotSupported),
        };

        let selection_set = operation
            .selection_set()
            .expect("the node SelectionSet is not optional in the spec; qed")
            .selections()
            .map(|selection| Selection::from_ast(selection, &current_field_type, schema, 0))
            .collect::<Result<Vec<Option<_>>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<Selection>>();

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

        Ok(Operation {
            selection_set,
            name,
            variables,
            kind,
        })
    }

    fn is_introspection(&self) -> bool {
        // If the only field is `__typename` it's considered as an introspection query
        if self.selection_set.len() == 1
            && self
                .selection_set
                .get(0)
                .map(|s| matches!(s, Selection::Field {name, ..} if name.as_str() == TYPENAME))
                .unwrap_or_default()
        {
            return true;
        }
        self.selection_set.iter().all(|sel| match sel {
            Selection::Field { name, .. } => {
                let name = name.as_str();
                // `__typename` can only be resolved in runtime,
                // so this query cannot be seen as an introspection query
                name == "__schema" || name == "__type"
            }
            _ => false,
        })
    }
}

impl From<ast::OperationType> for OperationKind {
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
    use serde_json_bytes::json;
    use test_log::test;

    use super::*;
    use crate::json_ext::ValueExt;

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
            let schema = with_supergraph_boilerplate($schema)
                .parse::<Schema>()
                .expect("could not parse schema");
            let api_schema = schema.api_schema();
            let query = Query::parse($query, &schema).expect("could not parse query");
            let mut response = Response::builder().data($response.clone()).build();

            query.format_response(
                &mut response,
                $operation,
                $variables.as_object().unwrap().clone(),
                api_schema,
            );
            assert_eq_and_ordered!(response.data.as_ref().unwrap(), &$expected);
        }};
    }

    fn with_supergraph_boilerplate(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
        schema
            @core(feature: "https://specs.apollo.dev/core/v0.1")
            @core(feature: "https://specs.apollo.dev/join/v0.1")
            @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
             {
            query: Query
        }
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        "#,
            content
        )
    }

    macro_rules! assert_format_response_fed2 {
        ($schema:expr, $query:expr, $response:expr, $operation:expr, $expected:expr $(,)?) => {{
            assert_format_response_fed2!(
                $schema,
                $query,
                $response,
                $operation,
                Value::Object(Object::default()),
                $expected
            );
        }};

        ($schema:expr, $query:expr, $response:expr, $operation:expr, $variables:expr, $expected:expr $(,)?) => {{
            let schema = with_supergraph_boilerplate_fed2($schema)
                .parse::<Schema>()
                .expect("could not parse schema");
            let api_schema = schema.api_schema();
            let query = Query::parse($query, &schema).expect("could not parse query");
            let mut response = Response::builder().data($response.clone()).build();

            query.format_response(
                &mut response,
                $operation,
                $variables.as_object().unwrap().clone(),
                api_schema,
            );
            assert_eq_and_ordered!(response.data.as_ref().unwrap(), &$expected);
        }};
    }

    fn with_supergraph_boilerplate_fed2(content: &str) -> String {
        format!(
            "{}\n{}",
            r#"
            schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
            {
                query: Query
            }

            directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
            directive @join__graph(name: String!, url: String!) on ENUM_VALUE
            directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
            directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
            directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
            directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ENUM | ENUM_VALUE | SCALAR | INPUT_OBJECT | INPUT_FIELD_DEFINITION | ARGUMENT_DEFINITION

            scalar join__FieldSet
            scalar link__Import
            enum link__Purpose {
            """
            `SECURITY` features provide metadata necessary to securely resolve fields.
            """
            SECURITY

            """
            `EXECUTION` features provide metadata necessary for operation execution.
            """
            EXECUTION
            }

            enum join__Graph {
                TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
            }
        "#,
            content
        )
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
            "{get {foo stuff{bar baz} array{... on Baz { bar baz } } other{bar}}}",
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
    fn reformat_response_array_of_scalar_simple() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }

            ",
            "{get {array}}",
            json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_scalar_alias() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }

            ",
            "{get {stuff: array}}",
            json! {{
                "get": {
                    "stuff": [1,2,3,4],
                },
            }},
            None,
            json! {{
                "get": {
                    "stuff": [1,2,3,4],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_scalar_duplicate_alias() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }

            ",
            "{get {array stuff:array}}",
            json! {{
                "get": {
                    "array": [1,2,3,4],
                    "stuff": [1,2,3,4],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [1,2,3,4],
                    "stuff": [1,2,3,4],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_type_simple() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            type Element {
                stuff: String
            }
            ",
            "{get {array{stuff}}}",
            json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_type_alias() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            type Element {
                stuff: String
            }
            ",
            "{get { aliased: array {stuff}}}",
            json! {{
                "get": {
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
            None,
            json! {{
                "get": {
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_type_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            type Element {
                stuff: String
            }
            ",
            "{get {array{stuff} array{stuff}}}",
            json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_type_duplicate_alias() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }
            
            type Element {
                stuff: String
            }
            ",
            "{get {array{stuff} aliased: array{stuff}}}",
            json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                    "aliased": [{"stuff": "FOO"}, {"stuff": "BAR"}],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_enum_simple() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }
            ",
            "{get {array}}",
            json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_enum_alias() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }
            ",
            "{get {stuff: array}}",
            json! {{
                "get": {
                    "stuff": ["FOO", "BAR"],
                },
            }},
            None,
            json! {{
                "get": {
                    "stuff": ["FOO", "BAR"],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_enum_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }
            ",
            "{get {array array}}",
            json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_enum_duplicate_alias() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Element]
            }

            enum Element {
                FOO
                BAR
            }
            ",
            "{get {array stuff: array}}",
            json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                    "stuff": ["FOO", "BAR"],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": ["FOO", "BAR"],
                    "stuff": ["FOO", "BAR"],
                },
            }},
        );
    }

    #[test]
    // If this test fails, this means you got greedy about allocations,
    // beware of aliases!
    fn reformat_response_array_of_int_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Int]
            }

            ",
            "{get {array array}}",
            json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [1,2,3,4],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_float_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Float]
            }

            ",
            "{get {array array}}",
            json! {{
                "get": {
                    "array": [1.2,3.4],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [1.2,3.4],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_bool_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [Boolean]
            }

            ",
            "{get {array array}}",
            json! {{
                "get": {
                    "array": [true,false],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": [true,false],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_string_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [String]
            }

            ",
            "{get {array array}}",
            json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_array_of_id_duplicate() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                array: [ID]
            }

            ",
            "{get {array array}}",
            json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }},
            None,
            json! {{
                "get": {
                    "array": ["hello","world"],
                },
            }},
        );
    }

    #[test]
    fn reformat_response_query_with_root_typename() {
        assert_format_response!(
            "type Query {
                get: Thing
            }
            type Thing {
                foo: String
            }
            ",
            "{get {foo __typename} __typename}",
            json! {{
                "get": {
                    "foo": "1",
                    "__typename": "Thing"
                }
            }},
            None,
            json! {{
                "get": {
                    "foo": "1",
                    "__typename": "Thing"
                },
                "__typename": "Query",
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
                .query($query.to_string())
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
            let res = run_validation!(with_supergraph_boilerplate($schema), $query, $variables);
            assert!(res.is_ok(), "validation should have succeeded: {:?}", res);
        }};
    }

    macro_rules! assert_validation_error {
        ($schema:expr, $query:expr, $variables:expr $(,)?) => {{
            let res = run_validation!(with_supergraph_boilerplate($schema), $query, $variables);
            assert!(res.is_err(), "validation should have failed");
        }};
    }

    #[test]
    fn variable_validation() {
        let schema = "type Query { x: String }";
        // https://spec.graphql.org/June2018/#sec-Int
        assert_validation!(schema, "query($foo:Int){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Int!){x}", json!({}));
        // When expected as an input type, only integer input values are accepted.
        assert_validation!(schema, "query($foo:Int){x}", json!({"foo":2}));
        assert_validation!(schema, "query($foo:Int){x}", json!({ "foo": i32::MAX }));
        assert_validation!(schema, "query($foo:Int){x}", json!({ "foo": i32::MIN }));
        // All other input values, including strings with numeric content, must raise a query error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":"2"}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":2.0}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":"str"}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:Int){x}", json!({"foo":{}}));
        //  If the integer input value represents a value less than -231 or greater than or equal to 231, a query error should be raised.
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

        // https://spec.graphql.org/draft/#sec-Float.Input-Coercion
        assert_validation!(schema, "query($foo:Float){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Float!){x}", json!({}));

        // When expected as an input type, both integer and float input values are accepted.
        assert_validation!(schema, "query($foo:Float){x}", json!({"foo":2}));
        assert_validation!(schema, "query($foo:Float){x}", json!({"foo":2.0}));
        // All other input values, including strings with numeric content,
        // must raise a request error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:Float){x}", json!({"foo":"2.0"}));
        assert_validation_error!(schema, "query($foo:Float){x}", json!({"foo":"2"}));

        // https://spec.graphql.org/June2018/#sec-String
        assert_validation!(schema, "query($foo:String){x}", json!({}));
        assert_validation_error!(schema, "query($foo:String!){x}", json!({}));

        // When expected as an input type, only valid UTF‐8 string input values are accepted.
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "str"}));

        // All other input values must raise a query error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo": 0}));
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo": 42.0}));
        assert_validation_error!(schema, "query($foo:String){x}", json!({"foo": {}}));

        // https://spec.graphql.org/June2018/#sec-Boolean
        assert_validation!(schema, "query($foo:Boolean){x}", json!({}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({}));
        // When expected as an input type, only boolean input values are accepted.
        // All other input values must raise a query error indicating an incorrect type.
        assert_validation!(schema, "query($foo:Boolean!){x}", json!({"foo":true}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo":"true"}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo": 0}));
        assert_validation_error!(schema, "query($foo:Boolean!){x}", json!({"foo": "no"}));

        // https://spec.graphql.org/June2018/#sec-ID
        assert_validation!(schema, "query($foo:ID){x}", json!({}));
        assert_validation_error!(schema, "query($foo:ID!){x}", json!({}));
        // When expected as an input type, any string (such as "4") or integer (such as 4)
        // input value should be coerced to ID as appropriate for the ID formats a given GraphQL server expects.
        assert_validation!(schema, "query($foo:ID){x}", json!({"foo": 4}));
        assert_validation!(schema, "query($foo:ID){x}", json!({"foo": "4"}));
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "str"}));
        assert_validation!(schema, "query($foo:String){x}", json!({"foo": "4.0"}));
        // Any other input value, including float input values (such as 4.0), must raise a query error indicating an incorrect type.
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": 4.0}));
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": true}));
        assert_validation_error!(schema, "query($foo:ID){x}", json!({"foo": {}}));

        // https://spec.graphql.org/June2018/#sec-Type-System.List
        assert_validation!(schema, "query($foo:[Int]){x}", json!({}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({ "foo": null }));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":1}));
        assert_validation!(schema, "query($foo:[String]){x}", json!({"foo":"bar"}));
        assert_validation!(schema, "query($foo:[[Int]]){x}", json!({"foo":1}));
        assert_validation!(
            schema,
            "query($foo:[[Int]]){x}",
            json!({"foo":[[1], [2, 3]]})
        );
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":"str"}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":{}}));
        assert_validation_error!(schema, "query($foo:[Int]!){x}", json!({}));
        assert_validation_error!(schema, "query($foo:[Int!]){x}", json!({"foo":[1, null]}));
        assert_validation!(schema, "query($foo:[Int]!){x}", json!({"foo":[]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":["f","o","o"]}));
        assert_validation_error!(schema, "query($foo:[Int]){x}", json!({"foo":["1","2","3"]}));
        assert_validation!(
            schema,
            "query($foo:[String]){x}",
            json!({"foo":["1","2","3"]})
        );
        assert_validation_error!(schema, "query($foo:[String]){x}", json!({"foo":[1,2,3]}));
        assert_validation!(schema, "query($foo:[Int!]){x}", json!({"foo":[1,2,3]}));
        assert_validation_error!(schema, "query($foo:[Int!]){x}", json!({"foo":[1,null,3]}));
        assert_validation!(schema, "query($foo:[Int]){x}", json!({"foo":[1,null,3]}));

        // https://spec.graphql.org/June2018/#sec-Input-Objects
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
    fn it_statically_includes() {
        let schema = with_supergraph_boilerplate(
            "type Query {
            name: String
            review: Review
            product: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }",
        )
        .parse::<Schema>()
        .expect("could not parse schema");

        let query = Query::parse(
            "query  {
                name @include(if: false)
                review @include(if: false)
                product @include(if: true) {
                    name
                }
            }",
            &schema,
        )
        .expect("could not parse query");
        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        let query = Query::parse(
            "query  {
                name @include(if: false)
                review
                product @include(if: true) {
                    name
                }
            }",
            &schema,
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        // Inline fragment
        let query = Query::parse(
            "query  {
                name @include(if: false)
                ... @include(if: false) {
                    review
                }
                product @include(if: true) {
                    name
                }
            }",
            &schema,
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 1);
            }
            _ => panic!("expected a field"),
        }

        // Fragment spread
        let query = Query::parse(
            "
            fragment ProductName on Product {
                name
            }
            query  {
                name @include(if: false)
                review
                product @include(if: true) {
                    ...ProductName @include(if: false)
                }
            }",
            &schema,
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 0);
            }
            _ => panic!("expected a field"),
        }
    }

    #[test]
    fn it_statically_skips() {
        let schema = with_supergraph_boilerplate(
            "type Query {
            name: String
            review: Review
            product: Product
        }

        type Product {
            id: String!
            name: String
            review: Review
        }

        type Review {
            id: String!
            body: String
        }",
        )
        .parse::<Schema>()
        .expect("could not parse schema");

        let query = Query::parse(
            "query  {
                name @skip(if: true)
                review @skip(if: true)
                product @skip(if: false) {
                    name
                }
            }",
            &schema,
        )
        .expect("could not parse query");
        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        let query = Query::parse(
            "query  {
                name @skip(if: true)
                review
                product @skip(if: false) {
                    name
                }
            }",
            &schema,
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("product")),
            _ => panic!("expected a field"),
        }

        // Inline fragment
        let query = Query::parse(
            "query  {
                name @skip(if: true)
                ... @skip(if: true) {
                    review
                }
                product @skip(if: false) {
                    name
                }
            }",
            &schema,
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 1);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 1);
            }
            _ => panic!("expected a field"),
        }

        // Fragment spread
        let query = Query::parse(
            "
            fragment ProductName on Product {
                name
            }
            query  {
                name @skip(if: true)
                review
                product @skip(if: false) {
                    ...ProductName @skip(if: true)
                }
            }",
            &schema,
        )
        .expect("could not parse query");

        assert_eq!(query.operations.len(), 1);
        let operation = &query.operations[0];
        assert_eq!(operation.selection_set.len(), 2);
        match operation.selection_set.get(0).unwrap() {
            Selection::Field { name, .. } => assert_eq!(name, &ByteString::from("review")),
            _ => panic!("expected a field"),
        }
        match operation.selection_set.get(1).unwrap() {
            Selection::Field {
                name,
                selection_set: Some(selection_set),
                ..
            } => {
                assert_eq!(name, &ByteString::from("product"));
                assert_eq!(selection_set.len(), 0);
            }
            _ => panic!("expected a field"),
        }
    }

    #[test]
    fn it_should_fail_with_empty_selection_set() {
        let schema = with_supergraph_boilerplate(
            "type Query {
            product: Product
        }

        type Product {
            id: String!
            name: String
        }",
        )
        .parse::<Schema>()
        .expect("could not parse schema");

        let _query_error = Query::parse(
            "query  {
                product {
                }
            }",
            &schema,
        )
        .expect_err("should not parse query");
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
    fn check_fragment_on_interface() {
        let schema = "type Query {
            get: Product
        }

        interface Product {
            id: String!
            name: String
        }

        type Vodka {
            id: String!
            name: String
        }

        type Beer implements Product {
            id: String!
            name: String
        }";

        assert_format_response!(
            schema,
            "
            fragment ProductBase on Product {
              __typename
              id
              name
            }
            query  {
                get {
                  ...ProductBase
                }
            }",
            json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }},
            None,
            json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }},
        );

        assert_format_response!(
            schema,
            "
            fragment ProductBase on Product {
              id
              name
            }
            query  {
                get {
                  ...ProductBase
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }},
        );

        assert_format_response!(
            schema,
            "
            query  {
                get {
                  ... on Product {
                    __typename
                    id
                    name
                  }
                }
            }",
            json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }},
            None,
            json! {{
                "get": {
                    "__typename": "Beer",
                    "id": "a",
                    "name": "Asahi",
                },
            }},
        );

        assert_format_response!(
            schema,
            "
            query  {
                get {
                  ... on Product {
                    id
                    name
                  }
                }
            }",
            json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }},
            None,
            json! {{
                "get": {
                    "id": "a",
                    "name": "Asahi",
                },
            }},
        );

        // Make sure we do not return data for type that doesn't implement interface
        assert_format_response!(
            schema,
            "
            fragment ProductBase on Product {
              __typename
              id
              name
            }
            query  {
                get {
                  ...ProductBase
                }
            }",
            json! {{
                "get": {
                    "__typename": "Vodka",
                    "id": "a",
                    "name": "Crystal",
                },
            }},
            None,
            json! {{
                "get": {}
            }},
        );

        assert_format_response!(
            schema,
            "
            query  {
                get {
                  ... on Product {
                    __typename
                    id
                    name
                  }
                }
            }",
            json! {{
                "get": {
                    "__typename": "Vodka",
                    "id": "a",
                    "name": "Crystal",
                },
            }},
            None,
            json! {{
                "get": {}
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
            bar: String
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

        assert_format_response!(
            schema,
            "query  {
                get @skip(if: false) @include(if: false) {
                    a:name
                    bar
                }
                get @skip(if: false) {
                    a:name
                    a:name
                }
            }",
            json! {{
                "get": {
                    "a": "a",
                    "bar": "foo",
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

    #[test]
    fn union_with_typename() {
        let schema = "type Query {
            get: ProductResult
        }

        type Product{
            symbol: String!
        }
        type ProductError{
            reason: String
        }
        union ProductResult = Product | ProductError
        ";

        assert_format_response!(
            schema,
            "query  {
                get {
                    __typename
                    ... on Product {
                      symbol
                    }
                    ... on ProductError {
                      reason
                    }
                }
            }",
            json! {{
                "get": {
                    "__typename": "Product",
                    "symbol": "1"
                },
            }},
            None,
            json! {{
                "get": {
                    "__typename": "Product",
                    "symbol": "1"
                },
            }},
        );
    }

    #[test]
    fn inaccessible_on_interface() {
        let schema = "type Query
        {
            test_interface: Interface
            test_union: U
            test_enum: E
        }
        
        type Object implements Interface @inaccessible {
            foo: String
            other: String
        }

        type Object2 implements Interface {
            foo: String
            other: String @inaccessible
        }
          
        interface Interface {
            foo: String
        }

        type A @inaccessible {
            common: String
            a: String
        }

        type B {
            common: String
            b: String
        }
        
        union U = A | B

        enum E {
            X @inaccessible
            Y @inaccessible
            Z
        }
        ";

        assert_format_response_fed2!(
            schema,
            "query  {
                test_interface {
                    __typename
                    foo
                }

                test_interface2: test_interface {
                    __typename
                    foo
                }

                test_union {
                    ...on B {
                        __typename
                        common
                    }
                }

                test_union2: test_union {
                    ...on B {
                        __typename
                        common
                    }
                }

                test_enum
                test_enum2: test_enum
            }",
            json! {{
                "test_interface": {
                    "__typename": "Object",
                    "foo": "bar",
                    "other": "a"
                },

                "test_interface2": {
                    "__typename": "Object2",
                    "foo": "bar",
                    "other": "a"
                },

                "test_union": {
                    "__typename": "A",
                    "common": "hello",
                    "a": "A"
                },

                "test_union2": {
                    "__typename": "B",
                    "common": "hello",
                    "b": "B"
                },

                "test_enum": "X",
                "test_enum2": "Z"
            }},
            None,
            json! {{
                "test_interface": null,
                "test_interface2": {
                    "__typename": "Object2",
                    "foo": "bar",
                },
                "test_union": null,
                "test_union2": {
                    "__typename": "B",
                    "common": "hello",
                },
                "test_enum": null,
                "test_enum2": "Z"
            }},
        );
    }

    #[test]
    fn fragment_on_interface_on_query() {
        let schema = r#"schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/join/v0.2", for: EXECUTION)
            @link(url: "https://specs.apollo.dev/inaccessible/v0.2", for: SECURITY)
        {
            query: MyQueryObject
        }

        directive @join__field(graph: join__Graph!, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
        directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
        directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        directive @inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ENUM | ENUM_VALUE | SCALAR | INPUT_OBJECT | INPUT_FIELD_DEFINITION | ARGUMENT_DEFINITION

        scalar join__FieldSet
        scalar link__Import
        enum link__Purpose {
        """
        `SECURITY` features provide metadata necessary to securely resolve fields.
        """
        SECURITY

        """
        `EXECUTION` features provide metadata necessary for operation execution.
        """
        EXECUTION
        }

        enum join__Graph {
            TEST @join__graph(name: "test", url: "http://localhost:4001/graphql")
        }

        type MyQueryObject implements Interface {
            object: MyObject
            other: String
        }

        type MyObject {
            data: String
            foo: String
        }

        interface Interface {
            object: MyObject
        }"#;

        let query = "{
            ...FragmentTest
        }
        fragment FragmentTest on Interface {
            object {
                data
            }
        }";

        let schema = schema.parse::<Schema>().expect("could not parse schema");
        let api_schema = schema.api_schema();
        let query = Query::parse(query, &schema).expect("could not parse query");
        let mut response = Response::builder()
            .data(json! {{
                "object": {
                    "__typename": "MyObject",
                    "data": "a",
                    "foo": "bar"
                }
            }})
            .build();

        query.format_response(&mut response, None, Default::default(), api_schema);
        assert_eq_and_ordered!(
            response.data.as_ref().unwrap(),
            &json! {{
                "object": {
                    "data": "a"
                }
            }}
        );
    }

    #[test]
    fn fragment_on_interface() {
        let schema = "type Query
        {
            test_interface: Interface
        }

        interface Interface {
            foo: String
        }

        type MyTypeA implements Interface {
            foo: String
            something: String
        }

        type MyTypeB implements Interface {
            foo: String
            somethingElse: String!
        }
        ";

        assert_format_response_fed2!(
            schema,
            "query  {
                test_interface {
                    __typename
                    foo
                    ...FragmentA
                }
            }

            fragment FragmentA on MyTypeA {
                something
            }

            fragment FragmentB on MyTypeB {
                somethingElse
            }",
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }},
            None,
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }},
        );

        assert_format_response_fed2!(
            schema,
            "query  {
                test_interface {
                    __typename
                    ...FragmentI
                }
            }

            fragment FragmentI on Interface {
                foo
            }
            ",
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar"
                }
            }},
            None,
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar"
                }
            }},
        );

        assert_format_response_fed2!(
            schema,
            "query  {
                test_interface {
                    __typename
                    foo
                    ... on MyTypeA {
                        something
                    }
                    ... on MyTypeB {
                        somethingElse
                    }
                }
            }",
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }},
            None,
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }},
        );

        assert_format_response_fed2!(
            schema,
            "query  {
                test_interface {
                    __typename
                    foo
                    ...FragmentB
                }
            }

            fragment FragmentA on MyTypeA {
                something
            }

            fragment FragmentB on MyTypeB {
                somethingElse
            }",
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                    "something": "something"
                }
            }},
            None,
            json! {{
                "test_interface": {
                    "__typename": "MyTypeA",
                    "foo": "bar",
                }
            }},
        );
    }

    #[test]
    fn parse_introspection_query() {
        let schema = "type Query {
            foo: String
            stuff: Bar
            array: [Bar]
            baz: String
        }
        type Bar {
            bar: String
            baz: String
        }";

        let schema = with_supergraph_boilerplate(schema)
            .parse::<Schema>()
            .expect("could not parse schema");
        let api_schema = schema.api_schema();

        let query = "{
            __type(name: \"Bar\") {
              name
              fields {
                name
                type {
                  name
                }
              }
            }
          }}";
        assert!(Query::parse(query, api_schema)
            .unwrap()
            .operations
            .get(0)
            .unwrap()
            .is_introspection());

        let query = "query {
            __schema {
              queryType {
                name
              }
            }
          }";

        assert!(Query::parse(query, api_schema)
            .unwrap()
            .operations
            .get(0)
            .unwrap()
            .is_introspection());

        let query = "query {
            __typename
          }";

        assert!(Query::parse(query, api_schema)
            .unwrap()
            .operations
            .get(0)
            .unwrap()
            .is_introspection());
    }

    #[test]
    fn fragment_on_union() {
        let schema = "type Query {
            settings: ServiceSettings
        }

        type ServiceSettings {
            location: ServiceLocation
        }

        union ServiceLocation = AccountLocation | Address

        type AccountLocation {
            id: ID
            address: Address
        }

        type Address {
            city: String
        }";

        assert_format_response_fed2!(
            schema,
            "{
                settings {
                  location {
                    ...SettingsLocation
                  }
                }
              }

              fragment SettingsLocation on ServiceLocation {
                ... on Address {
                  city
                }
                 ... on AccountLocation {
                   id
                   address {
                     city
                   }
                 }
              }",
            json! {{
                "settings": {
                    "location": {
                        "__typename": "AccountLocation",
                        "id": "1234"
                    }
                }
            }},
            None,
            json! {{
                "settings": {
                    "location": {
                        "id": "1234",
                        "address": null
                    }
                }
            }},
        );
    }

    #[test]
    fn fragment_on_interface_without_typename() {
        let schema = "type Query {
            inStore(key: String!): InStore!
        }

        type InStore implements CartQueryInterface {
            cart: Cart
            carts: CartQueryResult!
        }

        interface CartQueryInterface {
            carts: CartQueryResult!
            cart: Cart
        }

        type Cart {
            id: ID!
            total: Int!
        }

        type CartQueryResult {
            results: [Cart!]!
            total: Int!
        }";

        assert_format_response_fed2!(
            schema,
            r#"query {
                mtb: inStore(key: "mountainbikes") {
                    ...CartFragmentTest
                }
            }

            fragment CartFragmentTest on CartQueryInterface {
                carts {
                    results {
                        id
                    }
                    total
                }
            }"#,
            json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                    "cart": null
                }
            }},
            None,
            json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                }
            }},
        );

        // With inline fragment
        assert_format_response_fed2!(
            schema,
            r#"query {
                mtb: inStore(key: "mountainbikes") {
                    ... on CartQueryInterface {
                        carts {
                            results {
                                id
                            }
                            total
                        }
                    }
                }
            }"#,
            json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                    "cart": null
                }
            }},
            None,
            json! {{
                "mtb": {
                    "carts": {
                        "results": [{"id": "id"}],
                        "total": 1234
                    },
                }
            }},
        );
    }
}
