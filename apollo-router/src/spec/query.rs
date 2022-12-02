//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::collections::HashSet;

use apollo_parser::ast;
use apollo_parser::ast::AstNode;
use derivative::Derivative;
use graphql::Error;
use serde::de::Visitor;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use tracing::level_filters::LevelFilter;

use crate::error::FetchError;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::PathElement;
use crate::json_ext::Value;
use crate::query_planner::fetch::OperationKind;
use crate::*;

pub(crate) const TYPENAME: &str = "__typename";

/// A GraphQL query.
#[derive(Debug, Derivative, Default, Serialize, Deserialize)]
#[derivative(PartialEq, Hash, Eq)]
pub(crate) struct Query {
    string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    fragments: Fragments,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) operations: Vec<Operation>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) subselections: HashMap<SubSelection, Query>,
}

#[derive(Debug, Derivative, Default)]
#[derivative(PartialEq, Hash, Eq)]
pub(crate) struct SubSelection {
    pub(crate) path: Path,
    pub(crate) subselection: String,
}

impl Serialize for SubSelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = format!("{}|{}", self.path, self.subselection);
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for SubSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(SubSelectionVisitor)
    }
}

struct SubSelectionVisitor;
impl<'de> Visitor<'de> for SubSelectionVisitor {
    type Value = SubSelection;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a string containing the path and the subselection separated by |")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if let Some((path, subselection)) = s.split_once('|') {
            Ok(SubSelection {
                path: Path::from(path),
                subselection: subselection.to_string(),
            })
        } else {
            Err(E::custom("invalid subselection"))
        }
    }
}

impl Query {
    /// Re-format the response value to match this query.
    ///
    /// This will discard unrequested fields and re-order the output to match the order of the
    /// query.
    #[tracing::instrument(skip_all, level = "trace")]
    pub(crate) fn format_response(
        &self,
        response: &mut Response,
        operation_name: Option<&str>,
        is_deferred: bool,
        variables: Object,
        schema: &Schema,
    ) -> Vec<Path> {
        let data = std::mem::take(&mut response.data);
        if let Some(Value::Object(mut input)) = data {
            let operation = self.operation(operation_name);
            if is_deferred {
                if let Some(subselection) = &response.subselection {
                    // Get subselection from hashmap
                    match self.subselections.get(&SubSelection {
                        path: response.path.clone().unwrap_or_default(),
                        subselection: subselection.clone(),
                    }) {
                        Some(subselection_query) => {
                            let mut output = Object::default();
                            let operation = &subselection_query.operations[0];
                            let mut parameters = FormatParameters {
                                variables: &variables,
                                schema,
                                errors: Vec::new(),
                                nullified: Vec::new(),
                            };
                            response.data = Some(
                                match self.apply_root_selection_set(
                                    operation,
                                    &mut parameters,
                                    &mut input,
                                    &mut output,
                                    &mut Path::default(),
                                ) {
                                    Ok(()) => output.into(),
                                    Err(InvalidValue) => Value::Null,
                                },
                            );

                            if !parameters.errors.is_empty() {
                                if let Ok(value) = serde_json_bytes::to_value(&parameters.errors) {
                                    response.extensions.insert("valueCompletion", value);
                                }
                            }

                            return parameters.nullified;
                        }
                        None => failfast_debug!("can't find subselection for {:?}", subselection),
                    }
                // the primary query was empty, we return an empty object
                } else {
                    response.data = Some(Value::Object(Object::default()));
                    return vec![];
                }
            } else if let Some(operation) = operation {
                let mut output = Object::default();

                let all_variables = if operation.variables.is_empty() {
                    variables
                } else {
                    operation
                        .variables
                        .iter()
                        .filter_map(|(k, Variable { default_value, .. })| {
                            default_value.as_ref().map(|v| (k, v))
                        })
                        .chain(variables.iter())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                };

                let mut parameters = FormatParameters {
                    variables: &all_variables,
                    schema,
                    errors: Vec::new(),
                    nullified: Vec::new(),
                };

                response.data = Some(
                    match self.apply_root_selection_set(
                        operation,
                        &mut parameters,
                        &mut input,
                        &mut output,
                        &mut Path::default(),
                    ) {
                        Ok(()) => output.into(),
                        Err(InvalidValue) => Value::Null,
                    },
                );
                if !parameters.errors.is_empty() {
                    if let Ok(value) = serde_json_bytes::to_value(&parameters.errors) {
                        response.extensions.insert("valueCompletion", value);
                    }
                }

                return parameters.nullified;
            } else {
                failfast_debug!("can't find operation for {:?}", operation_name);
            }
        } else {
            failfast_debug!("invalid type for data in response. data: {:#?}", data);
        }

        response.data = Some(Value::default());

        vec![]
    }

