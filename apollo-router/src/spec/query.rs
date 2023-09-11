//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::hir;
use apollo_compiler::validation::ValidationDatabase;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::AstDatabase;
use apollo_compiler::FileId;
use apollo_compiler::HirDatabase;
use derivative::Derivative;
use indexmap::IndexSet;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use tracing::level_filters::LevelFilter;

use self::subselections::BooleanValues;
use self::subselections::SubSelectionKey;
use self::subselections::SubSelectionValue;
use crate::error::FetchError;
use crate::error::ValidationErrors;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::ResponsePathElement;
use crate::json_ext::Value;
use crate::query_planner::fetch::OperationKind;
use crate::spec::FieldType;
use crate::spec::Fragments;
use crate::spec::InvalidValue;
use crate::spec::Schema;
use crate::spec::Selection;
use crate::spec::SpecError;
use crate::Configuration;

pub(crate) mod subselections;
pub(crate) mod transform;
pub(crate) mod traverse;

pub(crate) const TYPENAME: &str = "__typename";
pub(crate) const QUERY_EXECUTABLE: &str = "query";

/// A GraphQL query.
#[derive(Derivative, Serialize, Deserialize)]
#[derivative(PartialEq, Hash, Eq, Debug)]
pub(crate) struct Query {
    pub(crate) string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) fragments: Fragments,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) operations: Vec<Operation>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) subselections: HashMap<SubSelectionKey, SubSelectionValue>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) unauthorized_paths: Vec<Path>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) filtered_query: Option<Arc<Query>>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) defer_stats: DeferStats,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) is_original: bool,
    /// Validation errors, used for comparison with the JS implementation.
    ///
    /// `ValidationErrors` is not serde-serializable. If this comes from cache,
    /// the plan ought also to be cached, so we should not need this value anyways.
    /// XXX(@goto-bus-stop): Remove when only Rust validation is used
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    #[serde(skip)]
    pub(crate) validation_error: Option<ValidationErrors>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DeferStats {
    /// Is `@defer` used at all (except `@defer(if=false)`)
    pub(crate) has_defer: bool,

    /// Is `@defer` used without `if` (or `@defer(if=true)`)
    pub(crate) has_unconditional_defer: bool,

    /// Names of boolean variables used in `@defer(if=$var)`
    pub(crate) conditional_defer_variable_names: IndexSet<String>,
}

impl Query {
    pub(crate) fn empty() -> Self {
        Self {
            string: String::new(),
            fragments: Fragments {
                map: HashMap::new(),
            },
            operations: Vec::new(),
            subselections: HashMap::new(),
            unauthorized_paths: Vec::new(),
            filtered_query: None,
            defer_stats: DeferStats {
                has_defer: false,
                has_unconditional_defer: false,
                conditional_defer_variable_names: IndexSet::new(),
            },
            is_original: true,
            validation_error: None,
        }
    }

