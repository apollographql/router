//! Query processing.
//!
//! Parsing, formatting and manipulation of queries.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::executable;
use apollo_compiler::schema::ExtendedType;
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
use crate::spec::IncludeSkip;
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

    /// Pre-computed grouped fields, keyed by
    /// `outer: selection_set_ptr → inner: runtime_type_name → field_list`.
    ///
    /// Initialized lazily on the first call to `format_response` (which has access to
    /// the schema).  After initialization the map is read-only, so no synchronisation
    /// is needed at format time.
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    #[serde(skip)]
    pub(crate) grouped_fields_cache:
        OnceLock<HashMap<usize, HashMap<String, Arc<[CachedGroupedField]>>>>,
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
            grouped_fields_cache: OnceLock::new(),
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

        // Initialize the grouped-fields cache on first use.
        self.init_grouped_fields_cache(schema);

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
            grouped_fields_cache: OnceLock::new(),
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

    fn format_value<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        input: &mut Value,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
        // for every type, if we have an invalid value, we will replace it with null
        // and return Ok(()), because values are optional by default
        match field_type {
            executable::Type::Named(name) => match name.as_str() {
                "Int" => self.format_integer(parameters, path, input, output),
                "Float" => self.format_float(parameters, path, input, output),
                "Boolean" => self.format_boolean(parameters, path, input, output),
                "String" => self.format_string(parameters, path, input, output),
                "Id" => self.format_id(parameters, path, input, output),
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

    #[inline]
    fn format_non_nullable_value<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        input: &mut Value,
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

        self.format_value(parameters, &inner_type, input, output, path, selection_set)?;

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

            Err(InvalidValue)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn format_list<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        input: &mut Value,
        inner_type: &executable::Type,
        output: &mut Value,
        path: &mut Vec<ResponsePathElement<'b>>,
        selection_set: &'a [Selection],
    ) -> Result<(), InvalidValue> {
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
                    self.format_value(
                        parameters,
                        inner_type,
                        element,
                        &mut output_array[i],
                        path,
                        selection_set,
                    )?;
                    path.pop();
                    Ok(())
                })
        {
            // We pop here because, if an error is found, the path still contains the index of the
            // invalid value.
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
        Ok(())
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn format_named_type<'a: 'b, 'b>(
        &'a self,
        parameters: &mut FormatParameters,
        field_type: &executable::Type,
        input: &mut Value,
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
                    .as_str()
                    .filter(|s| enum_type.values.contains_key(*s))
                    .map(|_| input.clone())
                    .unwrap_or_default();
                return Ok(());
            }
            _ => {}
        }

        if let Value::Object(input_object) = input {
            if let Some(input_type) = input_object.get(TYPENAME).and_then(|val| val.as_str()) {
                // If there is a __typename, make sure the pointed type is a valid type of the
                // schema. Otherwise, something is wrong, and in case we might be inadvertently
                // leaking some data for an @inacessible type or something, nullify the whole
                // object. However, do note that due to `@interfaceObject`, some subgraph can have
                // returned a __typename that is the name of an interface in the supergraph, and
                // this is fine (that is, we should not return such a __typename to the user, but
                // as long as it's not returned, having it in the internal data is ok and sometimes
                // expected).
                let Some(ExtendedType::Object(_) | ExtendedType::Interface(_)) =
                    parameters.schema.types.get(input_type)
                else {
                    parameters.nullified.push(Path::from_response_slice(path));
                    *output = Value::Null;
                    return Ok(());
                };
            }

            if output.is_null() {
                *output = Value::Object(Object::with_capacity(selection_set.len()));
            }
            let output_object = output.as_object_mut().ok_or(InvalidValue)?;

            let typename = input_object
                .get(TYPENAME)
                .and_then(|val| val.as_str())
                .and_then(|s| apollo_compiler::ast::NamedType::new(s).ok())
                .map(apollo_compiler::ast::Type::Named);

            let current_type = match parameters.schema.types.get(field_type.inner_named_type()) {
                Some(ExtendedType::Interface(..) | ExtendedType::Union(..)) => {
                    typename.as_ref().unwrap_or(field_type)
                }
                _ => field_type,
            };

            if self
                .apply_selection_set(
                    selection_set,
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

    /// Initialise the grouped-fields cache on first use.
    ///
    /// Called at the start of every `format_response` invocation.  After the first
    /// call the `OnceLock` is populated and subsequent calls are a no-op (a single
    /// atomic load).
    fn init_grouped_fields_cache(&self, schema: &ApiSchema) {
        // Capture borrows of the individual fields we need so the closure does not
        // capture `self` as a whole (which would conflict with the `&self.grouped_fields_cache`
        // borrow that `OnceLock::get_or_init` holds).
        let operation = &self.operation;
        let fragments = &self.fragments;
        let subselections = &self.subselections;

        self.grouped_fields_cache.get_or_init(|| {
            build_grouped_fields_cache(operation, fragments, subselections, schema)
        });
    }

    fn apply_selection_set<'a: 'b, 'b>(
        &'a self,
        selection_set: &'a [Selection],
        parameters: &mut FormatParameters,
        input: &mut Object,
        output: &mut Object,
        path: &mut Vec<ResponsePathElement<'b>>,
        // the type under which we apply selections
        current_type: &executable::Type,
    ) -> Result<(), InvalidValue> {
        // Determine the runtime type from __typename, falling back to the declared type
        // when __typename is absent (e.g. for concrete object types).
        let runtime_type = input
            .get(TYPENAME)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| current_type.inner_named_type().as_str());

        // For filtered (non-original) queries, propagate __typename into the output so
        // downstream processing retains type information.
        if !self.is_original {
            if let Some(input_type) = input.get(TYPENAME) {
                output.insert(TYPENAME, input_type.clone());
            }
        }

        // Look up the pre-computed field list for (selection_set_ptr, runtime_type).
        // The `'a` lifetime flows: &'a self → &'a OnceLock → &'a HashMap → &'a Arc →
        // &'a [CachedGroupedField], so all borrows below are valid for 'a ≥ 'b.
        let ptr = selection_set.as_ptr() as usize;
        let fields: &'a [CachedGroupedField] = self
            .grouped_fields_cache
            .get()
            .and_then(|cache| cache.get(&ptr))
            .and_then(|inner| inner.get(runtime_type))
            .map(|arc| arc.as_ref())
            .unwrap_or(&[]);

        for field in fields {
            if field.should_skip(parameters.variables) {
                continue;
            }

            let response_key = &field.response_key;
            let response_key_str = response_key.as_str();

            if field.name.as_str() == TYPENAME {
                let object_type = parameters
                    .schema
                    .get_object(current_type.inner_named_type())
                    .or_else(|| {
                        let input_value = input.get(response_key_str)?.as_str()?;
                        parameters.schema.get_object(input_value)
                    });

                if let Some(object_type) = object_type {
                    output.insert(response_key.clone(), object_type.name.as_str().into());
                } else {
                    return Err(InvalidValue);
                }
                continue;
            }

            if let Some(input_value) = input.get_mut(response_key_str) {
                // ROUTER-1598 guard: a later fragment spread must not overwrite a null
                // that was correctly propagated by an earlier fragment's non-null
                // violation.  If the input is already null and the output already has
                // a value for this key, skip.
                if input_value.is_null() && output.contains_key(response_key_str) {
                    continue;
                }

                let output_value = output.entry(response_key.clone()).or_insert(Value::Null);

                path.push(ResponsePathElement::Key(response_key_str));
                let res = self.format_value(
                    parameters,
                    &field.field_type.0,
                    input_value,
                    output_value,
                    path,
                    &field.sub_selections,
                );
                path.pop();
                res?
            } else {
                if !output.contains_key(response_key_str) {
                    output.insert(response_key.clone(), Value::Null);
                }
                if field.field_type.is_non_null() {
                    parameters.errors.push(
                        Error::builder()
                            .message(format!(
                                "Null value found for non-nullable type {}",
                                field.field_type.0.inner_named_type()
                            ))
                            .path(Path::from_response_slice(path))
                            .build(),
                    );

                    return Err(InvalidValue);
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
        let ptr = selection_set.as_ptr() as usize;
        let fields: &'a [CachedGroupedField] = self
            .grouped_fields_cache
            .get()
            .and_then(|cache| cache.get(&ptr))
            .and_then(|inner| inner.get(root_type_name))
            .map(|arc| arc.as_ref())
            .unwrap_or(&[]);

        for field in fields {
            if field.should_skip(parameters.variables) {
                continue;
            }

            let field_name_str = field.response_key.as_str();

            if field.name.as_str() == TYPENAME {
                if !output.contains_key(field_name_str) {
                    output.insert(
                        field.response_key.clone(),
                        Value::String(root_type_name.into()),
                    );
                }
                continue;
            }

            if let Some(input_value) = input.get_mut(field_name_str) {
                // Same ROUTER-1598 guard as apply_selection_set.
                if input_value.is_null() && output.contains_key(field_name_str) {
                    continue;
                }

                let output_value = output
                    .entry(field.response_key.clone())
                    .or_insert(Value::Null);
                path.push(ResponsePathElement::Key(field_name_str));
                let res = self.format_value(
                    parameters,
                    &field.field_type.0,
                    input_value,
                    output_value,
                    path,
                    &field.sub_selections,
                );
                path.pop();
                res?
            } else if field.field_type.is_non_null() {
                parameters.errors.push(
                    Error::builder()
                        .message(format!(
                            "Cannot return null for non-nullable field {root_type_name}.{field_name_str}"
                        ))
                        .path(Path::from_response_slice(path))
                        .build(),
                );
                return Err(InvalidValue);
            } else {
                output.insert(field.response_key.clone(), Value::Null);
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

// ---------------------------------------------------------------------------
// Pre-computed grouped fields cache
// ---------------------------------------------------------------------------

/// A pre-resolved field entry for a specific (selection_set_ptr, runtime_type) pair.
///
/// Produced by `build_grouped_fields_cache` at query-initialization time.  At
/// response-format time, iterating cached entries replaces the per-object fragment
/// traversal and type-condition evaluation that were the performance bottleneck in
/// the spec-correct spike (`worktree-proper-refactor`).
#[derive(Debug)]
pub(crate) struct CachedGroupedField {
    /// Response key — the alias if present, otherwise the field name.
    response_key: ByteString,
    /// Field name (used for `__typename` detection at format time).
    name: ByteString,
    /// GraphQL type of this field.
    field_type: FieldType,
    /// All skip conditions for this field occurrence.
    ///
    /// Index 0 (if present) is the field's own `@skip`/`@include`; subsequent entries
    /// come from enclosing conditional fragment spreads.  An empty `Vec` means "always
    /// include" (the common fast-path — avoids any allocation or iteration).
    /// The field is skipped when *any* condition says skip.
    skip_conditions: Vec<IncludeSkip>,
    /// Sub-selections for this field occurrence.  Empty for scalar/enum fields.
    ///
    /// Cloned from the query document once during cache construction — no further
    /// clones are needed at response-format time.
    sub_selections: Arc<[Selection]>,
}

impl CachedGroupedField {
    /// Returns `true` when this field should be skipped given the current variables.
    #[inline]
    fn should_skip(&self, variables: &Object) -> bool {
        self.skip_conditions.iter().any(|c| c.should_skip(variables))
    }
}

/// Build the grouped-fields cache for a complete query document.
///
/// Returns a two-level map: outer key = selection-set identity (pointer to first
/// `Selection` as `usize`), inner key = concrete runtime type name, value = flat
/// list of `CachedGroupedField` in DFS order (fragments resolved, type conditions
/// evaluated).
fn build_grouped_fields_cache(
    operation: &Operation,
    fragments: &Fragments,
    subselections: &HashMap<SubSelectionKey, SubSelectionValue>,
    schema: &ApiSchema,
) -> HashMap<usize, HashMap<String, Arc<[CachedGroupedField]>>> {
    let mut cache: HashMap<usize, HashMap<String, Arc<[CachedGroupedField]>>> = HashMap::new();

    // Work queue: each entry is (ptr, len, declared_type_name) — the pointer and length
    // together identify the selection-set slice; the type name is used to enumerate
    // concrete runtime types.
    let mut pending: std::collections::VecDeque<(usize, usize, String)> =
        std::collections::VecDeque::new();

    // Seed with the root operation selection set.
    pending.push_back((
        operation.selection_set.as_ptr() as usize,
        operation.selection_set.len(),
        operation.type_name.clone(),
    ));

    // Seed with all defer subselections.
    for subsel in subselections.values() {
        if !subsel.selection_set.is_empty() {
            pending.push_back((
                subsel.selection_set.as_ptr() as usize,
                subsel.selection_set.len(),
                subsel.type_name.clone(),
            ));
        }
    }

    while let Some((ptr, len, type_name)) = pending.pop_front() {
        // Enumerate all concrete runtime types for this declared type.
        let runtime_types = get_concrete_runtime_types(&type_name, schema);

        for runtime_type in runtime_types {
            // Check / insert into the inner map — skip if already computed.
            let inner = cache.entry(ptr).or_default();
            if inner.contains_key(&runtime_type) {
                continue;
            }
            // Insert a placeholder to prevent cycles (a type referencing itself).
            inner.insert(runtime_type.clone(), Arc::from([]));

            // SAFETY: `ptr` and `len` were captured from a `Vec<Selection>` that
            // lives either in `operation.selection_set`, in a subselection, or in a
            // previously-built `Arc<[Selection]>` that is already stored in `cache`.
            // All of these allocations remain valid for the lifetime of the `Query`.
            let selection_set: &[Selection] =
                unsafe { std::slice::from_raw_parts(ptr as *const Selection, len) };

            // Build the flat field list for this (selection_set, runtime_type) pair.
            let fields =
                collect_fields_for_cache(selection_set, &runtime_type, fragments, schema, &[]);

            // Enqueue sub-selections for processing.
            for field in &fields {
                let sub_ptr = field.sub_selections.as_ptr() as usize;
                let sub_len = field.sub_selections.len();
                if sub_len > 0 {
                    let sub_type = field.field_type.0.inner_named_type().as_str().to_owned();
                    pending.push_back((sub_ptr, sub_len, sub_type));
                }
            }

            // Store the actual (non-placeholder) entry.
            cache
                .entry(ptr)
                .or_default()
                .insert(runtime_type, Arc::from(fields.into_boxed_slice()));
        }
    }

    cache
}

/// Enumerate all concrete runtime type names that could appear for `type_name`.
///
/// For concrete object types, returns just the type itself.  For abstract types
/// (interface / union), returns the type itself (as a fallback when `__typename` is
/// absent) plus all concrete object types that implement or are members of the
/// abstract type.
fn get_concrete_runtime_types(type_name: &str, schema: &ApiSchema) -> Vec<String> {
    match schema.types.get(type_name) {
        Some(ExtendedType::Object(_)) => vec![type_name.to_owned()],
        Some(ExtendedType::Interface(_) | ExtendedType::Union(_)) => {
            // Include the abstract type itself as a fallback key for when `__typename`
            // is absent, plus all concrete implementors / union members.
            let mut types = vec![type_name.to_owned()];
            types.extend(
                schema
                    .types
                    .iter()
                    .filter(|(_, ty)| matches!(ty, ExtendedType::Object(_)))
                    .filter(|(name, _)| schema.is_subtype(type_name, name.as_str()))
                    .map(|(name, _)| name.as_str().to_owned()),
            );
            types
        }
        _ => vec![type_name.to_owned()],
    }
}

/// Build the flat `CachedGroupedField` list for a single (selection_set, runtime_type) pair.
///
/// This runs a depth-first traversal of `selection_set`, resolving all inline
/// fragments and named fragment spreads using type-condition filtering against
/// `runtime_type`.  The result is the same sequence of fields that the current
/// `apply_selection_set` would visit, but computed once at query-initialisation
/// time rather than on every response object.
fn collect_fields_for_cache(
    selection_set: &[Selection],
    runtime_type: &str,
    fragments: &Fragments,
    schema: &ApiSchema,
    parent_conditions: &[IncludeSkip],
) -> Vec<CachedGroupedField> {
    let mut result = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    collect_fields_for_cache_inner(
        selection_set,
        runtime_type,
        fragments,
        schema,
        parent_conditions,
        &mut visited,
        &mut result,
    );
    result
}

fn collect_fields_for_cache_inner(
    selection_set: &[Selection],
    runtime_type: &str,
    fragments: &Fragments,
    schema: &ApiSchema,
    parent_conditions: &[IncludeSkip],
    visited: &mut HashSet<String>,
    result: &mut Vec<CachedGroupedField>,
) {
    for selection in selection_set {
        match selection {
            Selection::Field {
                name,
                alias,
                selection_set,
                field_type,
                include_skip,
            } => {
                // Build the effective skip conditions for this field occurrence.
                // Start with the field's own condition (if it is not the trivial
                // "always include" default), then append any inherited parent conditions.
                let default_is = IncludeSkip::default();
                let mut skip_conditions: Vec<IncludeSkip> = Vec::new();
                if include_skip != &default_is {
                    skip_conditions.push(include_skip.clone());
                }
                for parent in parent_conditions {
                    if parent != &default_is {
                        skip_conditions.push(parent.clone());
                    }
                }

                // Clone sub-selections once.  An empty Arc is used for scalar fields.
                let sub_sel: Arc<[Selection]> = match selection_set.as_deref() {
                    Some(ss) if !ss.is_empty() => Arc::from(ss.to_vec().into_boxed_slice()),
                    _ => Arc::from([]),
                };

                result.push(CachedGroupedField {
                    response_key: alias.as_ref().unwrap_or(name).clone(),
                    name: name.clone(),
                    field_type: field_type.clone(),
                    skip_conditions,
                    sub_selections: sub_sel,
                });
            }

            Selection::InlineFragment {
                type_condition,
                selection_set,
                include_skip,
                ..
            } => {
                // Apply type condition — only descend if the runtime type matches.
                if !does_fragment_type_apply(runtime_type, type_condition, schema) {
                    continue;
                }

                // Accumulate the spread's own condition into the parent conditions.
                let default_is = IncludeSkip::default();
                let new_parent: Vec<IncludeSkip> = if include_skip != &default_is {
                    let mut v = parent_conditions.to_vec();
                    v.push(include_skip.clone());
                    v
                } else {
                    parent_conditions.to_vec()
                };

                collect_fields_for_cache_inner(
                    selection_set,
                    runtime_type,
                    fragments,
                    schema,
                    &new_parent,
                    visited,
                    result,
                );
            }

            Selection::FragmentSpread {
                name,
                include_skip,
                ..
            } => {
                // Cycle guard: named fragments may reference each other.
                if !visited.insert(name.clone()) {
                    continue;
                }

                let Some(Fragment {
                    type_condition,
                    selection_set,
                }) = fragments.get(name)
                else {
                    continue;
                };

                if !does_fragment_type_apply(runtime_type, type_condition, schema) {
                    continue;
                }

                let default_is = IncludeSkip::default();
                let new_parent: Vec<IncludeSkip> = if include_skip != &default_is {
                    let mut v = parent_conditions.to_vec();
                    v.push(include_skip.clone());
                    v
                } else {
                    parent_conditions.to_vec()
                };

                collect_fields_for_cache_inner(
                    selection_set,
                    runtime_type,
                    fragments,
                    schema,
                    &new_parent,
                    visited,
                    result,
                );
            }
        }
    }
}

/// Spec §6.3.2 DoesFragmentTypeApply.
///
/// Returns `true` when the runtime concrete type satisfies the fragment's type
/// condition.  Used both during cache construction (to decide which fragments apply)
/// and during `apply_selection_set` formatting (for the `is_original = false` guard).
fn does_fragment_type_apply(
    runtime_type: &str,
    fragment_type: &str,
    schema: &ApiSchema,
) -> bool {
    if runtime_type == fragment_type {
        return true;
    }
    // is_subtype(abstract, candidate) → "does candidate belong to abstract?"
    schema.is_subtype(fragment_type, runtime_type)
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
