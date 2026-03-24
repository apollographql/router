//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::executable;
use apollo_compiler::schema::ExtendedType;
use derivative::Derivative;
use indexmap::IndexMap;
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
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::ResponsePathElement;
use crate::json_ext::Value;
use crate::plugins::authorization::UnauthorizedPaths;
use crate::query_planner::fetch::OperationKind;
use crate::services::layers::query_analysis::ParsedDocument;
use crate::services::layers::query_analysis::ParsedDocumentInner;
use crate::services::layers::query_analysis::get_operation;
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
        variables: Object,
        schema: &ApiSchema,
        defer_conditions: BooleanValues,
        include_coercion_errors: bool,
    ) -> Vec<Path> {
        let data = std::mem::take(&mut response.data);

        match data {
            Some(Value::Object(mut input)) => {
                if self.is_deferred(defer_conditions) {
                    // Get subselection from hashmap
                    match self.subselections.get(&SubSelectionKey {
                        defer_label: response.label.clone(),
                        defer_conditions,
                    }) {
                        Some(subselection) => {
                            let mut output =
                                Object::with_capacity(subselection.selection_set.len());
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
                                    &mut input,
                                    &mut output,
                                    &mut Vec::new(),
                                ) {
                                    Ok(()) => output.into(),
                                    Err(InvalidValue) => Value::Null,
                                },
                            );

                            if !parameters.errors.is_empty()
                                && let Ok(value) = serde_json_bytes::to_value(&parameters.errors)
                            {
                                response
                                    .extensions
                                    .insert(EXTENSIONS_VALUE_COMPLETION_KEY, value);
                            }

                            return parameters.nullified;
                        }
                        None => {
                            response.data = Some(Value::Object(Object::default()));
                            return vec![];
                        }
                    }
                } else {
                    let mut output = Object::with_capacity(self.operation.selection_set.len());

                    let all_variables = if self.operation.variables.is_empty() {
                        variables
                    } else {
                        self.operation
                            .variables
                            .iter()
                            .filter_map(|(k, Variable { default_value, .. })| {
                                default_value.as_ref().map(|v| (k, v))
                            })
                            .chain(variables.iter())
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
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
                            &mut input,
                            &mut output,
                            &mut Vec::new(),
                        ) {
                            Ok(()) => output.into(),
                            Err(InvalidValue) => Value::Null,
                        },
                    );
                    if !parameters.errors.is_empty()
                        && let Ok(value) = serde_json_bytes::to_value(&parameters.errors)
                    {
                        response
                            .extensions
                            .insert(EXTENSIONS_VALUE_COMPLETION_KEY, value);
                    }

                    if let Some(errors) = parameters.coercion_errors.as_mut()
                        && !errors.is_empty()
                    {
                        response.errors.append(errors);
                    }

                    return parameters.nullified;
                }
            }
            Some(Value::Null) => {
                response.data = Some(Value::Null);
                return vec![];
            }
            _ => {
                failfast_debug!("invalid type for data in response. data: {:#?}", data);
            }
        }

        response.data = Some(Value::Null);

        vec![]
    }

    pub(crate) fn parse_document(
        query: &str,
        operation_name: Option<&str>,
        schema: &Schema,
        configuration: &Configuration,
    ) -> Result<ParsedDocument, SpecError> {
        let parser = &mut apollo_compiler::parser::Parser::new()
            .recursion_limit(configuration.limits.parser_max_recursion)
            .token_limit(configuration.limits.parser_max_tokens);
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

    /// <https://spec.graphql.org/October2021/#CompleteValue()>
    ///
    /// Validates and formats a resolved field value according to its GraphQL type.
    /// For object/interface/union types, merges the sub-selection sets of all
    /// field nodes sharing this response key (`MergeSelectionSets`) and recurses
    /// via `apply_selection_set`.
    fn complete_value<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        // All Field nodes that share this response key; their sub-selection sets
        // are merged (MergeSelectionSets) when completing an object value.
        fields: &[&'a Selection],
        input: &mut Value,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
    ) -> Result<(), InvalidValue> {
        match field_type {
            executable::Type::NonNullNamed(_) | executable::Type::NonNullList(_) => {
                let inner_type = match field_type {
                    executable::Type::NonNullList(ty) => ty.clone().list(),
                    executable::Type::NonNullNamed(name) => executable::Type::Named(name.clone()),
                    _ => unreachable!(),
                };
                self.complete_value(parameters, &inner_type, fields, input, output, path)?;
                if output.is_null() {
                    let message = format!("Null value found for non-nullable type {inner_type}");
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
                    return Err(InvalidValue);
                }
            }
            executable::Type::List(inner_type) => {
                let Value::Array(input_array) = input else {
                    return Ok(());
                };
                if output.is_null() {
                    *output = Value::Array(vec![Value::Null; input_array.len()]);
                }
                let output_array = output.as_array_mut().ok_or(InvalidValue)?;
                if let Err(InvalidValue) =
                    input_array
                        .iter_mut()
                        .enumerate()
                        .try_for_each(|(i, element)| {
                            path.push(ResponsePathElement::Index(i));
                            self.complete_value(
                                parameters,
                                inner_type,
                                fields,
                                element,
                                &mut output_array[i],
                                path,
                            )?;
                            path.pop();
                            Ok(())
                        })
                {
                    // We pop here because, if an error is found, the path still
                    // contains the index of the invalid value.
                    path.pop();
                    parameters.nullified.push(Path::from_response_slice(path));
                    parameters.insert_coercion_error(
                        Error::builder()
                            .message(format!(
                                "Invalid value found inside the array of type [{inner_type}]"
                            ))
                            .path(Path::from_response_slice(path))
                            .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                            .build(),
                    );
                    *output = Value::Null;
                }
            }
            executable::Type::Named(type_name) => match type_name.as_str() {
                "Int" => self.format_integer(parameters, path, input, output),
                "Float" => self.format_float(parameters, path, input, output),
                "Boolean" => self.format_boolean(parameters, path, input, output),
                "String" => self.format_string(parameters, path, input, output),
                "Id" => self.format_id(parameters, path, input, output),
                _ => {
                    match parameters.schema.types.get(type_name) {
                        Some(ExtendedType::Scalar(_)) => {
                            *output = input.clone();
                        }
                        Some(ExtendedType::Enum(enum_type)) => {
                            *output = input
                                .as_str()
                                .filter(|s| enum_type.values.contains_key(*s))
                                .map(|_| input.clone())
                                .unwrap_or_default();
                        }
                        _ => {
                            // Object, Interface, or Union — apply sub-selections.
                            if let Value::Object(input_object) = input {
                                if let Some(input_type) =
                                    input_object.get(TYPENAME).and_then(|v| v.as_str())
                                {
                                    // If there is a __typename, make sure the pointed type is a
                                    // valid type of the schema. Otherwise, something is wrong,
                                    // and in case we might be inadvertently leaking some data
                                    // for an @inaccessible type or something, nullify the whole
                                    // object. However, do note that due to `@interfaceObject`,
                                    // some subgraph can have returned a __typename that is the
                                    // name of an interface in the supergraph, and this is fine
                                    // (that is, we should not return such a __typename to the
                                    // user, but as long as it's not returned, having it in the
                                    // internal data is ok and sometimes expected).
                                    let Some(ExtendedType::Object(_) | ExtendedType::Interface(_)) =
                                        parameters.schema.types.get(input_type)
                                    else {
                                        parameters.nullified.push(Path::from_response_slice(path));
                                        *output = Value::Null;
                                        return Ok(());
                                    };
                                }

                                if output.is_null() {
                                    *output = Value::Object(Object::with_capacity(fields.len()));
                                }
                                let output_object = output.as_object_mut().ok_or(InvalidValue)?;

                                let typename = input_object
                                    .get(TYPENAME)
                                    .and_then(|val| val.as_str())
                                    .and_then(|s| apollo_compiler::ast::NamedType::new(s).ok())
                                    .map(apollo_compiler::ast::Type::Named);

                                let current_type = match parameters.schema.types.get(type_name) {
                                    Some(ExtendedType::Interface(..) | ExtendedType::Union(..)) => {
                                        typename.as_ref().unwrap_or(field_type)
                                    }
                                    _ => field_type,
                                };

                                // MergeSelectionSets: flatten sub-selections from all
                                // field nodes sharing this response key.
                                let merged = fields.iter().flat_map(|sel| {
                                    if let Selection::Field {
                                        selection_set: Some(ss),
                                        ..
                                    } = sel
                                    {
                                        ss.iter()
                                    } else {
                                        (&[] as &[Selection]).iter()
                                    }
                                });

                                if self
                                    .apply_selection_set(
                                        merged,
                                        parameters,
                                        input_object,
                                        output_object,
                                        path,
                                        current_type,
                                    )
                                    .is_err()
                                {
                                    parameters.nullified.push(Path::from_response_slice(path));
                                    *output = Value::Null;
                                }
                            } else {
                                parameters.nullified.push(Path::from_response_slice(path));
                                *output = Value::Null;
                            }
                        }
                    }
                }
            },
        }
        Ok(())
    }

    #[inline]
    fn format_integer(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &mut Value,
        output: &mut Value,
    ) {
        // if the value is invalid, we do not insert it in the output object
        // which is equivalent to inserting null
        if input.as_i64().is_some_and(|i| i32::try_from(i).is_ok())
            || input.as_i64().is_some_and(|i| i32::try_from(i).is_ok())
        {
            *output = input.clone();
        } else {
            if !input.is_null() {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type Int")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
            }
            *output = Value::Null;
        }
    }

    #[inline]
    fn format_float(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &mut Value,
        output: &mut Value,
    ) {
        if input.as_f64().is_some() {
            *output = input.clone();
        } else {
            if !input.is_null() {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type Float")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
            }
            *output = Value::Null;
        }
    }

    #[inline]
    fn format_boolean(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &mut Value,
        output: &mut Value,
    ) {
        if input.as_bool().is_some() {
            *output = input.clone();
        } else {
            if !input.is_null() {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type Boolean")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
            }
            *output = Value::Null;
        }
    }

    #[inline]
    fn format_string(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &mut Value,
        output: &mut Value,
    ) {
        if input.as_str().is_some() {
            *output = input.clone();
        } else {
            if !input.is_null() {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type String")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
            }
            *output = Value::Null;
        }
    }

    #[inline]
    fn format_id(
        &self,
        parameters: &mut FormatParameters,
        path: &[ResponsePathElement<'_>],
        input: &mut Value,
        output: &mut Value,
    ) {
        if input.is_string() || input.is_i64() || input.is_u64() || input.is_f64() {
            *output = input.clone();
        } else {
            if !input.is_null() {
                parameters.insert_coercion_error(
                    Error::builder()
                        .message("Invalid value found for the type ID")
                        .path(Path::from_response_slice(path))
                        .extension("code", ERROR_CODE_RESPONSE_VALIDATION)
                        .build(),
                );
            }
            *output = Value::Null;
        }
    }

    /// <https://spec.graphql.org/October2021/#CollectFields()>
    ///
    /// Traverses `selections` (and any inline/named fragments it contains) and
    /// groups every `Field` selection by its response name (alias if present,
    /// otherwise field name) into `grouped_fields`.  The `@skip` and `@include`
    /// directives are honoured; each named fragment is visited at most once per
    /// call tree (tracked via `visited_fragments`).  Sets `*any_fragment_applied`
    /// to `true` if at least one fragment whose type condition was satisfied was
    /// encountered.
    fn collect_fields<'a>(
        &'a self,
        schema: &ApiSchema,
        object_type: &str,
        selection_set: impl IntoIterator<Item = &'a Selection>,
        variables: &Object,
        visited_fragments: &mut HashSet<&'a str>,
        grouped_fields: &mut IndexMap<&'a ByteString, Vec<&'a Selection>>,
        any_fragment_applied: &mut bool,
    ) {
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    alias,
                    include_skip,
                    ..
                } => {
                    if include_skip.should_skip(variables) {
                        continue;
                    }
                    let response_key = alias.as_ref().unwrap_or(name);
                    grouped_fields
                        .entry(response_key)
                        .or_default()
                        .push(selection);
                }
                Selection::InlineFragment {
                    type_condition,
                    selection_set: frag_ss,
                    include_skip,
                    ..
                } => {
                    if include_skip.should_skip(variables) {
                        continue;
                    }
                    if Self::does_fragment_type_apply(schema, object_type, type_condition) {
                        *any_fragment_applied = true;
                        self.collect_fields(
                            schema,
                            object_type,
                            frag_ss.iter(),
                            variables,
                            visited_fragments,
                            grouped_fields,
                            any_fragment_applied,
                        );
                    }
                }
                Selection::FragmentSpread {
                    name, include_skip, ..
                } => {
                    if include_skip.should_skip(variables) {
                        continue;
                    }
                    // Each named fragment is visited at most once (spec requirement).
                    if !visited_fragments.insert(name.as_str()) {
                        continue;
                    }
                    if let Some(Fragment {
                        type_condition,
                        selection_set: frag_ss,
                    }) = self.fragments.get(name)
                    {
                        if Self::does_fragment_type_apply(schema, object_type, type_condition) {
                            *any_fragment_applied = true;
                            self.collect_fields(
                                schema,
                                object_type,
                                frag_ss.iter(),
                                variables,
                                visited_fragments,
                                grouped_fields,
                                any_fragment_applied,
                            );
                        }
                    } else {
                        // the fragment should have been already checked with the schema
                        failfast_debug!("missing fragment named: {}", name);
                    }
                }
            }
        }
    }

    /// <https://spec.graphql.org/October2021/#DoesFragmentTypeApply()>
    fn does_fragment_type_apply(
        schema: &ApiSchema,
        object_type: &str,
        fragment_type: &str,
    ) -> bool {
        object_type == fragment_type || schema.is_subtype(fragment_type, object_type)
    }

    /// <https://spec.graphql.org/October2021/#ExecuteSelectionSet()>
    ///
    /// Groups `selections` by response key via `collect_fields`, then calls
    /// `apply_field` once per response key.  Processing each key exactly once
    /// prevents double-processing bugs such as ROUTER-1598 (null propagation
    /// bypassed by a later fragment).
    fn apply_selection_set<'a: 'b, 'b>(
        &'a self,
        selections: impl IntoIterator<Item = &'a Selection>,
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Vec<ResponsePathElement<'b>>,
        current_type: &executable::Type,
    ) -> Result<(), InvalidValue> {
        let current_type_name = current_type.inner_named_type().as_str();
        let mut visited_fragments: HashSet<&'a str> = HashSet::new();
        let mut grouped_fields: IndexMap<&'a ByteString, Vec<&'a Selection>> = IndexMap::default();
        let mut any_fragment_applied = false;

        self.collect_fields(
            parameters.schema,
            current_type_name,
            selections,
            parameters.variables,
            &mut visited_fragments,
            &mut grouped_fields,
            &mut any_fragment_applied,
        );

        // For filtered queries, when at least one fragment's type condition was
        // satisfied, preserve __typename so the original query can perform its own
        // type-condition checks.
        if !self.is_original && any_fragment_applied {
            if let Some(input_type) = input.get(TYPENAME) {
                output.insert(TYPENAME, input_type.clone());
            }
        }

        for (response_key, fields) in &grouped_fields {
            self.apply_field(
                response_key,
                fields,
                parameters,
                input,
                output,
                path,
                current_type,
            )?;
        }

        Ok(())
    }

    /// <https://spec.graphql.org/October2021/#ExecuteField()>
    ///
    /// Executes a single field given the group of field nodes that all share the
    /// same response key.  Handles `__typename` specially; for all other fields
    /// calls `complete_value`.
    fn apply_field<'a: 'b, 'b>(
        &'a self,
        response_key: &'a ByteString,
        fields: &[&'a Selection],
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Vec<ResponsePathElement<'b>>,
        current_type: &executable::Type,
    ) -> Result<(), InvalidValue> {
        // `field[0]` won't panic, since collect_fields only creates groups with at least one field.
        let Selection::Field {
            name, field_type, ..
        } = fields[0]
        else {
            // This won't happen, since collect_fields only ever pushes Field variants.
            failfast_debug!("non-Field variant in apply_field");
            return Err(InvalidValue);
        };

        if name.as_str() == TYPENAME {
            let object_type = parameters
                .schema
                .get_object(current_type.inner_named_type())
                .or_else(|| {
                    let input_value = input.get(response_key.as_str())?.as_str()?;
                    parameters.schema.get_object(input_value)
                });

            if let Some(object_type) = object_type {
                output.insert((*response_key).clone(), object_type.name.as_str().into());
            } else {
                return Err(InvalidValue);
            }
            return Ok(());
        }

        if let Some(input_value) = input.get_mut(response_key.as_str()) {
            let output_value = output.entry((*response_key).clone()).or_insert(Value::Null);
            path.push(ResponsePathElement::Key(response_key.as_str()));
            let res = self.complete_value(
                parameters,
                &field_type.0,
                fields,
                input_value,
                output_value,
                path,
            );
            path.pop();
            res?;
        } else {
            if !output.contains_key(response_key.as_str()) {
                output.insert((*response_key).clone(), Value::Null);
            }
            if field_type.is_non_null() {
                parameters.errors.push(
                    Error::builder()
                        .message(format!(
                            "Null value found for non-nullable type {}",
                            field_type.0.inner_named_type()
                        ))
                        .path(Path::from_response_slice(path))
                        .build(),
                );
                return Err(InvalidValue);
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
        let mut visited_fragments: HashSet<&'a str> = HashSet::new();
        let mut grouped_fields: IndexMap<&'a ByteString, Vec<&'a Selection>> = IndexMap::default();
        let mut any_fragment_applied = false;

        self.collect_fields(
            parameters.schema,
            root_type_name,
            selection_set.iter(),
            parameters.variables,
            &mut visited_fragments,
            &mut grouped_fields,
            &mut any_fragment_applied,
        );

        for (response_key, fields) in grouped_fields {
            let Selection::Field {
                name, field_type, ..
            } = fields[0]
            else {
                continue;
            };
            let response_key_str = response_key.as_str();

            if name.as_str() == TYPENAME {
                output.insert(
                    (*response_key).clone(),
                    Value::String(root_type_name.into()),
                );
                continue;
            }

            if let Some(input_value) = input.get_mut(response_key_str) {
                let output_value = output.entry((*response_key).clone()).or_insert(Value::Null);
                path.push(ResponsePathElement::Key(response_key_str));
                let res = self.complete_value(
                    parameters,
                    &field_type.0,
                    &fields,
                    input_value,
                    output_value,
                    path,
                );
                path.pop();
                res?;
            } else if field_type.is_non_null() {
                parameters.errors.push(
                    Error::builder()
                        .message(format!(
                            "Cannot return null for non-nullable field \
                             {root_type_name}.{response_key_str}"
                        ))
                        .path(Path::from_response_slice(path))
                        .build(),
                );
                return Err(InvalidValue);
            } else {
                output.insert((*response_key).clone(), Value::Null);
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
                .collect();
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
                        .or(default_value.as_ref());
                    let path = super::JsonValuePath::Variable {
                        name: name.as_str(),
                    };
                    ty.validate_input_value(value, schema, &path, strict_variable_validation)
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

    pub(crate) fn variable_value<'a>(
        &'a self,
        variable_name: &str,
        variables: &'a Object,
    ) -> Option<&'a Value> {
        variables
            .get(variable_name)
            .or_else(|| self.default_variable_value(variable_name))
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

    pub(crate) fn defer_variables_set(&self, variables: &Object) -> BooleanValues {
        let mut bits = 0_u32;
        for (i, variable) in self
            .defer_stats
            .conditional_defer_variable_names
            .iter()
            .enumerate()
        {
            let value = variables
                .get(variable.as_str())
                .or_else(|| self.default_variable_value(variable));

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
        executable::Value::Int(value) => Some(value.as_str().parse::<i64>().ok()?.into()),
        executable::Value::Float(value) => Some(value.try_to_f64().ok()?.into()),
        executable::Value::Null => Some(Value::Null),
        executable::Value::String(value) => Some(value.as_str().into()),
        executable::Value::Boolean(value) => Some((*value).into()),
        executable::Value::Enum(value) => Some(value.as_str().into()),
        executable::Value::List(value) => value.iter().map(|v| parse_hir_value(v)).collect(),
        executable::Value::Object(value) => value
            .iter()
            .map(|(k, v)| Some((k.as_str(), parse_hir_value(v)?)))
            .collect(),
    }
}

#[cfg(test)]
mod tests;
