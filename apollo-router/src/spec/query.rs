//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable;
use apollo_compiler::schema::ExtendedType;
use apollo_json::JsonKind;
use apollo_json::NewValue;
use apollo_json::PendingId;
use apollo_json::PendingTree;
use apollo_json::ValueBuilder;
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
use crate::graphql::json_object::empty_object;
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
    /// Formats a response for an operation that authorization may have filtered,
    /// returning the nullified paths from every pass.
    ///
    /// The filtered query formats first, projecting the data onto the authorized shape
    /// and discarding fields the subgraphs returned beyond it (via `@requires`, or a
    /// misbehaving subgraph). The original query formats second, expanding the result to
    /// the shape the client requested and nulling what filtering removed. One pass cannot
    /// do both: the original alone returns unauthorized values, the filtered alone drops
    /// requested fields from the response.
    pub(crate) fn format_response_filtered_then_original(
        &self,
        response: &mut Response,
        variables: Value,
        schema: &ApiSchema,
        defer_conditions: BooleanValues,
        include_coercion_errors: bool,
    ) -> Vec<Path> {
        let mut paths = Vec::new();
        if let Some(filtered_query) = self.filtered_query.as_ref() {
            paths = filtered_query.format_response(
                response,
                variables.clone(),
                schema,
                defer_conditions,
                include_coercion_errors,
            );
        }
        paths.extend(self.format_response(
            response,
            variables,
            schema,
            defer_conditions,
            include_coercion_errors,
        ));
        paths
    }

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
                        defer_path: response
                            .path
                            .as_ref()
                            .map(subselections::defer_path_from_response_path)
                            .unwrap_or_default(),
                    }) {
                        Some(subselection) => {
                            let mut parameters = FormatParameters {
                                variables: &variables,
                                schema,
                                errors: Vec::new(),
                                coercion_errors: include_coercion_errors.then(Vec::new),
                                nullified: Vec::new(),
                                output: PendingTree::new(),
                            };
                            let root = parameters.output.object();

                            response.data = Some(
                                match self.apply_root_selection_set(
                                    &subselection.type_name,
                                    &subselection.selection_set,
                                    &mut parameters,
                                    &input,
                                    root,
                                    &mut Vec::new(),
                                ) {
                                    Ok(()) => parameters.seal(root),
                                    Err(InvalidValue) => json_ext::null(),
                                },
                            );

                            if !parameters.errors.is_empty()
                                && let Ok(document) = apollo_json::to_value(&parameters.errors)
                            {
                                response.extensions = insert_member(
                                    std::mem::take(&mut response.extensions),
                                    EXTENSIONS_VALUE_COMPLETION_KEY,
                                    document,
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
                            response.data = Some(empty_object());
                            return vec![];
                        }
                    }
                } else {
                    let all_variables: Value = if self.operation.variables.is_empty() {
                        variables
                    } else {
                        let mut builder = ValueBuilder::new();
                        for (name, Variable { default_value, .. }) in
                            self.operation.variables.iter()
                        {
                            if let Some(value) = default_value {
                                // Writing a handle at a key of an object root
                                // cannot fail; only a non-finite float can, and
                                // a handle is not one.
                                let _ = builder.set(name.as_str(), value.clone());
                            }
                        }
                        for (name, value) in variables.object_iter() {
                            // Infallible for the same reason as above.
                            let _ = builder.set(&*name, value);
                        }
                        builder.seal()
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
                        output: PendingTree::new(),
                    };
                    let root = parameters.output.object();

                    response.data = Some(
                        match self.apply_root_selection_set(
                            operation_type_name,
                            &self.operation.selection_set,
                            &mut parameters,
                            &input,
                            root,
                            &mut Vec::new(),
                        ) {
                            Ok(()) => parameters.seal(root),
                            Err(InvalidValue) => json_ext::null(),
                        },
                    );
                    if !parameters.errors.is_empty()
                        && let Ok(document) = apollo_json::to_value(&parameters.errors)
                    {
                        response.extensions = insert_member(
                            std::mem::take(&mut response.extensions),
                            EXTENSIONS_VALUE_COMPLETION_KEY,
                            document,
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
                // XXX(@goto-bus-stop): this should raise an execution error?
                tracing::debug!("invalid type for data in response. data: {other:#?}");
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
        parameters: &mut FormatParameters<'a>,
        field_type: &executable::Type,
        input: &Value,
        output: PendingId,
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
        parameters: &mut FormatParameters<'a>,
        field_type: &executable::Type,
        input: &Value,
        output: PendingId,
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

        if parameters.output.is_null(output) {
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
        parameters: &mut FormatParameters<'a>,
        input: &Value,
        inner_type: &executable::Type,
        output: PendingId,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        let Some(input_array) = input.as_array() else {
            return Ok(());
        };
        // Elements are formatted in place in this pending array. On failure
        // the slot is nulled, so a list abandoned partway through never
        // leaves half an array behind.
        let tree = &mut parameters.output;
        if tree.is_array(output) {
            // An earlier fragment already formatted this list; keep what it
            // wrote and let the selections below merge into it.
        } else if tree.is_null(output) {
            // Nothing written here yet: one pending null per input element. A
            // nullified slot reads as a null too, matching the `is_null` check
            // this replaced — that accepted any spelling of null, so this does.
            tree.set_array(output);
            for _ in 0..input_array.len() {
                tree.push(output);
            }
        } else {
            // Some other value is already at this slot, so the list cannot be
            // formatted over it. An adopted handle cannot appear here: a
            // list-typed field only ever reaches this function, which writes a
            // pending array or a null.
            tree.set(output, NewValue::Null);
            return Err(InvalidValue);
        }
        let mut element = tree.first(output);
        let result = input_array.iter().enumerate().try_for_each(|(i, input)| {
            let slot = match element {
                Some(slot) => slot,
                None => parameters.output.push(output),
            };
            element = parameters.output.next(slot);
            path.push(ResponsePathElement::Index(i));
            let res = self.format_value(parameters, inner_type, input, slot, path, selection_set);
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
            parameters.output.set(output, NewValue::Null);
            return Err(InvalidValue);
        }
        Ok(())
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn format_named_type<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters<'a>,
        field_type: &executable::Type,
        input: &Value,
        type_name: &Name,
        output: PendingId,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        // we cannot know about the expected format of custom scalars
        // so we must pass them directly to the client
        match parameters.schema.types.get(type_name) {
            Some(ExtendedType::Scalar(_)) => {
                parameters.output.set(output, NewValue::Node(input.clone()));
                return Ok(());
            }
            Some(ExtendedType::Enum(enum_type)) => {
                // An enum value outside the schema's value set is nullified,
                // exactly as the previous `unwrap_or_default()` on a value did.
                let value = match input
                    .as_str()
                    .filter(|value| enum_type.values.contains_key(value.as_ref()))
                {
                    Some(_) => NewValue::Node(input.clone()),
                    None => NewValue::Null,
                };
                parameters.output.set(output, value);
                return Ok(());
            }
            _ => {}
        }

        if input.is_object() {
            // One member scan for both uses below: the schema check here and the
            // concrete-type resolution that follows, which takes the schema's own
            // name for the type rather than an owned copy of the input's.
            let input_typename = input.get(TYPENAME);
            let mut typename = None;
            if let Some(input_type) = input_typename.as_ref().and_then(|val| val.as_str()) {
                // If there is a __typename, make sure the pointed type is a valid type of the
                // schema. Otherwise, something is wrong, and in case we might be inadvertently
                // leaking some data for an @inacessible type or something, nullify the whole
                // object. However, do note that due to `@interfaceObject`, some subgraph can have
                // returned a __typename that is the name of an interface in the supergraph, and
                // this is fine (that is, we should not return such a __typename to the user, but
                // as long as it's not returned, having it in the internal data is ok and sometimes
                // expected).
                let Some((name, ExtendedType::Object(_) | ExtendedType::Interface(_))) =
                    parameters.schema.types.get_key_value(input_type.as_ref())
                else {
                    parameters.nullified.push(Path::from_response_slice(path));
                    parameters.output.set(output, NewValue::Null);
                    return Ok(());
                };
                typename = Some(apollo_compiler::ast::Type::Named(name.clone()));
            }

            // On failure the slot is nulled, so an object abandoned partway
            // through never leaves a half-formatted object behind.
            let tree = &mut parameters.output;
            if tree.is_object(output) {
                // An earlier fragment already formatted members here; keep
                // them and let this selection set merge into them.
            } else if tree.is_null(output) {
                // Nothing written here yet, in any spelling of null — the
                // `is_null` check this replaced accepted them all.
                tree.set_object(output);
            } else {
                // Some other value is already at this slot. An adopted handle
                // cannot appear here: a field with a selection set only reaches
                // this function, which writes a pending object or a null.
                tree.set(output, NewValue::Null);
                return Err(InvalidValue);
            }

            let current_type = match parameters.schema.types.get(field_type.inner_named_type()) {
                Some(ExtendedType::Interface(..) | ExtendedType::Union(..)) => {
                    typename.as_ref().unwrap_or(field_type)
                }
                _ => field_type,
            };

            // The spec's `CollectFields` step 1: "if visitedFragments is not provided,
            // initialize it to the empty set". Each object gets a fresh set, so a
            // fragment applied on a nested object can neither suppress nor be
            // suppressed by the same fragment on an ancestor.
            let mut visited_fragments = HashSet::new();

            if let Err(err) = self.apply_selection_set(
                selection_set,
                parameters,
                input,
                output,
                path,
                current_type,
                &mut visited_fragments,
            ) {
                parameters.nullified.push(Path::from_response_slice(path));
                parameters.output.set(output, NewValue::Null);
                // Propagate the Err, since `apply_selection_set` already emitted an error.
                return Err(err);
            }
        } else {
            parameters.nullified.push(Path::from_response_slice(path));
            parameters.output.set(output, NewValue::Null);
            // We don't emit errors for null object value nor propagate Err.
            // Note: `format_non_nullable_value` will emit an error if this object's type is
            //       non-nullable.
        }

        Ok(())
    }

    #[inline]
    fn format_integer(
        &self,
        parameters: &mut FormatParameters<'_>,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: PendingId,
    ) -> Result<(), InvalidValue> {
        // if the value is invalid, we do not insert it in the output object
        // which is equivalent to inserting null
        if input.is_valid_int_input() {
            parameters.output.set(output, NewValue::Node(input.clone()));
            Ok(())
        } else {
            parameters.output.set(output, NewValue::Null);
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
        parameters: &mut FormatParameters<'_>,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: PendingId,
    ) -> Result<(), InvalidValue> {
        if input.is_valid_float_input() {
            parameters.output.set(output, NewValue::Node(input.clone()));
            Ok(())
        } else {
            parameters.output.set(output, NewValue::Null);
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
        parameters: &mut FormatParameters<'_>,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: PendingId,
    ) -> Result<(), InvalidValue> {
        if input.as_bool().is_some() {
            parameters.output.set(output, NewValue::Node(input.clone()));
            Ok(())
        } else {
            parameters.output.set(output, NewValue::Null);
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
        parameters: &mut FormatParameters<'_>,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: PendingId,
    ) -> Result<(), InvalidValue> {
        if input.kind() == JsonKind::String {
            parameters.output.set(output, NewValue::Node(input.clone()));
            Ok(())
        } else {
            parameters.output.set(output, NewValue::Null);
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
        parameters: &mut FormatParameters<'_>,
        path: &[ResponsePathElement<'_>],
        input: &Value,
        output: PendingId,
    ) -> Result<(), InvalidValue> {
        if input.is_string() || input.is_number() {
            parameters.output.set(output, NewValue::Node(input.clone()));
            Ok(())
        } else {
            parameters.output.set(output, NewValue::Null);
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

    #[allow(clippy::too_many_arguments)]
    fn apply_selection_set<'a: 'b, 'b>(
        &'a self,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters<'a>,
        input: &Value,
        output: PendingId,
        path: &mut Vec<ResponsePathElement<'b>>,
        // the type under which we apply selections
        current_type: &executable::Type,
        // The spec's `visitedFragments` (CollectFields, step 3.d): shared with the
        // recursive calls for inline fragments and fragment bodies on the same
        // object; each new object gets a fresh set from its caller in
        // `format_named_type`.
        visited_fragments: &mut HashSet<&'a str>,
    ) -> Result<(), InvalidValue> {
        // Subgraphs usually return fields in the order the query selected
        // them, so member lookups resume from the previous hit instead of
        // rescanning the object from the start for every field.
        let mut member_cursor = 0;
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
                            insert(
                                parameters,
                                output,
                                field_name.as_str(),
                                object_type.name.as_str(),
                            );
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

                    if let Some(input_value) =
                        input.get_with_cursor(field_name.as_str(), &mut member_cursor)
                    {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was already copied into output
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null

                        // A prior fragment spread may have nullified this field due to a non-null
                        // constraint violation. Object-typed inputs keep their value, so
                        // input_value stays non-null even after nullification; without this guard
                        // a later fragment would re-enter format_value and overwrite the null.
                        if let Some(existing) =
                            parameters.output.member(output, field_name.as_str())
                            && (input_value.is_null() || parameters.output.is_null(existing))
                        {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        path.push(ResponsePathElement::Key(field_name.as_str()));
                        // Format through the member's slot: a field seen for the
                        // first time is appended as a pending null and written in
                        // place, so nothing is moved out of the object and back.
                        let slot = parameters.output.member_slot(output, field_name.as_str());
                        let res = self.format_value(
                            parameters,
                            &field_type.0,
                            &input_value,
                            slot,
                            path,
                            selection_set,
                        );
                        path.pop();
                        // Type-aware Err handling: non-null fields propagate Err to continue the
                        // bubble; nullable fields swallow (the child already reported, the field
                        // is already nullified).
                        if res.is_err() && field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    } else {
                        // Appends a null only when the key is new.
                        parameters.output.member_slot(output, field_name.as_str());
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
                            insert(parameters, output, TYPENAME, input_type);
                        }

                        // Inline fragments collect fields into the same object, so
                        // they share `visited_fragments` with their parent — an
                        // anonymous `... on T { ...Frag }` wrapper still
                        // de-duplicates `...Frag` (CollectFields step 3.c.iv).
                        self.apply_selection_set(
                            selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            current_type,
                            visited_fragments,
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

                    // Skip if we have already applied this named fragment to this
                    // object. The first application wrote every reachable field; a
                    // second application would write the same values, so it is safe
                    // to omit. Recorded before the fragment-existence and
                    // type-condition checks, as in CollectFields step 3.d.iii, so a
                    // leaf reached via distinct chain names (`...Leaf` then
                    // `...ChainA` where ChainA spreads Leaf) still de-duplicates.
                    if !visited_fragments.insert(name.as_str()) {
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
                                insert(parameters, output, TYPENAME, input_type);
                            }

                            self.apply_selection_set(
                                selection_set,
                                parameters,
                                input,
                                output,
                                path,
                                current_type,
                                visited_fragments,
                            )?;
                        }
                    } else {
                        // the fragment should have been already checked with the schema
                        // XXX(@goto-bus-stop): can we store a reference to the fragment instead of
                        // just a name?
                        tracing::error!(
                            name = name,
                            "missing fragment in response formatting, this is a bug"
                        );
                        return Err(InvalidValue);
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_root_selection_set<'a: 'b, 'b>(
        &'a self,
        root_type_name: &'a str,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters<'a>,
        input: &Value,
        output: PendingId,
        path: &mut Vec<ResponsePathElement<'b>>,
    ) -> Result<(), InvalidValue> {
        // Track which named fragments have already been applied to the root object
        // — the spec's `visitedFragments` (CollectFields, step 1: "if
        // visitedFragments is not provided, initialize it to the empty set").
        // Re-applying a `...Frag` at the same (input, output, root_type_name, path)
        // is idempotent — the same fields would be written from the same input — so
        // the second application can be skipped. This collapses exponential
        // fragment-of-fragment blowups (e.g. `L1 = ...L0 ...L0`, `L2 = ...L1 ...L1`,
        // ...) into linear work.
        let mut visited_fragments: HashSet<&'a str> = HashSet::new();
        self.apply_root_selection_set_cached(
            root_type_name,
            selection_set,
            parameters,
            input,
            output,
            path,
            &mut visited_fragments,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_root_selection_set_cached<'a: 'b, 'b>(
        &'a self,
        root_type_name: &'a str,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters<'a>,
        input: &Value,
        output: PendingId,
        path: &mut Vec<ResponsePathElement<'b>>,
        visited_fragments: &mut HashSet<&'a str>,
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
                        if parameters.output.member(output, field_name_str).is_none() {
                            insert(parameters, output, field_name_str, root_type_name);
                        }
                    } else if let Some(input_value) = input.get(field_name_str) {
                        // if there's already a value for that key in the output it means either:
                        // - the value is a scalar and was already copied into output
                        // - the value was already null and is already present in output
                        // if we expect an object or list at that key, output will already contain
                        // an object or list and then input_value cannot be null

                        // A prior fragment spread may have nullified this field due to a non-null
                        // constraint violation. Object-typed inputs keep their value, so
                        // input_value stays non-null even after nullification; without this guard
                        // a later fragment would re-enter format_value and overwrite the null.
                        if let Some(existing) = parameters.output.member(output, field_name_str)
                            && (input_value.is_null() || parameters.output.is_null(existing))
                        {
                            continue;
                        }

                        let selection_set = selection_set.as_deref().unwrap_or_default();
                        path.push(ResponsePathElement::Key(field_name_str));
                        // Format through the member's slot, as in
                        // `apply_selection_set`.
                        let slot = parameters.output.member_slot(output, field_name_str);
                        let res = self.format_value(
                            parameters,
                            &field_type.0,
                            &input_value,
                            slot,
                            path,
                            selection_set,
                        );
                        path.pop();
                        // Type-aware Err handling (mirrors `apply_selection_set`): non-null fields
                        // propagate Err to continue the bubble; nullable fields swallow (child
                        // already reported, field is already nullified).
                        if res.is_err() && field_type.is_non_null() {
                            return Err(InvalidValue);
                        }
                    } else {
                        insert(parameters, output, field_name_str, ());
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
                        // Inline fragments collect fields into the same object, so
                        // they share `visited_fragments` with their parent — an
                        // anonymous `... on T { ...Frag }` wrapper still
                        // de-duplicates `...Frag` (CollectFields step 3.c.iv).
                        self.apply_root_selection_set_cached(
                            root_type_name,
                            selection_set,
                            parameters,
                            input,
                            output,
                            path,
                            visited_fragments,
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

                    // Skip if we have already applied this named fragment to the root
                    // object. The first application wrote every reachable field; a
                    // second application would write the same values, so it is safe
                    // to omit. Recorded before the fragment-existence and
                    // type-condition checks, as in CollectFields step 3.d.iii, so a
                    // leaf reached via distinct chain names (`...Leaf` then
                    // `...ChainA` where ChainA spreads Leaf) still de-duplicates.
                    if !visited_fragments.insert(name.as_str()) {
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
                                visited_fragments,
                            )?;
                        }
                    } else {
                        // the fragment should have been already checked with the schema
                        // XXX(@goto-bus-stop): can we store a reference to the fragment instead of
                        // just a name?
                        tracing::error!(
                            name = name,
                            "missing fragment in response formatting, this is a bug"
                        );
                        return Err(InvalidValue);
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
                .filter(|name| !known_variables.contains(&**name))
                .collect::<Vec<_>>();
            if !unknown_variables.is_empty() {
                tracing::debug!(
                    unknown_variables = ?unknown_variables,
                    "Received variable unknown to the query",
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
        response_path: Option<&Path>,
        path: &Path,
        defer_conditions: BooleanValues,
    ) -> bool {
        let selection_set = match self.subselections.get(&SubSelectionKey {
            defer_label: label.clone(),
            defer_conditions,
            defer_path: response_path
                .map(subselections::defer_path_from_response_path)
                .unwrap_or_default(),
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

/// Writes `value` at `key` of the pending object `object`, keeping the
/// position of a key already present.
fn insert<'a>(
    parameters: &mut FormatParameters<'a>,
    object: PendingId,
    key: &'a str,
    value: impl Into<NewValue<'a>>,
) {
    let slot = parameters.output.member_slot(object, key);
    parameters.output.set(slot, value);
}

/// Intermediate structure for arguments passed through the entire formatting
struct FormatParameters<'a> {
    variables: &'a Value,
    errors: Vec<Error>,
    coercion_errors: Option<Vec<Error>>,
    nullified: Vec<Path>,
    schema: &'a ApiSchema,
    /// The response under construction. Members are *pending*: they reach an
    /// arena only when the root is sealed, once, by
    /// [`FormatParameters::seal`]. Every object and list of the response is
    /// held in this one tree, so formatting allocates its buffer's growth
    /// rather than a vector per object, and a subtree that nullability
    /// propagation later discards is never written to an arena. A subtree
    /// that came through formatting unchanged is adopted by reference from
    /// the arena it arrived in. Keys borrow from the parsed query.
    output: PendingTree<'a>,
}

impl FormatParameters<'_> {
    /// Seals the response under `root` into one document — one arena for
    /// the entire response, rather than one per object.
    fn seal(&mut self, root: PendingId) -> Value {
        std::mem::take(&mut self.output).seal(root)
    }

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