    pub(crate) fn parse(
        query: impl Into<String>,
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Self, SpecError> {
        let string = query.into();

        let parser = apollo_parser::Parser::new(string.as_str())
            .recursion_limit(configuration.server.experimental_parser_recursion_limit);
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

    #[allow(clippy::too_many_arguments)]
    fn format_value(
        &self,
        parameters: &mut FormatParameters,
        field_type: &FieldType,
        input: &mut Value,
        output: &mut Value,
        path: &mut Path,
        parent_type: &FieldType,
        selection_set: &[Selection],
    ) -> Result<(), InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            FieldType::NonNull(inner_type) => {
                match self.format_value(
                    parameters,
                    inner_type,
                    input,
                    output,
                    path,
                    field_type,
                    selection_set,
                ) {
                    Err(_) => Err(InvalidValue),
                    Ok(_) => {
                        if output.is_null() {
                            let message = match path.last() {
                                Some(PathElement::Key(k)) => format!(
                                    "Cannot return null for non-nullable field {parent_type}.{}",
                                   k
                                ),
                                Some(PathElement::Index(i)) => format!(
                                    "Cannot return null for non-nullable array element of type {inner_type} at index {}",
                                   i
                                ),
                                _ => todo!(),
                            };
                            parameters.errors.push(Error {
                                message,
                                path: Some(path.clone()),
                                ..Error::default()
                            });

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
                            path.push(PathElement::Index(i));
                            let res = self.format_value(
                                parameters,
                                inner_type,
                                element,
                                &mut output_array[i],
                                path,
                                field_type,
                                selection_set,
                            );
                            path.pop();
                            res
                        }) {
                        Err(InvalidValue) => {
                            parameters.nullified.push(path.clone());
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
                if parameters.schema.custom_scalars.contains(type_name) {
                    *output = input.clone();
                    return Ok(());
                } else if let Some(enum_type) = parameters.schema.enums.get(type_name) {
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
                            if !parameters.schema.object_types.contains_key(input_type) {
                                parameters.nullified.push(path.clone());
                                *output = Value::Null;
                                return Ok(());
                            }
                        }

                        if output.is_null() {
                            *output = Value::Object(Object::default());
                        }
                        let output_object = output.as_object_mut().ok_or(InvalidValue)?;

                        if self
                            .apply_selection_set(
                                selection_set,
                                parameters,
                                input_object,
                                output_object,
                                path,
                                &FieldType::Named(type_name.to_string()),
                            )
                            .is_err()
                        {
                            parameters.nullified.push(path.clone());
                            *output = Value::Null;
                        }

                        Ok(())
                    }
                    _ => {
                        parameters.nullified.push(path.clone());
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
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Path,
        parent_type: &FieldType,
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
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
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
                            path.push(PathElement::Key(field_name.as_str().to_string()));
                            let res = self.format_value(
                                parameters,
                                field_type,
                                input_value,
                                output_value,
                                path,
                                parent_type,
                                selection_set,
                            );
                            path.pop();
                            res?
                        }
                    } else {
                        if !output.contains_key(field_name.as_str()) {
                            output.insert((*field_name).clone(), Value::Null);
                        }
                        if field_type.is_non_null() {
                            parameters.errors.push(Error {
                                message: format!(
                                    "Cannot return null for non-nullable field {parent_type}.{}",
                                    field_name.as_str()
                                ),
                                path: Some(path.clone()),
                                ..Error::default()
                            });

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
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    let is_apply = if let Some(input_type) =
                        input.get(TYPENAME).and_then(|val| val.as_str())
                    {
                        // check if the fragment matches the input type directly, and if not, check if the
                        // input type is a subtype of the fragment's type condition (interface, union)
                        input_type == type_condition.as_str()
                            || parameters.schema.is_subtype(type_condition, input_type)
                    } else {
                        // known_type = true means that from the query's shape, we know
                        // we should get the right type here. But in the case we get a
                        // __typename field and it does not match, we should not apply
                        // that fragment
                        // If the type condition is an interface and the current known type implements it
                        known_type
                            .as_ref()
                            .map(|k| parameters.schema.is_subtype(type_condition, k))
                            .unwrap_or_default()
                            || known_type.as_deref() == Some(type_condition.as_str())
                    };

                    if is_apply {
                        self.apply_selection_set(
                            selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            parent_type,
                        )?;
                    }
                }
                Selection::FragmentSpread {
                    name,
                    known_type,
                    skip,
                    include,
                } => {
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
                        if fragment
                            .skip
                            .should_skip(parameters.variables)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        if !fragment
                            .include
                            .should_include(parameters.variables)
                            .unwrap_or(true)
                        {
                            continue;
                        }

                        let is_apply = if let Some(input_type) =
                            input.get(TYPENAME).and_then(|val| val.as_str())
                        {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            input_type == fragment.type_condition.as_str()
                                || parameters
                                    .schema
                                    .is_subtype(&fragment.type_condition, input_type)
                        } else {
                            // If the type condition is an interface and the current known type implements it
                            known_type
                                .as_ref()
                                .map(|k| parameters.schema.is_subtype(&fragment.type_condition, k))
                                .unwrap_or_default()
                                || known_type.as_deref() == Some(fragment.type_condition.as_str())
                        };

                        if is_apply {
                            self.apply_selection_set(
                                &fragment.selection_set,
                                parameters,
                                input,
                                output,
                                path,
                                parent_type,
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
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Path,
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
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
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
                        path.push(PathElement::Key(field_name_str.to_string()));
                        let res = self.format_value(
                            parameters,
                            field_type,
                            input_value,
                            output_value,
                            path,
                            field_type,
                            selection_set,
                        );
                        path.pop();
                        res?
                    } else if field_name_str == TYPENAME {
                        if !output.contains_key(field_name_str) {
                            output.insert(
                                field_name.clone(),
                                Value::String(operation.kind.to_string().into()),
                            );
                        }
                    } else if field_type.is_non_null() {
                        parameters.errors.push(Error {
                            message: format!(
                                "Cannot return null for non-nullable field {}.{field_name_str}",
                                operation.kind
                            ),
                            path: Some(path.clone()),
                            ..Error::default()
                        });
                        return Err(InvalidValue);
                    } else {
                        output.insert(field_name.clone(), Value::Null);
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    skip,
                    include,
                    ..
                } => {
                    // top level objects will not provide a __typename field
                    if type_condition.as_str()
                        != parameters.schema.root_operation_name(operation.kind)
                    {
                        return Err(InvalidValue);
                    }

                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    self.apply_selection_set(
                        selection_set,
                        parameters,
                        input,
                        output,
                        path,
                        &FieldType::Named(type_condition.clone()),
                    )?;
                }
                Selection::FragmentSpread {
                    name,
                    known_type: _,
                    skip,
                    include,
                } => {
                    if skip.should_skip(parameters.variables).unwrap_or(false) {
                        continue;
                    }

                    if !include.should_include(parameters.variables).unwrap_or(true) {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
                        if fragment
                            .skip
                            .should_skip(parameters.variables)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        if !fragment
                            .include
                            .should_include(parameters.variables)
                            .unwrap_or(true)
                        {
                            continue;
                        }

                        let operation_type_name =
                            parameters.schema.root_operation_name(operation.kind);
                        let is_apply = {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            operation_type_name == fragment.type_condition.as_str()
                                || parameters
                                    .schema
                                    .is_subtype(&fragment.type_condition, operation_type_name)
                        };

                        if !is_apply {
                            return Err(InvalidValue);
                        }

                        self.apply_selection_set(
                            &fragment.selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            &FieldType::Named(operation_type_name.into()),
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
    pub(crate) fn validate_variables(
        &self,
        request: &Request,
        schema: &Schema,
    ) -> Result<(), Response> {
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
            .filter_map(
                |(
                    name,
                    Variable {
                        field_type: ty,
                        default_value,
                    },
                )| {
                    let value = request
                        .variables
                        .get(*name)
                        .or(default_value.as_ref())
                        .unwrap_or(&Value::Null);
                    ty.validate_input_value(value, schema).err().map(|_| {
                        FetchError::ValidationInvalidTypeVariable {
                            name: name.to_string(),
                        }
                        .to_graphql_error(None)
                    })
                },
            )
            .collect::<Vec<_>>();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Response::builder().errors(errors).build())
        }
    }

    pub(crate) fn contains_only_typename(&self) -> bool {
        self.operations.len() == 1 && self.operations[0].is_only_typename()
    }

    pub(crate) fn contains_introspection(&self) -> bool {
        self.operations.iter().any(Operation::is_introspection)
    }

    pub(crate) fn variable_value<'a>(
        &'a self,
        operation_name: Option<&str>,
        variable_name: &str,
        variables: &'a Object,
    ) -> Option<&'a Value> {
        variables
            .get(variable_name)
            .or_else(|| self.default_variable_value(operation_name, variable_name))
    }

    pub(crate) fn default_variable_value(
        &self,
        operation_name: Option<&str>,
        variable_name: &str,
    ) -> Option<&Value> {
        self.operation(operation_name).and_then(|op| {
            op.variables
                .get(variable_name)
                .and_then(|Variable { default_value, .. }| default_value.as_ref())
        })
    }

    fn operation(&self, operation_name: Option<&str>) -> Option<&Operation> {
        match operation_name {
            Some(name) => self
                .operations
                .iter()
                // we should have an error if the only operation is anonymous but the query specifies a name
                .find(|op| {
                    if let Some(op_name) = op.name.as_deref() {
                        op_name == name
                    } else {
                        false
                    }
                }),
            None => self.operations.get(0),
        }
    }

    pub(crate) fn contains_path(&self, path: &Path) -> bool {
        todo!()
    }
}

/// Intermediate structure for arguments passed through the entire formatting
struct FormatParameters<'a> {
    variables: &'a Object,
    errors: Vec<Error>,
    nullified: Vec<Path>,
    schema: &'a Schema,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Operation {
    name: Option<String>,
    kind: OperationKind,
    selection_set: Vec<Selection>,
    variables: HashMap<ByteString, Variable>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Variable {
    field_type: FieldType,
    default_value: Option<Value>,
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
            .ok_or_else(|| {
                SpecError::ParsingError(
                    "the node SelectionSet is not optional in the spec".to_string(),
                )
            })?
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
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "the node Variable is not optional in the spec".to_string(),
                        )
                    })?
                    .name()
                    .ok_or_else(|| {
                        SpecError::ParsingError(
                            "the node Name is not optional in the spec".to_string(),
                        )
                    })?
                    .text()
                    .to_string();
                let ty = FieldType::try_from(definition.ty().ok_or_else(|| {
                    SpecError::ParsingError("the node Type is not optional in the spec".to_string())
                })?)?;

                Ok((
                    ByteString::from(name),
                    Variable {
                        field_type: ty,
                        default_value: parse_default_value(&definition),
                    },
                ))
            })
            .collect::<Result<_, _>>()?;

        Ok(Operation {
            selection_set,
            name,
            variables,
            kind,
        })
    }

    /// A query or mutation containing only `__typename` at the root level
    fn is_only_typename(&self) -> bool {
        self.selection_set.len() == 1
            && self
                .selection_set
                .get(0)
                .map(|s| matches!(s, Selection::Field {name, ..} if name.as_str() == TYPENAME))
                .unwrap_or_default()
    }

    fn is_introspection(&self) -> bool {
        // If the only field is `__typename` it's considered as an introspection query
        if self.is_only_typename() {
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

    pub(crate) fn kind(&self) -> &OperationKind {
        &self.kind
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

pub(crate) fn parse_value(value: &ast::Value) -> Option<Value> {
    match value {
        ast::Value::Variable(_) => None,
        ast::Value::StringValue(s) => String::try_from(s).ok().map(Into::into),
        ast::Value::FloatValue(f) => f64::try_from(f).ok().map(Into::into),
        ast::Value::IntValue(i) => {
            let s = i.source_string();
            s.parse::<i64>()
                .ok()
                .map(Into::into)
                .or_else(|| s.parse::<u64>().ok().map(Into::into))
        }
        ast::Value::BooleanValue(b) => bool::try_from(b).ok().map(Into::into),
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
mod tests;