    /// Re-format the response value to match this query.
    ///
    /// This will discard unrequested fields and re-order the output to match the order of the
    /// query.
    #[tracing::instrument(skip_all, level = "trace")]
    pub(crate) fn format_response(
        &self,
        response: &mut Response,
        operation_name: Option<&str>,
        variables: Object,
        schema: &Schema,
        defer_conditions: BooleanValues,
    ) -> Vec<Path> {
        let data = std::mem::take(&mut response.data);

        let original_operation = self.operation(operation_name);
        match data {
            Some(Value::Object(mut input)) => {
                if self.is_deferred(defer_conditions) {
                    // Get subselection from hashmap
                    match self.subselections.get(&SubSelectionKey {
                        defer_label: response.label.clone(),
                        defer_conditions,
                    }) {
                        Some(subselection) => {
                            let mut output = Object::default();
                            let mut parameters = FormatParameters {
                                variables: &variables,
                                schema,
                                errors: Vec::new(),
                                nullified: Vec::new(),
                            };
                            // Detect if root __typename is asked in the original query (the qp doesn't put root __typename in subselections)
                            // cf https://github.com/apollographql/router/issues/1677
                            let operation_kind_if_root_typename =
                                original_operation.and_then(|op| {
                                    op.selection_set
                                        .iter()
                                        .any(|f| f.is_typename_field())
                                        .then(|| *op.kind())
                                });
                            if let Some(operation_kind) = operation_kind_if_root_typename {
                                output.insert(TYPENAME, operation_kind.as_str().into());
                            }

                            response.data = Some(
                                match self.apply_root_selection_set(
                                    &subselection.type_name,
                                    &subselection.selection_set,
                                    &mut parameters,
                                    &mut input,
                                    &mut output,
                                    &mut Vec::new(),
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
                        None => {
                            response.data = Some(Value::Object(Object::default()));
                            return vec![];
                        }
                    }
                } else if let Some(operation) = original_operation {
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

                    let operation_type_name = schema.root_operation_name(operation.kind);
                    let mut parameters = FormatParameters {
                        variables: &all_variables,
                        schema,
                        errors: Vec::new(),
                        nullified: Vec::new(),
                    };

                    response.data = Some(
                        match self.apply_root_selection_set(
                            operation_type_name,
                            &operation.selection_set,
                            &mut parameters,
                            &mut input,
                            &mut output,
                            &mut Vec::new(),
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
            }
            Some(Value::Null) => {
                // Detect if root __typename is asked in the original query (the qp doesn't put root __typename in subselections)
                // cf https://github.com/apollographql/router/issues/1677
                let operation_kind_if_root_typename = original_operation.and_then(|op| {
                    op.selection_set
                        .iter()
                        .any(|f| f.is_typename_field())
                        .then(|| *op.kind())
                });
                response.data = match operation_kind_if_root_typename {
                    Some(operation_kind) => {
                        let mut output = Object::default();
                        output.insert(TYPENAME, operation_kind.as_str().into());
                        Some(output.into())
                    }
                    None => Some(Value::default()),
                };

                return vec![];
            }
            _ => {
                failfast_debug!("invalid type for data in response. data: {:#?}", data);
            }
        }

        response.data = Some(Value::default());

        vec![]
    }

    pub(crate) fn make_compiler(
        query: &str,
        schema: &Schema,
        configuration: &Configuration,
    ) -> (ApolloCompiler, FileId) {
        let mut compiler = ApolloCompiler::new()
            .recursion_limit(configuration.limits.parser_max_recursion)
            .token_limit(configuration.limits.parser_max_tokens);
        compiler.set_type_system_hir(schema.type_system.clone());
        let id = compiler.add_executable(query, QUERY_EXECUTABLE);
        (compiler, id)
    }

    #[cfg(test)]
    pub(crate) fn parse(
        query: impl Into<String>,
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Self, SpecError> {
        let query = query.into();

        let (compiler, id) = Self::make_compiler(&query, schema, configuration);
        Self::check_errors(&compiler, id)?;
        let (fragments, operations, defer_stats) =
            Self::extract_query_information(&compiler, schema)?;

        Ok(Query {
            string: query,
            fragments,
            operations,
            subselections: HashMap::new(),
            unauthorized_paths: Vec::new(),
            filtered_query: None,
            defer_stats,
            is_original: true,
            validation_error: None,
        })
    }

    /// Check for parse errors in a query in the compiler.
    pub(crate) fn check_errors(compiler: &ApolloCompiler, id: FileId) -> Result<(), SpecError> {
        let ast = compiler.db.ast(id);
        // Trace log recursion limit data
        let recursion_limit = ast.recursion_limit();
        tracing::trace!(?recursion_limit, "recursion limit data");

        let mut parse_errors = ast.errors().peekable();
        if parse_errors.peek().is_some() {
            let text = parse_errors
                .map(|err| err.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            failfast_debug!("parsing error(s): {}", text);
            return Err(SpecError::ParsingError(text));
        }

        Ok(())
    }

    /// Check for validation errors in a query in the compiler.
    pub(crate) fn validate_query(
        compiler: &ApolloCompiler,
        id: FileId,
    ) -> Result<(), ValidationErrors> {
        // Bail out on validation errors, only if the input is expected to be valid
        let diagnostics = compiler.db.validate_executable(id);
        let errors = diagnostics
            .into_iter()
            .filter(|err| err.data.is_error())
            .collect::<Vec<_>>();

        if errors.is_empty() {
            return Ok(());
        }

        Err(ValidationErrors { errors })
    }

    /// Extract serializable data structures from the apollo-compiler HIR.
    pub(crate) fn extract_query_information(
        compiler: &ApolloCompiler,
        schema: &Schema,
    ) -> Result<(Fragments, Vec<Operation>, DeferStats), SpecError> {
        let mut defer_stats = DeferStats {
            has_defer: false,
            has_unconditional_defer: false,
            conditional_defer_variable_names: IndexSet::new(),
        };
        let fragments = Fragments::from_hir(compiler, schema, &mut defer_stats)?;
        let operations = compiler
            .db
            .all_operations()
            .iter()
            .map(|operation| Operation::from_hir(operation, schema, &mut defer_stats))
            .collect::<Result<Vec<_>, SpecError>>()?;

        Ok((fragments, operations, defer_stats))
    }

    #[allow(clippy::too_many_arguments)]
    fn format_value<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &hir::Type,
        input: &mut Value,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        parent_type: &hir::Type,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            hir::Type::NonNull { ty: inner_type, .. } => {
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
                                Some(ResponsePathElement::Key(k)) => format!(
                                    "Cannot return null for non-nullable field {parent_type}.{k}"
                                ),
                                Some(ResponsePathElement::Index(i)) => format!(
                                    "Cannot return null for non-nullable array element of type {inner_type} at index {i}"
                                ),
                                _ => todo!(),
                            };
                            parameters.errors.push(Error {
                                message,
                                path: Some(Path::from_response_slice(path)),
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
            hir::Type::List { ty: inner_type, .. } => match input {
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
                            path.push(ResponsePathElement::Index(i));
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
                            parameters.nullified.push(Path::from_response_slice(path));
                            *output = Value::Null;
                            Ok(())
                        }
                        Ok(()) => Ok(()),
                    }
                }
                _ => Ok(()),
            },
            hir::Type::Named { name, .. } if name == "Int" => {
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
            hir::Type::Named { name, .. } if name == "Float" => {
                if input.as_f64().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            hir::Type::Named { name, .. } if name == "Boolean" => {
                if input.as_bool().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            hir::Type::Named { name, .. } if name == "String" => {
                if input.as_str().is_some() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            hir::Type::Named { name, .. } if name == "Id" => {
                if input.is_string() || input.is_i64() || input.is_u64() || input.is_f64() {
                    *output = input.clone();
                } else {
                    *output = Value::Null;
                }
                Ok(())
            }
            hir::Type::Named {
                name: type_name, ..
            } => {
                let definitions = &parameters.schema.type_system.definitions;
                // we cannot know about the expected format of custom scalars
                // so we must pass them directly to the client
                if definitions.scalars.contains_key(type_name) {
                    *output = input.clone();
                    return Ok(());
                } else if let Some(enum_type) = definitions.enums.get(type_name) {
                    return match input.as_str() {
                        Some(s) => {
                            if enum_type.value(s).is_some() {
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
                            let definitions = &parameters.schema.type_system.definitions;
                            // If there is a __typename, make sure the pointed type is a valid type of the schema. Otherwise, something is wrong, and in case we might
                            // be inadvertently leaking some data for an @inacessible type or something, nullify the whole object. However, do note that due to `@interfaceObject`,
                            // some subgraph can have returned a __typename that is the name of an interface in the supergraph, and this is fine (that is, we should not
                            // return such a __typename to the user, but as long as it's not returned, having it in the internal data is ok and sometimes expected).
                            if !definitions.objects.contains_key(input_type)
                                && !definitions.interfaces.contains_key(input_type)
                            {
                                parameters.nullified.push(Path::from_response_slice(path));
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
                                field_type,
                            )
                            .is_err()
                        {
                            parameters.nullified.push(Path::from_response_slice(path));
                            *output = Value::Null;
                        }

                        Ok(())
                    }
                    _ => {
                        parameters.nullified.push(Path::from_response_slice(path));
                        *output = Value::Null;
                        Ok(())
                    }
                }
            }
        }
    }

    fn apply_selection_set<'a: 'b, 'b>(
        &'a self,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Vec<ResponsePathElement<'b>>,
        parent_type: &hir::Type,
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
                    include_skip,
                } => {
                    let field_name = alias.as_ref().unwrap_or(name);
                    if include_skip.should_skip(parameters.variables) {
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
                        if name.as_str() == TYPENAME {
                            if let Some(input_str) = input_value.as_str() {
                                if parameters
                                    .schema
                                    .type_system
                                    .definitions
                                    .objects
                                    .contains_key(input_str)
                                {
                                    *output_value = input_value.clone();
                                } else {
                                    return Err(InvalidValue);
                                }
                            }
                        } else {
                            path.push(ResponsePathElement::Key(field_name.as_str()));
                            let res = self.format_value(
                                parameters,
                                &field_type.0,
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
                                path: Some(Path::from_response_slice(path)),
                                ..Error::default()
                            });

                            return Err(InvalidValue);
                        }
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    include_skip,
                    defer: _,
                    defer_label: _,
                    known_type,
                } => {
                    if include_skip.should_skip(parameters.variables) {
                        continue;
                    }

                    let is_apply = if let Some(input_type) =
                        input.get(TYPENAME).and_then(|val| val.as_str())
                    {
                        // Only check if the fragment matches the input type directly, and if not, check if the
                        // input type is a subtype of the fragment's type condition (interface, union)
                        input_type == type_condition.as_str()
                            || parameters.schema.is_subtype(type_condition, input_type)
                    } else {
                        known_type
                            .as_ref()
                            // We have no typename, we apply the selection set if the known_type implements the type_condition
                            .map(|k| parameters.schema.is_subtype(type_condition, k))
                            .unwrap_or_default()
                            || known_type.as_deref() == Some(type_condition.as_str())
                    };

                    if is_apply {
                        // if this is the filtered query, we must keep the __typename field because the original query must know the type
                        if !self.is_original {
                            if let Some(input_type) = input.get(TYPENAME) {
                                output.insert(TYPENAME, input_type.clone());
                            }
                        }

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
                    include_skip,
                    defer: _,
                    defer_label: _,
                } => {
                    if include_skip.should_skip(parameters.variables) {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
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
                            // if this is the filtered query, we must keep the __typename field because the original query must know the type
                            if !self.is_original {
                                if let Some(input_type) = input.get(TYPENAME) {
                                    output.insert(TYPENAME, input_type.clone());
                                }
                            }

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

    fn apply_root_selection_set<'a: 'b, 'b>(
        &'a self,
        root_type_name: &str,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Vec<ResponsePathElement<'b>>,
    ) -> Result<(), InvalidValue> {
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    alias,
                    selection_set,
                    field_type,
                    include_skip,
                } => {
                    if include_skip.should_skip(parameters.variables) {
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
                        path.push(ResponsePathElement::Key(field_name_str));
                        let res = self.format_value(
                            parameters,
                            &field_type.0,
                            input_value,
                            output_value,
                            path,
                            &field_type.0,
                            selection_set,
                        );
                        path.pop();
                        res?
                    } else if name.as_str() == TYPENAME {
                        if !output.contains_key(field_name_str) {
                            output.insert(field_name.clone(), Value::String(root_type_name.into()));
                        }
                    } else if field_type.is_non_null() {
                        parameters.errors.push(Error {
                            message: format!(
                                "Cannot return null for non-nullable field {}.{field_name_str}",
                                root_type_name
                            ),
                            path: Some(Path::from_response_slice(path)),
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
                    include_skip,
                    ..
                } => {
                    // top level objects will not provide a __typename field
                    if type_condition.as_str() != root_type_name {
                        return Err(InvalidValue);
                    }

                    if include_skip.should_skip(parameters.variables) {
                        continue;
                    }

                    self.apply_selection_set(
                        selection_set,
                        parameters,
                        input,
                        output,
                        path,
                        &FieldType::new_named(type_condition).0,
                    )?;
                }
                Selection::FragmentSpread {
                    name,
                    known_type: _,
                    include_skip,
                    defer: _,
                    defer_label: _,
                } => {
                    if include_skip.should_skip(parameters.variables) {
                        continue;
                    }

                    if let Some(fragment) = self.fragments.get(name) {
                        let is_apply = {
                            // check if the fragment matches the input type directly, and if not, check if the
                            // input type is a subtype of the fragment's type condition (interface, union)
                            root_type_name == fragment.type_condition.as_str()
                                || parameters
                                    .schema
                                    .is_subtype(&fragment.type_condition, root_type_name)
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
                            &FieldType::new_named(root_type_name).0,
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

    pub(crate) fn operation(&self, operation_name: Option<impl AsRef<str>>) -> Option<&Operation> {
        match operation_name {
            Some(name) => self
                .operations
                .iter()
                // we should have an error if the only operation is anonymous but the query specifies a name
                .find(|op| {
                    if let Some(op_name) = op.name.as_deref() {
                        op_name == name.as_ref()
                    } else {
                        false
                    }
                }),
            None => self.operations.get(0),
        }
    }

    pub(crate) fn contains_error_path(
        &self,
        operation_name: Option<&str>,
        label: &Option<String>,
        path: &Path,
        defer_conditions: BooleanValues,
    ) -> bool {
        let selection_set = match self.subselections.get(&SubSelectionKey {
            defer_label: label.clone(),
            defer_conditions,
        }) {
            Some(subselection) => &subselection.selection_set,
            None => match self.operation(operation_name) {
                None => return false,
                Some(op) => &op.selection_set,
            },
        };
        selection_set
            .iter()
            .any(|selection| selection.contains_error_path(&path.0, &self.fragments))
    }

    pub(crate) fn defer_variables_set(
        &self,
        operation_name: Option<&str>,
        variables: &Object,
    ) -> BooleanValues {
        let mut bits = 0_u32;
        for (i, variable) in self
            .defer_stats
            .conditional_defer_variable_names
            .iter()
            .enumerate()
        {
            let value = variables
                .get(variable.as_str())
                .or_else(|| self.default_variable_value(operation_name, variable));

            if matches!(value, Some(serde_json_bytes::Value::Bool(true))) {
                bits |= 1 << i;
            }
        }

        BooleanValues { bits }
    }

    pub(crate) fn is_deferred(&self, defer_conditions: BooleanValues) -> bool {
        self.defer_stats.has_unconditional_defer || defer_conditions.bits != 0
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
    pub(crate) name: Option<String>,
    kind: OperationKind,
    type_name: String,
    pub(crate) selection_set: Vec<Selection>,
    variables: HashMap<ByteString, Variable>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Variable {
    field_type: FieldType,
    default_value: Option<Value>,
}

impl Operation {
    pub(crate) fn from_hir(
        operation: &hir::OperationDefinition,
        schema: &Schema,
        defer_stats: &mut DeferStats,
    ) -> Result<Self, SpecError> {
        let name = operation.name().map(|s| s.to_owned());
        let kind = operation.operation_ty().into();
        let type_name = schema.root_operation_name(kind).to_owned();
        let current_field_type = FieldType::new_named(&type_name);
        let selection_set = operation
            .selection_set()
            .selection()
            .iter()
            .filter_map(|selection| {
                Selection::from_hir(selection, &current_field_type, schema, 0, defer_stats)
                    .transpose()
            })
            .collect::<Result<_, _>>()?;
        let variables = operation
            .variables()
            .iter()
            .map(|variable| {
                let name = variable.name().into();
                let variable = Variable {
                    field_type: variable.ty().into(),
                    default_value: variable.default_value().and_then(parse_hir_value),
                };
                Ok((name, variable))
            })
            .collect::<Result<_, _>>()?;
        Ok(Operation {
            selection_set,
            name,
            type_name,
            variables,
            kind,
        })
    }

    /// Checks to see if this is a query or mutation containing only
    /// `__typename` at the root level (possibly more than one time, possibly
    /// with aliases). If so, returns Some with a Vec of the output keys
    /// corresponding.
    pub(crate) fn is_only_typenames_with_output_keys(&self) -> Option<Vec<ByteString>> {
        if self.selection_set.is_empty() {
            None
        } else {
            let output_keys: Vec<ByteString> = self
                .selection_set
                .iter()
                .filter_map(|s| s.output_key_if_typename_field())
                .collect();
            if output_keys.len() == self.selection_set.len() {
                Some(output_keys)
            } else {
                None
            }
        }
    }

    fn is_introspection(&self) -> bool {
        // If the only field is `__typename` it's considered as an introspection query
        if self.is_only_typenames_with_output_keys().is_some() {
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

impl From<hir::OperationType> for OperationKind {
    fn from(operation_type: hir::OperationType) -> Self {
        match operation_type {
            hir::OperationType::Query => Self::Query,
            hir::OperationType::Mutation => Self::Mutation,
            hir::OperationType::Subscription => Self::Subscription,
        }
    }
}

pub(crate) fn parse_hir_value(value: &hir::Value) -> Option<Value> {
    match value {
        hir::Value::Variable(_) => None,
        hir::Value::Int { value, .. } => Some((value.get() as i64).into()),
        hir::Value::Float { value, .. } => Some(value.get().into()),
        hir::Value::Null { .. } => Some(Value::Null),
        hir::Value::String { value, .. } => Some(value.as_str().into()),
        hir::Value::Boolean { value, .. } => Some((*value).into()),
        hir::Value::Enum { value, .. } => Some(value.src().into()),
        hir::Value::List { value, .. } => value.iter().map(parse_hir_value).collect(),
        hir::Value::Object { value, .. } => value
            .iter()
            .map(|(k, v)| Some((k.src(), parse_hir_value(v)?)))
            .collect(),
    }
}

#[cfg(test)]
mod tests;
