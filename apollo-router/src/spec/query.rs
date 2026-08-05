//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.
#![allow(clippy::mutable_key_type)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable;
use apollo_compiler::schema::ExtendedType;
use apollo_json::DocumentBuilder;
use apollo_json::NewValue;
use derivative::Derivative;
use indexmap::IndexSet;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use tracing::level_filters::LevelFilter;

use self::subselections::BooleanValues;
use self::subselections::SubSelectionKey;
use self::subselections::SubSelectionValue;
use super::Fragment;
use super::QueryHash;
use crate::Configuration;
use crate::configuration::mode::Mode;
use crate::error::FetchError;
use crate::graphql::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::graphql::json_object::insert_member;
use crate::json_ext;
use crate::json_ext::Path;
use crate::json_ext::ResponsePathElement;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::authorization::UnauthorizedPaths;
use crate::query_planner::fetch::OperationKind;
use crate::services::query_parsing::ParsedDocument;
use crate::services::query_parsing::ParsedDocumentInner;
use crate::services::query_parsing::get_operation;
use crate::spec::FieldType;
use crate::spec::Fragments;
use crate::spec::InvalidValue;
use crate::spec::Schema;
use crate::spec::Selection;
use crate::spec::SpecError;
use crate::spec::query::metrics::observe_query_lexical_token;
use crate::spec::query::metrics::observe_query_recursion;
use crate::spec::schema::ApiSchema;

pub(crate) mod metrics;
pub(crate) mod subselections;
pub(crate) mod transform;
pub(crate) mod traverse;

pub(crate) const TYPENAME: &str = "__typename";
pub(crate) const ERROR_CODE_RESPONSE_VALIDATION: &str = "RESPONSE_VALIDATION_FAILED";
pub(crate) const EXTENSIONS_VALUE_COMPLETION_KEY: &str = "valueCompletion";

/// A GraphQL query.
#[derive(Derivative, Serialize, Deserialize)]
#[derivative(PartialEq, Hash, Eq, Debug)]
pub(crate) struct Query {
    pub(crate) string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) fragments: Fragments,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) operation: Operation,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) subselections: HashMap<SubSelectionKey, SubSelectionValue>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) unauthorized: UnauthorizedPaths,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) filtered_query: Option<Arc<Query>>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) defer_stats: DeferStats,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) is_original: bool,

    /// This is a hash that depends on:
    /// - the query itself
    /// - the schema
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    pub(crate) schema_aware_hash: QueryHash,
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
    /// Returns an empty query. This should be used somewhat carefully and only in tests.
    /// Other parts of the router may not handle empty queries properly.
    ///
    /// FIXME: This should be marked cfg(test) but it's used in places where adding cfg(test) is tricky.
    pub(crate) fn empty_for_tests() -> Self {
        Self {
            string: String::new(),
            fragments: Fragments {
                map: HashMap::new(),
            },
            operation: Operation::empty(),
            subselections: HashMap::new(),
            unauthorized: UnauthorizedPaths::default(),
            filtered_query: None,
            defer_stats: DeferStats {
                has_defer: false,
                has_unconditional_defer: false,
                conditional_defer_variable_names: IndexSet::default(),
            },
            is_original: true,
            schema_aware_hash: QueryHash::default(),
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
        variables: Value,
        schema: &ApiSchema,
        defer_conditions: BooleanValues,
        include_coercion_errors: bool,
    ) -> Vec<Path> {
        let data = std::mem::take(&mut response.data);

        match data {
            Some(input) if input.is_object() => {
                if self.is_deferred(defer_conditions) {
                    // Get subselection from hashmap
                    match self.subselections.get(&SubSelectionKey {
                        defer_label: response.label.clone(),
                        defer_conditions,
                    }) {
                        Some(subselection) => {
                            let mut output = OutputObject::new();
                            let mut parameters = FormatParameters {
                                variables: &variables,
                                schema,
                                errors: Vec::new(),
                                coercion_errors: include_coercion_errors.then(Vec::new),
                                nullified: Vec::new(),
                            };

                            response.data = Some(
                                match self.apply_root_selection_set(
                                    &subselection.type_name,
                                    &subselection.selection_set,
                                    &mut parameters,
                                    &input,
                                    &mut output,
                                    &mut Vec::new(),
                                ) {
                                    Ok(()) => output.into_value(),
                                    Err(InvalidValue) => json_ext::null(),
                                },
                            );

                            if !parameters.errors.is_empty()
                                && let Ok(document) = apollo_json::to_document(&parameters.errors)
                            {
                                response.extensions = insert_member(
                                    std::mem::take(&mut response.extensions),
                                    EXTENSIONS_VALUE_COMPLETION_KEY,
                                    document.root_handle(),
                                );
                            }

                            if let Some(errors) = parameters.coercion_errors.as_mut()
                                && !errors.is_empty()
                            {
                                response.errors.append(errors);
                            }

                            return parameters.nullified;
                        }
                        None => {
                            response.data = Some(json_ext::object([]));
                            return vec![];
                        }
                    }
                } else {
                    let mut output = OutputObject::new();

                    let all_variables: Value = if self.operation.variables.is_empty() {
                        variables
                    } else {
                        let mut builder = DocumentBuilder::new();
                        for (name, Variable { default_value, .. }) in
                            self.operation.variables.iter()
                        {
                            if let Some(value) = default_value {
                                builder
                                    .set(name.as_str(), value.clone())
                                    .expect("the variables builder always has an object root");
                            }
                        }
                        for (name, value) in variables.object_iter() {
                            builder
                                .set(name.as_str(), value)
                                .expect("the variables builder always has an object root");
                        }
                        builder.seal().root_handle()
                    };

                    let operation_type_name = schema
                        .root_operation(self.operation.kind.into())
                        .map(|name| name.as_str())
                        .unwrap_or(self.operation.kind.default_type_name());
                    let mut parameters = FormatParameters {
                        variables: &all_variables,
                        schema,
                        errors: Vec::new(),
                        coercion_errors: include_coercion_errors.then(Vec::new),
                        nullified: Vec::new(),
                    };

                    response.data = Some(
                        match self.apply_root_selection_set(
                            operation_type_name,
                            &self.operation.selection_set,
                            &mut parameters,
                            &input,
                            &mut output,
                            &mut Vec::new(),
                        ) {
                            Ok(()) => output.into_value(),
                            Err(InvalidValue) => json_ext::null(),
                        },
                    );
                    if !parameters.errors.is_empty()
                        && let Ok(document) = apollo_json::to_document(&parameters.errors)
                    {
                        response.extensions = insert_member(
                            std::mem::take(&mut response.extensions),
                            EXTENSIONS_VALUE_COMPLETION_KEY,
                            document.root_handle(),
                        );
                    }

                    if let Some(errors) = parameters.coercion_errors.as_mut()
                        && !errors.is_empty()
                    {
                        response.errors.append(errors);
                    }

                    return parameters.nullified;
                }
            }
            Some(value) if value.is_null() => {
                response.data = Some(json_ext::null());
                return vec![];
            }
            other => {
                failfast_debug!("invalid type for data in response. data: {:#?}", other);
            }
        }

        response.data = Some(json_ext::null());

        vec![]
    }

    pub(crate) fn parse_document(
        query: &str,
        operation_name: Option<&str>,
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<ParsedDocument, SpecError> {
        let parser = &mut apollo_compiler::parser::Parser::new()
            .recursion_limit(configuration.limits.router.parser_max_recursion)
            .token_limit(configuration.limits.router.parser_max_tokens);
        let ast = match parser.parse_ast(query, "query.graphql") {
            Ok(ast) => ast,
            Err(errors) => {
                return Err(SpecError::ParseError(errors.into()));
            }
        };

        let api_schema = schema.api_schema();
        let executable_document = match ast.to_executable_validate(api_schema) {
            Ok(doc) => doc,
            Err(errors) => {
                return Err(SpecError::ValidationError(errors.into()));
            }
        };

        // Trace log recursion limit data
        let recursion_limit = parser.recursion_reached();
        let token_limit = parser.tokens_reached();
        tracing::trace!(?recursion_limit, "recursion limit data");

        observe_query_recursion(recursion_limit);
        observe_query_lexical_token(token_limit);

        let hash = schema.schema_id.operation_hash(query, operation_name);
        ParsedDocumentInner::new(
            ast,
            Arc::new(executable_document),
            operation_name,
            Arc::new(hash),
        )
    }

    #[cfg(test)]
    pub(crate) fn parse(
        query_text: impl Into<String>,
        operation_name: Option<&str>,
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<Self, tower::BoxError> {
        let query_text = query_text.into();

        let doc = Self::parse_document(&query_text, operation_name, schema, configuration)?;
        let (fragments, operation, defer_stats, schema_aware_hash) =
            Self::extract_query_information(schema, &query_text, &doc.executable, operation_name)?;

        Ok(Query {
            string: query_text,
            fragments,
            operation,
            subselections: HashMap::new(),
            unauthorized: UnauthorizedPaths::default(),
            filtered_query: None,
            defer_stats,
            is_original: true,
            schema_aware_hash,
        })
    }

    /// Extract serializable data structures from the apollo-compiler HIR.
    pub(crate) fn extract_query_information(
        schema: &Schema,
        query_text: &str,
        document: &ExecutableDocument,
        operation_name: Option<&str>,
    ) -> Result<(Fragments, Operation, DeferStats, QueryHash), SpecError> {
        let mut defer_stats = DeferStats {
            has_defer: false,
            has_unconditional_defer: false,
            conditional_defer_variable_names: IndexSet::default(),
        };
        let fragments = Fragments::from_hir(document, schema, &mut defer_stats)?;
        let operation = get_operation(document, operation_name)?;
        let operation = Operation::from_hir(&operation, schema, &mut defer_stats, &fragments)?;
        let hash = schema.schema_id.operation_hash(query_text, operation_name);

        Ok((fragments, operation, defer_stats, hash))
    }

    /// Format a field's value.
    /// - Returns Err(InvalidValue) if formatting fails and error(s) have been emitted.
    fn format_value<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        input: &Value,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            executable::Type::Named(name) => match name.as_str() {
                // Scalar formatters return `Err` on coercion failure.  Propagating `Err` here
                // avoids redundant "Null value found for non-nullable" errors.
                "Int" => self.format_integer(parameters, path, input, output)?,
                "Float" => self.format_float(parameters, path, input, output)?,
                "Boolean" => self.format_boolean(parameters, path, input, output)?,
                "String" => self.format_string(parameters, path, input, output)?,
                "Id" => self.format_id(parameters, path, input, output)?,
                _ => self.format_named_type(
                    parameters,
                    field_type,
                    input,
                    name,
                    output,
                    path,
                    selection_set,
                )?,
            },
            // if the list contains nonnullable types, we will receive a Err(InvalidValue)
            // and should replace the entire list with null
            // if the types are nullable, the inner call to filter_errors will take care
            // of setting the current entry to null
            executable::Type::List(inner_type) => {
                self.format_list(parameters, input, inner_type, output, path, selection_set)?
            }
            // for non null types, we validate with the inner type, then if we get an InvalidValue
            // we set it to null and immediately return an error instead of Ok(()), because we
            // want the error to go up until the next nullable parent
            executable::Type::NonNullNamed(_) | executable::Type::NonNullList(_) => self
                .format_non_nullable_value(
                    parameters,
                    field_type,
                    input,
                    output,
                    path,
                    selection_set,
                )?,
        }
        Ok(())
    }

    /// Format a non-null field's value.
    /// - Returns Err(InvalidValue) if formatting fails and error(s) have been emitted.
    #[inline]
    fn format_non_nullable_value<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        input: &Value,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        let inner_type = match field_type {
            executable::Type::NonNullList(ty) => ty.clone().list(),
            executable::Type::NonNullNamed(name) => executable::Type::Named(name.clone()),
            // This function should never be called for non-nullable types
            _ => {
                tracing::error!("`format_non_nullable_value` was called with a nullable type!!");
                debug_assert!(field_type.is_non_null());
                return Err(InvalidValue);
            }
        };

        let inner_result =
            self.format_value(parameters, &inner_type, input, output, path, selection_set);

        if output.is_null() {
            let message = format!("Null value found for non-nullable type {inner_type}");
            match inner_result {
                Ok(()) => {
                    // Null value from the subgraph (explicit null for a non-null position) without
                    // coercion error from formatting. Emit errors here.
                    parameters.errors.push(
                        Error::builder()
                            .message(&message)
                            .path(Path::from_response_slice(path))
                            .build(),
                    );
                    parameters.insert_coercion_error(
                        Error::builder()
                            .message(message)
                            .path(Path::from_response_slice(path))
                            .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                            .build(),
                    );
                }
                Err(InvalidValue) => {
                    // The `format_value` errored. Decide based on `inner_type`:
                    //   - List: `format_list` only returns `Err` when an element's
                    //     `format_value` returned `Err` with a non-null element type. Skip.
                    //   - Composite (Object/Interface/Union): the Err came from a child
                    //     selection set, which already emitted both sinks at the originating
                    //     leaf. Skip.
                    //   - Otherwise (primitive scalar / Enum / custom scalar): the scalar
                    //     formatter emitted coercion errors but does not know it sits in a
                    //     non-null position; Emit a valueCompletion error here.
                    let inner_emitted_error = inner_type.is_list()
                        || matches!(
                            parameters.schema.types.get(inner_type.inner_named_type()),
                            Some(
                                ExtendedType::Object(_)
                                    | ExtendedType::Interface(_)
                                    | ExtendedType::Union(_)
                            )
                        );
                    if !inner_emitted_error {
                        parameters.errors.push(
                            Error::builder()
                                .message(message)
                                .path(Path::from_response_slice(path))
                                .build(),
                        );
                    }
                }
            }
            // Propagate error to parent
            Err(InvalidValue)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn format_list<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        input: &Value,
        inner_type: &executable::Type,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        let Some(input_array) = input.as_array() else {
            return Ok(());
        };
        // Elements are formatted into this vector and written back as one array,
        // since an apollo-json value cannot be mutated through a handle.
        let mut output_array = if output.is_null() {
            vec![json_ext::null(); input_array.len()]
        } else {
            output.as_array().ok_or(InvalidValue)?
        };
        let result = input_array.iter().enumerate().try_for_each(|(i, element)| {
            path.push(ResponsePathElement::Index(i));
            let res = self.format_value(
                parameters,
                inner_type,
                element,
                &mut output_array[i],
                path,
                selection_set,
            );
            path.pop();
            // Type-aware Err handling: non-null inner type propagates (whole list
            // nullifies per spec). Nullable inner type swallows the error (element already
            // nullified by child).
            if res.is_err() && inner_type.is_non_null() {
                return Err(InvalidValue);
            }
            Ok(())
        });
        if let Err(InvalidValue) = result {
            parameters.nullified.push(Path::from_response_slice(path));
            // Emit only at the innermost list level (when inner_type is not a list).
            // We don't want to emit multiple errors for a nested list type like [[Int!]!]!.
            if !inner_type.is_list() {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message(format!(
                            "Invalid value found inside the array of type [{inner_type}]"
                        ))
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
            }
            *output = json_ext::null();
            return Err(InvalidValue);
        }
        *output = json_ext::array(output_array);
        Ok(())
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn format_named_type<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        input: &Value,
        type_name: &Name,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        // we cannot know about the expected format of custom scalars
        // so we must pass them directly to the client
        match parameters.schema.types.get(type_name) {
            Some(ExtendedType::Scalar(_)) => {
                *output = input.clone();
                return Ok(());
            }
            Some(ExtendedType::Enum(enum_type)) => {
                *output = input
                    .as_str_owned()
                    .filter(|s| enum_type.values.contains_key(s.as_str()))
                    .map(|_| input.clone())
                    .unwrap_or_default();
                return Ok(());
            }
            _ => {}
        }

        if input.is_object() {
            if let Some(input_type) = input.get(TYPENAME).and_then(|val| val.as_str_owned()) {
                // If there is a __typename, make sure the pointed type is a valid type of the
                // schema. Otherwise, something is wrong, and in case we might be inadvertently
                // leaking some data for an @inacessible type or something, nullify the whole
                // object. However, do note that due to `@interfaceObject`, some subgraph can have
                // returned a __typename that is the name of an interface in the supergraph, and
                // this is fine (that is, we should not return such a __typename to the user, but
                // as long as it's not returned, having it in the internal data is ok and sometimes
                // expected).
                let Some(ExtendedType::Object(_) | ExtendedType::Interface(_)) =
                    parameters.schema.types.get(input_type.as_str())
                else {
                    parameters.nullified.push(Path::from_response_slice(path));
                    *output = json_ext::null();
                    return Ok(());
                };
            }

            let mut output_object = if output.is_null() {
                OutputObject::new()
            } else if output.is_object() {
                OutputObject::from_value(output)
            } else {
                return Err(InvalidValue);
            };

            let typename = input
                .get(TYPENAME)
                .and_then(|val| val.as_str_owned())
                .and_then(|s| apollo_compiler::ast::NamedType::new(&s).ok())
                .map(apollo_compiler::ast::Type::Named);

            let current_type = match parameters.schema.types.get(field_type.inner_named_type()) {
                Some(ExtendedType::Interface(..) | ExtendedType::Union(..)) => {
                    typename.as_ref().unwrap_or(field_type)
                }
                _ => field_type,
            };

            if let Err(err) = self.apply_selection_set(
                selection_set,
                parameters,
                input,
                &mut output_object,
                path,
                current_type,
            ) {
                parameters.nullified.push(Path::from_response_slice(path));
                *output = json_ext::null();
                // Propagate the Err, since `apply_selection_set` already emitted an error.
                return Err(err);
            }
            *output = output_object.into_value();
        } else {
            parameters.nullified.push(Path::from_response_slice(path));
            *output = json_ext::null();
            // We don't emit errors for null object value nor propagate Err.
            // Note: `format_non_nullable_value` will emit an error if this object's type is
            //       non-nullable.
        }

        Ok(())
    }

    #[inline]
    fn format_integer(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: &mut Value,
    ) -> Result<(), InvalidValue> {
        // if the value is invalid, we do not insert it in the output object
        // which is equivalent to inserting null
        if input.is_valid_int_input() {
            *output = input.clone();
            Ok(())
        } else {
            *output = json_ext::null();
            if input.is_null() {
                Ok(())
            } else {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type Int")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
                Err(InvalidValue)
            }
        }
    }

    #[inline]
    fn format_float(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: &mut Value,
    ) -> Result<(), InvalidValue> {
        if input.is_valid_float_input() {
            *output = input.clone();
            Ok(())
        } else {
            *output = json_ext::null();
            if input.is_null() {
                Ok(())
            } else {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type Float")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
                Err(InvalidValue)
            }
        }
    }

    #[inline]
    fn format_boolean(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: &mut Value,
    ) -> Result<(), InvalidValue> {
        if input.as_bool().is_some() {
            *output = input.clone();
            Ok(())
        } else {
            *output = json_ext::null();
            if input.is_null() {
                Ok(())
            } else {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type Boolean")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
                Err(InvalidValue)
            }
        }
    }

    #[inline]
    fn format_string(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: &mut Value,
    ) -> Result<(), InvalidValue> {
        if input.as_str().is_some() {
            *output = input.clone();
            Ok(())
        } else {
            *output = json_ext::null();
            if input.is_null() {
                Ok(())
            } else {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type String")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
                Err(InvalidValue)
            }
        }
    }

    #[inline]
    fn format_id(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: &mut Value,
    ) -> Result<(), InvalidValue> {
        if input.is_string() || input.is_number() {
            *output = input.clone();
            Ok(())
        } else {
            *output = json_ext::null();
            if input.is_null() {
                Ok(())
            } else {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type ID")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
                Err(InvalidValue)
            }
        }
    }

    fn apply_selection_set<'a: 'b, 'b>(
        &'a self,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters,
        input: &Value,
        output: &mut OutputObject<'a>,
        path: &mut Vec<ResponsePathElement<'b>>,
        // the type under which we apply selections
        current_type: &executable::Type,
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

                    if name.as_str() == TYPENAME {
                        let object_type = parameters
                            .schema
                            .get_object(current_type.inner_named_type())
                            .or_else(|| {
                                let input_value = input.get(field_name.as_str())?.as_str_owned()?;
                                parameters.schema.get_object(&input_value)
                            });

                        if let Some(object_type) = object_type {
                            output.insert(field_name.as_str(), object_type.name.as_str());
                        } else {
                            // If the __typename value does not resolve to a known object type in
                            // the schema nor the current_type is an object type, emit an error.
                            // TODO: __typename could be an interface type (due to @interfaceObject).
                            //       That case is currently not handled and results in false error.
                            emit_missing_field(parameters, field_type, field_name.as_str(), path);
                            return Err(InvalidValue);
                        }
                        continue;
                    }

                    if let Some(input_value) = input.get(field_name.as_str()) {
                        let mut output_value = match output.get(field_name.as_str()) {
                            Some(existing) => {
                                // if there's already a value for that key in the output it means either:
                                // - the value is a scalar and was already copied into output
                                // - the value was already null and is already present in output
                                // if we expect an object or list at that key, output will already contain
                                // an object or list and then input_value cannot be null

                                // A prior fragment spread may have nullified this field due to a non-null
                                // constraint violation. Object-typed inputs keep their value, so
                                // input_value stays non-null even after nullification; without this guard
                                // a later fragment would re-enter format_value and overwrite the null.
                                if input_value.is_null() || existing.is_null() {
                                    continue;
                                }
                                existing
                            }
                            None => json_ext::null(),
                        };

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        path.push(ResponsePathElement::Key(field_name.as_str()));
                        let res = self.format_value(
                            parameters,
                            &field_type.0,
                            &input_value,
                            &mut output_value,
                            path,
                            selection_set,
                        );
                        path.pop();
                        output.insert(field_name.as_str(), output_value);
                        // Type-aware Err handling: non-null fields propagate Err to continue the
                        // bubble; nullable fields swallow (the child already reported, the field
                        // is already nullified).
                        if res.is_err() && field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    } else {
                        if !output.contains_key(field_name.as_str()) {
                            output.insert(field_name.as_str(), ());
                        }
                        // Emit error for missing field
                        emit_missing_field(parameters, field_type, field_name.as_str(), path);
                        if field_type.is_non_null() {
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
                    known_type: _,
                } => {
                    if include_skip.should_skip(parameters.variables) {
                        continue;
                    }

                    // NOTE: The subtype logic is strange. We are trying to determine if a fragment
                    // should be applied, but we don't have the __typename of the selection set
                    // (otherwise, we would be on a different branch). Consider the following query
                    // for a union Thing = Foo | Bar:
                    // { thing { ... on Foo { foo }, ... on Bar { bar } } }
                    //
                    // As we process the `... on Foo` fragment, `Foo` is `type_condition` and
                    // `Thing` is `current_type`, we *could* reverse the order in calling
                    // `is_subtype` and apply the fragment; however, the same is true for the `Bar`
                    // fragment. Without the type info of the data we have in our response, we
                    // can't know which to apply (or if both should apply in the case of
                    // interfaces).
                    //
                    // Without that information, this is the best we can do without construction a
                    // much more complicated reformatting heuristic.
                    //
                    // This formatter processes fragments sequentially rather than pre-merging them
                    // via CollectFields (as the GraphQL spec prescribes) — root cause of several
                    // correctness bugs, tracked in ROUTER-740.
                    let is_apply = current_type.inner_named_type().as_str()
                        == type_condition.as_str()
                        || parameters
                            .schema
                            .is_subtype(type_condition, current_type.inner_named_type().as_str());

                    if is_apply {
                        // if this is the filtered query, we must keep the __typename field because the original query must know the type
                        if !self.is_original
                            && let Some(input_type) = input.get(TYPENAME)
                        {
                            output.insert(TYPENAME, input_type);
                        }

                        self.apply_selection_set(
                            selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            current_type,
                        )?;
                    }
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

                    if let Some(Fragment {
                        type_condition,
                        selection_set,
                    }) = self.fragments.get(name)
                    {
                        // NOTE: This subtype logic is a bit strange. See the InlineFragment
                        // branch for why its done this way.
                        let is_apply = current_type.inner_named_type().as_str()
                            == type_condition.as_str()
                            || parameters.schema.is_subtype(
                                type_condition,
                                current_type.inner_named_type().as_str(),
                            );

                        if is_apply {
                            // if this is the filtered query, we must keep the __typename field because the original query must know the type
                            if !self.is_original
                                && let Some(input_type) = input.get(TYPENAME)
                            {
                                output.insert(TYPENAME, input_type);
                            }

                            self.apply_selection_set(
                                selection_set,
                                parameters,
                                input,
                                output,
                                path,
                                current_type,
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
        input: &Value,
        output: &mut OutputObject<'a>,
        path: &mut Vec<ResponsePathElement<'b>>,
    ) -> Result<(), InvalidValue> {
        // Track which named fragments have already been applied during this root
        // selection-set traversal. Re-applying a `...Frag` at the same (input,
        // output, root_type_name, path) is idempotent — the same fields would be
        // written from the same input — so the second application can be skipped.
        // This collapses exponential fragment-of-fragment blowups (e.g. `L1 = ...L0
        // ...L0`, `L2 = ...L1 ...L1`, ...) into linear work.
        let mut applied_fragments: HashSet<&'a str> = HashSet::new();
        self.apply_root_selection_set_cached(
            root_type_name,
            selection_set,
            parameters,
            input,
            output,
            path,
            &mut applied_fragments,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_root_selection_set_cached<'a: 'b, 'b>(
        &'a self,
        root_type_name: &str,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters,
        input: &Value,
        output: &mut OutputObject<'a>,
        path: &mut Vec<ResponsePathElement<'b>>,
        applied_fragments: &mut HashSet<&'a str>,
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

                    if name.as_str() == TYPENAME {
                        if !output.contains_key(field_name_str) {
                            output.insert(field_name_str, root_type_name);
                        }
                    } else if let Some(input_value) = input.get(field_name_str) {
                        let mut output_value = match output.get(field_name_str) {
                            Some(existing) => {
                                // if there's already a value for that key in the output it means either:
                                // - the value is a scalar and was already copied into output
                                // - the value was already null and is already present in output
                                // if we expect an object or list at that key, output will already contain
                                // an object or list and then input_value cannot be null

                                // A prior fragment spread may have nullified this field due to a non-null
                                // constraint violation. Object-typed inputs keep their value, so
                                // input_value stays non-null even after nullification; without this guard
                                // a later fragment would re-enter format_value and overwrite the null.
                                if input_value.is_null() || existing.is_null() {
                                    continue;
                                }
                                existing
                            }
                            None => json_ext::null(),
                        };

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        path.push(ResponsePathElement::Key(field_name_str));
                        let res = self.format_value(
                            parameters,
                            &field_type.0,
                            &input_value,
                            &mut output_value,
                            path,
                            selection_set,
                        );
                        path.pop();
                        output.insert(field_name_str, output_value);
                        // Type-aware Err handling (mirrors `apply_selection_set`): non-null fields
                        // propagate Err to continue the bubble; nullable fields swallow (child
                        // already reported, field is already nullified).
                        if res.is_err() && field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    } else {
                        output.insert(field_name_str, ());
                        emit_missing_field(parameters, field_type, field_name_str, path);
                        if field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    }
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set,
                    include_skip,
                    ..
                } => {
                    if include_skip.should_skip(parameters.variables) {
                        continue;
                    }

                    // check if the fragment matches the input type directly, and if not, check if the
                    // input type is a subtype of the fragment's type condition (interface, union)
                    let is_apply = (root_type_name == type_condition.as_str())
                        || parameters.schema.is_subtype(type_condition, root_type_name);

                    if is_apply {
                        // Inline fragments share the named-fragment cache with their
                        // parent so an anonymous `... on T { ...Frag }` wrapper still
                        // benefits from de-duplication of `...Frag`.
                        self.apply_root_selection_set_cached(
                            root_type_name,
                            selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            applied_fragments,
                        )?;
                    }
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

                    // Skip if we have already applied this named fragment during the
                    // current root-selection-set traversal. The first application
                    // wrote every reachable field; a second application would write
                    // the same values, so it is safe to omit.
                    if !applied_fragments.insert(name.as_str()) {
                        continue;
                    }

                    if let Some(Fragment {
                        type_condition,
                        selection_set,
                    }) = self.fragments.get(name)
                    {
                        // check if the fragment matches the input type directly, and if not, check if the
                        // input type is a subtype of the fragment's type condition (interface, union)
                        let is_apply = (root_type_name == type_condition.as_str())
                            || parameters.schema.is_subtype(type_condition, root_type_name);

                        if is_apply {
                            self.apply_root_selection_set_cached(
                                root_type_name,
                                selection_set,
                                parameters,
                                input,
                                output,
                                path,
                                applied_fragments,
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

    /// Validate a [`Request`]'s variables against this [`Query`] using a provided [`Schema`].
    #[tracing::instrument(skip_all, level = "trace")]
    // `Response` is large, but this is not a frequently used function
    #[allow(clippy::result_large_err)]
    pub(crate) fn validate_variables(
        &self,
        request: &Request,
        schema: &Schema,
        strict_variable_validation: Mode,
    ) -> Result<(), Response> {
        if LevelFilter::current() >= LevelFilter::DEBUG {
            let known_variables = self
                .operation
                .variables
                .keys()
                .map(|k| k.as_str())
                .collect::<HashSet<_>>();
            let unknown_variables = request
                .variables
                .object_iter()
                .map(|(name, _)| name)
                .filter(|name| !known_variables.contains(name.as_str()))
                .collect::<Vec<_>>();
            if !unknown_variables.is_empty() {
                failfast_debug!(
                    "Received variable unknown to the query: {:?}",
                    unknown_variables,
                );
            }
        }

        let errors = self
            .operation
            .variables
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
                        .get(name.as_str())
                        .or_else(|| default_value.clone());
                    let path = super::JsonValuePath::Variable {
                        name: name.as_str(),
                    };
                    ty.validate_input_value(
                        value.as_ref(),
                        schema,
                        &path,
                        strict_variable_validation,
                    )
                    .err()
                    .map(|message| {
                        FetchError::ValidationInvalidTypeVariable {
                            name: name.clone(),
                            message,
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

    /// The value a variable holds for this request, falling back to the operation's
    /// declared default. An apollo-json value is a handle rather than a place, so
    /// this hands back an owned handle sharing the caller's arena.
    pub(crate) fn variable_value(&self, variable_name: &str, variables: &Value) -> Option<Value> {
        variables
            .get(variable_name)
            .or_else(|| self.default_variable_value(variable_name).cloned())
    }

    pub(crate) fn default_variable_value(&self, variable_name: &str) -> Option<&Value> {
        self.operation
            .variables
            .get(variable_name)
            .and_then(|Variable { default_value, .. }| default_value.as_ref())
    }

    pub(crate) fn contains_error_path(
        &self,
        label: &Option<String>,
        path: &Path,
        defer_conditions: BooleanValues,
    ) -> bool {
        let selection_set = match self.subselections.get(&SubSelectionKey {
            defer_label: label.clone(),
            defer_conditions,
        }) {
            Some(subselection) => &subselection.selection_set,
            None => &self.operation.selection_set,
        };
        let match_length = selection_set
            .iter()
            .map(|selection| selection.matching_error_path_length(&path.0, &self.fragments))
            .max()
            .unwrap_or(0);
        path.len() == match_length
    }

    pub(crate) fn matching_error_path_length(&self, path: &Path) -> usize {
        self.operation
            .selection_set
            .iter()
            .map(|selection| selection.matching_error_path_length(&path.0, &self.fragments))
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn defer_variables_set(&self, variables: &Value) -> BooleanValues {
        let mut bits = 0_u32;
        for (i, variable) in self
            .defer_stats
            .conditional_defer_variable_names
            .iter()
            .enumerate()
        {
            let value = self.variable_value(variable.as_str(), variables);

            if value.and_then(|value| value.as_bool()) == Some(true) {
                bits |= 1 << i;
            }
        }

        BooleanValues { bits }
    }

    pub(crate) fn is_deferred(&self, defer_conditions: BooleanValues) -> bool {
        self.defer_stats.has_unconditional_defer || defer_conditions.bits != 0
    }
}

/// Emit a error for a user-asked field that is missing from the merged response. The caller
/// handles null-bubbling on non-nullable fields; this helper only emits.
///
/// Two sinks, asymmetric coverage:
///
/// - `parameters.errors` → `extensions.valueCompletion`, at the **parent path** (the path on
///   entry, without the field name pushed — the formatter never recursed into the missing field,
///   so the leaf doesn't exist in any meaningful sense for valueCompletion). **Only fires for
///   non-null fields**, preserving the legacy "valueCompletion is for non-null-coerced positions"
///   convention.
/// - `parameters.insert_coercion_error` → `response.errors`, at the **leaf path** `[...,
///   field_name]` with code `RESPONSE_VALIDATION_FAILED`. Fires for both nullable and non-null
///   missing fields — gated only by `enable_result_coercion_errors` (i.e.
///   `coercion_errors.is_some()`).
///
/// Net effect: nullable missing surfaces only in `response.errors` when coercion is on (no new
/// valueCompletion noise); non-null missing behaves as before plus also lands in
/// `response.errors`.
fn emit_missing_field<'b>(
    parameters: &mut FormatParameters,
    field_type: &crate::spec::FieldType,
    field_name: &'b str,
    path: &mut Vec<ResponsePathElement<'b>>,
) {
    // valueCompletion: non-null only
    if field_type.is_non_null() {
        // Based on historic discussion, we report this missing field error at the parent path (not
        // the missing field path), since the parent is the one being nullified. Though the field
        // name/path could be more informative.
        let message = format!(
            "Cannot return null for non-nullable type {}",
            field_type.0.inner_named_type()
        );
        let parent_path = Path::from_response_slice(path);
        parameters
            .errors
            .push(Error::builder().message(message).path(parent_path).build());
    }

    // response.errors: nullable and non-null both, gated by coercion config.
    if parameters.coercion_errors.is_some() {
        path.push(ResponsePathElement::Key(field_name));
        let field_path = Path::from_response_slice(path);
        path.pop();
        parameters.insert_coercion_error(
            Error::builder()
                .message("Missing field")
                .path(field_path)
                .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                .build(),
        );
    }
}

/// One output object under construction, holding its members in the order the
/// query requested them.
///
/// [`OutputObject::into_value`] seals the members into a value in one pass,
/// adopting each member by reference, so a subtree that passed through
/// formatting unchanged keeps sharing the arena it came from.
struct OutputObject<'a> {
    members: Vec<(Cow<'a, str>, NewValue)>,
}

impl<'a> OutputObject<'a> {
    fn new() -> Self {
        OutputObject {
            members: Vec::new(),
        }
    }

    /// Reopens an object value for further members, adopting each by reference.
    fn from_value(value: &Value) -> Self {
        OutputObject {
            members: value
                .object_iter()
                .map(|(key, member)| (Cow::Owned(key), NewValue::Node(member)))
                .collect(),
        }
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.members.iter().position(|(member, _)| &**member == key)
    }

    fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    /// A handle to the member already written at `key`.
    fn get(&self, key: &str) -> Option<Value> {
        let (_, member) = &self.members[self.position(key)?];
        Some(match member {
            NewValue::Node(handle) => handle.clone(),
            NewValue::Null => json_ext::null(),
            NewValue::Bool(value) => json_ext::bool_value(*value),
            NewValue::Int(value) => json_ext::from_i64(*value),
            NewValue::Float(value) => json_ext::from_f64(*value),
            NewValue::String(value) => json_ext::string(value.as_str()),
        })
    }

    /// Writes `value` at `key`, keeping the position of a key already present.
    fn insert(&mut self, key: impl Into<Cow<'a, str>>, value: impl Into<NewValue>) {
        let key = key.into();
        let existing = self.position(&key);
        match existing {
            Some(index) => self.members[index].1 = value.into(),
            None => self.members.push((key, value.into())),
        }
    }

    fn into_value(self) -> Value {
        let mut builder = DocumentBuilder::new();
        for (key, member) in self.members {
            builder
                .set(&*key, member)
                .expect("a fresh object root accepts any key");
        }
        builder.seal().root_handle()
    }
}

/// Intermediate structure for arguments passed through the entire formatting
struct FormatParameters<'a> {
    variables: &'a Value,
    errors: Vec<Error>,
    coercion_errors: Option<Vec<Error>>,
    nullified: Vec<Path>,
    schema: &'a ApiSchema,
}

impl FormatParameters<'_> {
    fn insert_coercion_error(&mut self, error: Error) {
        if let Some(errors) = self.coercion_errors.as_mut() {
            errors.push(error)
        }
    }
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
    fn empty() -> Self {
        Self {
            name: None,
            kind: OperationKind::Query,
            type_name: "".into(),
            selection_set: Vec::new(),
            variables: HashMap::new(),
        }
    }

    pub(crate) fn from_hir(
        operation: &executable::Operation,
        schema: &Schema,
        defer_stats: &mut DeferStats,
        fragments: &Fragments,
    ) -> Result<Self, SpecError> {
        let name = operation.name.as_ref().map(|s| s.as_str().to_owned());
        let kind = operation.operation_type.into();
        let type_name = schema.root_operation_name(kind).to_owned();

        let selection_set = operation
            .selection_set
            .selections
            .iter()
            .filter_map(|selection| {
                Selection::from_hir(selection, &type_name, schema, 0, defer_stats, fragments)
                    .transpose()
            })
            .collect::<Result<_, _>>()?;
        let variables = operation
            .variables
            .iter()
            .map(|variable| {
                let name = variable.name.as_str().into();
                let variable = Variable {
                    field_type: variable.ty.as_ref().into(),
                    default_value: variable
                        .default_value
                        .as_ref()
                        .and_then(|v| parse_hir_value(v)),
                };
                (name, variable)
            })
            .collect();

        Ok(Operation {
            selection_set,
            name,
            type_name,
            variables,
            kind,
        })
    }

    pub(crate) fn kind(&self) -> &OperationKind {
        &self.kind
    }
}

pub(crate) fn parse_hir_value(value: &executable::Value) -> Option<Value> {
    match value {
        executable::Value::Variable(_) => None,
        executable::Value::Int(value) => {
            Some(json_ext::from_i64(value.as_str().parse::<i64>().ok()?))
        }
        executable::Value::Float(value) => Some(json_ext::from_f64(value.try_to_f64().ok()?)),
        executable::Value::Null => Some(json_ext::null()),
        executable::Value::String(value) => Some(json_ext::string(value.as_str())),
        executable::Value::Boolean(value) => Some(json_ext::bool_value(*value)),
        executable::Value::Enum(value) => Some(json_ext::string(value.as_str())),
        executable::Value::List(value) => {
            let items = value
                .iter()
                .map(|v| parse_hir_value(v))
                .collect::<Option<Vec<_>>>()?;
            Some(json_ext::array(items))
        }
        executable::Value::Object(value) => {
            let entries = value
                .iter()
                .map(|(k, v)| Some((k.as_str().to_owned(), parse_hir_value(v)?)))
                .collect::<Option<Vec<_>>>()?;
            Some(json_ext::object(entries))
        }
    }
}

#[cfg(test)]
mod tests;
