use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::schema::Type;
use encoding_rs::Encoding;
use encoding_rs::UTF_8;
use http::HeaderMap;
use http::HeaderValue;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::response::Parts;
use itertools::Itertools;
use mime::Mime;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use crate::connectors::ApplyToError;
use crate::connectors::ApplyToErrorKind;
use crate::connectors::ConnectSpec;
use crate::connectors::Connector;
use crate::connectors::JSONSelection;
use crate::connectors::ProblemLocation;
use crate::connectors::runtime::errors::RuntimeError;
use crate::connectors::runtime::inputs::ContextReader;
use crate::connectors::runtime::key::ResponseKey;
use crate::connectors::runtime::mapping::Problem;
use crate::connectors::runtime::mapping::aggregate_apply_to_errors;
use crate::connectors::runtime::responses::DeserializeError::ContentDecoding;

const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

#[derive(Debug, thiserror::Error)]
pub enum HandleResponseError {
    #[error("Merge error: {0}")]
    MergeError(String),
}

/// Converts a response body into a json Value based on the Content-Type header.
pub fn deserialize_response(body: &[u8], headers: &HeaderMap) -> Result<Value, DeserializeError> {
    // If the body is empty, there's nothing to parse. We check body.is_empty()
    // directly because spec-compliant HTTP 204 responses must not include a
    // Content-Length header — so we can't rely on that header alone to detect
    // empty bodies. The Content-Length: 0 check is kept for non-compliant
    // servers that do send it, but body.is_empty() covers both cases.
    if body.is_empty()
        || headers
            .get(CONTENT_LENGTH)
            .and_then(|len| len.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|content_length| content_length == 0)
    {
        return Ok(Value::Null);
    }

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok()?.parse::<Mime>().ok());

    if content_type.is_none()
        || content_type
            .as_ref()
            .is_some_and(|ct| ct.subtype() == mime::JSON || ct.suffix() == Some(mime::JSON))
    {
        // Treat any JSON-y like content types as JSON
        // Also, because the HTTP spec says we should effectively "guess" the content type if there is no content type (None), we're going to guess it is JSON if the server has not specified one
        serde_json::from_slice::<Value>(body).map_err(DeserializeError::SerdeJson)
    } else if content_type
        .as_ref()
        .is_some_and(|ct| ct.type_() == mime::TEXT && ct.subtype() == mime::PLAIN)
    {
        // Plain text we can't parse as JSON so we'll instead return it as a JSON string
        // Before we can do that, we need to figure out the charset and attempt to decode the string
        let encoding = content_type
            .as_ref()
            .and_then(|ct| Encoding::for_label(ct.get_param("charset")?.as_str().as_bytes()))
            .unwrap_or(UTF_8);
        let (decoded_body, _, had_errors) = encoding.decode(body);

        if had_errors {
            return Err(ContentDecoding(encoding.name()));
        }

        Ok(Value::String(decoded_body.into_owned().into()))
    } else {
        // For any other content types, all we can do is treat it as a JSON null cause we don't know what it is
        Ok(Value::Null)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeserializeError {
    #[error("Could not parse JSON: {0}")]
    SerdeJson(#[source] serde_json::Error),
    #[error("Could not decode data with content encoding {0}")]
    ContentDecoding(&'static str),
}

pub fn handle_raw_response(
    data: &Value,
    parts: &Parts,
    key: ResponseKey,
    connector: &Connector,
    context: impl ContextReader,
    client_headers: &HeaderMap<HeaderValue>,
) -> MappedResponse {
    let inputs = key
        .inputs()
        .clone()
        .merger(&connector.response_variable_keys)
        .config(connector.config.as_ref())
        .context(context)
        .status(parts.status.as_u16())
        .request(&connector.response_headers, client_headers)
        .response(&connector.response_headers, Some(parts))
        .merge();
    let warnings = Vec::new();
    let (success, warnings) = is_success(
        connector.error_settings.connect_is_success.as_ref(),
        data,
        parts,
        &inputs,
        warnings,
    );
    if success {
        let response = map_response(connector, data, key, inputs, warnings);
        check_response_shape(connector, response)
    } else {
        map_error(connector, data, parts, key, inputs, warnings)
    }
}

/// The error code used for [`check_response_shape`] errors, distinguishing a
/// response-shape violation from an ordinary fetch failure (`CONNECTORS_FETCH`).
const RESPONSE_SHAPE_ERROR_CODE: &str = "CONNECTORS_RESPONSE_SHAPE";

/// Returns an error when the mapped response value doesn't match the shape
/// the schema declares for this field: arrayness (`[T]` vs `T`) or
/// non-null (`T!` receiving `null`). Without this check, a shape mismatch is
/// silently nullified by the GraphQL runtime, and a `null` for a non-null
/// field is only reported via the `extensions.valueCompletion` side channel
/// rather than a top-level error — both making the underlying bug nearly
/// impossible to diagnose. This provides an actionable error instead.
///
/// `output_type` is `None` for type-level connectors (e.g. `BatchEntity`,
/// which always returns an array by design and has its own validation in
/// `add_to_data`), since they have no field definition to derive shape from;
/// the check is skipped in that case.
///
/// Only runs for connect/v0.5+ connectors: this changes visible behavior (a
/// case that used to return null now returns an error), so v0.4-and-earlier
/// connectors are unaffected when they upgrade.
fn check_response_shape(connector: &Connector, response: MappedResponse) -> MappedResponse {
    let MappedResponse::Data {
        data,
        key,
        problems,
        errors,
    } = response
    else {
        return response;
    };

    if connector.spec < ConnectSpec::V0_5 {
        return MappedResponse::Data {
            data,
            key,
            problems,
            errors,
        };
    }

    let is_array = matches!(data, Value::Array(_));
    let is_null = matches!(data, Value::Null);
    let output_is_list = connector.output_type.as_ref().map(Type::is_list);
    let output_is_non_null = connector.output_type.as_ref().map(Type::is_non_null);

    let message = if is_null {
        (output_is_non_null == Some(true)).then(|| {
            "Response was null. The schema declares this field non-nullable, \
             but the connector returned no data."
                .to_string()
        })
    } else if output_is_list == Some(true) && !is_array {
        Some(
            "Response was not a list. The schema expects a list for this field, \
             but the connector returned a single object. \
             Check that the API returns an array."
                .to_string(),
        )
    } else if output_is_list == Some(false) && is_array {
        Some(
            "Response was a list. The schema expects a single object for this field, \
             but the connector returned an array."
                .to_string(),
        )
    } else {
        None
    };

    if let Some(message) = message {
        let mut error = RuntimeError::new(message, &key).with_code(RESPONSE_SHAPE_ERROR_CODE);
        error.subgraph_name = Some(connector.id.subgraph_name.clone());
        error.coordinate = Some(connector.id.coordinate());

        // The field is failing, so its declared errors cannot travel with it:
        // they were written to accompany data that is not being returned, and
        // `MappedResponse::Error` reports the one error that explains the
        // failure. They are still true statements about the response body
        // though, and a mapping author debugging a shape violation wants to
        // see them, so they are demoted to problems rather than dropped.
        let mut problems = problems;
        problems.extend(errors.into_iter().map(|declared| Problem {
            message: format!(
                "Declared error not reported to the client because the response \
                 failed its shape check: {}",
                declared.message
            ),
            path: declared.path,
            count: 1,
            location: ProblemLocation::Selection,
        }));

        MappedResponse::Error {
            error,
            key,
            problems,
        }
    } else {
        MappedResponse::Data {
            data,
            key,
            problems,
            errors,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GraphQLDataMapper<'a> {
    doc: &'a ExecutableDocument,
    subtypes_map: &'a IndexMap<String, IndexSet<String>>,
}

impl<'a> GraphQLDataMapper<'a> {
    fn new(
        doc: &'a ExecutableDocument,
        subtypes_map: &'a IndexMap<String, IndexSet<String>>,
    ) -> Self {
        Self { doc, subtypes_map }
    }

    fn fragment_matches(&self, data: &Value, fragment_type_condition: &Name) -> bool {
        if let Some(data_typename) = data.get("__typename") {
            match data_typename {
                Value::String(typename) => {
                    self.supertype_has_subtype(fragment_type_condition.as_str(), typename.as_str())
                }
                _ => false,
            }
        } else {
            true
        }
    }

    fn supertype_has_subtype(&self, supertype: &str, subtype: &str) -> bool {
        if supertype == subtype {
            true
        } else if let Some(subtypes) = self.subtypes_map.get(supertype) {
            subtypes
                .iter()
                .any(|s| self.supertype_has_subtype(s, subtype))
        } else {
            false
        }
    }

    fn map_data(&self, data: &Value, selection_set: &SelectionSet) -> Value {
        if selection_set.selections.is_empty() {
            return data.clone();
        }

        match data {
            Value::Object(map) => {
                let mut new_map = Map::new();

                for field in selection_set.selections.iter() {
                    match field {
                        Selection::Field(field) => {
                            if let Some(field_value) = map.get(field.name.as_str()) {
                                let output_field_name = field.alias.as_ref().unwrap_or(&field.name);
                                new_map.insert(
                                    output_field_name.to_string(),
                                    self.map_data(field_value, &field.selection_set),
                                );
                            } else if field.name == TYPENAME {
                                // __typename is an intrinsic field that always
                                // resolves to the concrete type name, even when
                                // the connector response doesn't include it
                                // (e.g., mappingOnly connectors returning `{}`).
                                let output_field_name = field.alias.as_ref().unwrap_or(&field.name);
                                new_map.insert(
                                    output_field_name.to_string(),
                                    Value::String(selection_set.ty.to_string().into()),
                                );
                            }
                        }

                        Selection::FragmentSpread(spread) => {
                            if let Some(fragment) =
                                self.doc.fragments.get(spread.fragment_name.as_str())
                                && self.fragment_matches(data, fragment.type_condition())
                            {
                                let mapped = self.map_data(data, &fragment.selection_set);
                                if let Some(fragment_map) = mapped.as_object() {
                                    new_map.extend(fragment_map.clone());
                                }
                            }
                        }

                        Selection::InlineFragment(fragment) => {
                            if let Some(type_condition) = &fragment.type_condition
                                && !self.fragment_matches(data, type_condition)
                            {
                                continue;
                            }
                            let mapped = self.map_data(data, &fragment.selection_set);
                            if let Some(fragment_map) = mapped.as_object() {
                                new_map.extend(fragment_map.clone());
                            }
                        }
                    }
                }

                Value::Object(new_map)
            }

            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.map_data(item, selection_set))
                    .collect(),
            ),

            primitive => primitive.clone(),
        }
    }
}

// If the user has set a custom success condition selector, resolve that expression,
// otherwise default to checking status code is 2XX
fn is_success(
    is_success_selection: Option<&JSONSelection>,
    data: &Value,
    parts: &Parts,
    inputs: &IndexMap<String, Value>,
    mut warnings: Vec<Problem>,
) -> (bool, Vec<Problem>) {
    let Some(is_success_selection) = is_success_selection else {
        return (parts.status.is_success(), warnings);
    };
    let (res, apply_to_errors) = is_success_selection.apply_with_vars(data, inputs);
    warnings.extend(aggregate_apply_to_errors(
        apply_to_errors,
        ProblemLocation::IsSuccess,
    ));

    let type_name = match res.as_ref() {
        Some(Value::Bool(b)) => return (*b, warnings),
        None => return (false, warnings),
        Some(Value::Null) => "null",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    };
    warnings.push(Problem {
        message: format!("`isSuccess` must evaluate to a boolean, got {type_name}"),
        path: String::new(),
        count: 1,
        location: ProblemLocation::IsSuccess,
    });
    (false, warnings)
}

/// Returns a response for a mapping-only connector by applying the selection against `{}`.
///
/// Used when `http` is omitted from a `@connect` directive, skipping the HTTP transport.
pub fn handle_mapping_only_response(
    key: ResponseKey,
    connector: &Connector,
    context: impl ContextReader,
    client_headers: &HeaderMap<HeaderValue>,
) -> MappedResponse {
    let data = Value::Object(Map::new());
    let inputs = key
        .inputs()
        .clone()
        .merger(&connector.response_variable_keys)
        .config(connector.config.as_ref())
        .context(context)
        .request(&connector.response_headers, client_headers)
        .merge();
    map_response(connector, &data, key, inputs, Vec::new())
}

/// The mapping-side path an error was declared at, as a dotted string, for the
/// `connector.selectionPath` extension.
///
/// This is *not* a GraphQL response path, and the difference is the reason it
/// lives in an extension rather than in `path`. [`ApplyToError`] records an
/// [`InputPath`](crate::connectors::json_selection::immutable::InputPath):
/// where the mapping was reading in the *source* JSON, interleaved with
/// `->method` markers for the methods it passed through. In a mapping like
/// `balance: amount->withError(...)` that path says `amount` — the API's field
/// — while the response path is `balance`. The two coincide only when the
/// mapping happens to be a rename-free passthrough.
///
/// The `->method` markers are dropped: they describe the mapping's internals,
/// and the customer feedback's objection to a path reading `["->withError"]`
/// applies just as well here.
fn selection_path(error: &ApplyToError) -> String {
    error
        .path()
        .iter()
        .filter_map(|segment| match segment {
            Value::String(name) if name.as_str().starts_with("->") => None,
            Value::String(name) => Some(name.as_str().to_string()),
            Value::Number(index) => Some(index.to_string()),
            _ => None,
        })
        .join(".")
}

/// Turn an error the schema author declared with `->withError` into the
/// client-facing GraphQL error it was written to be.
///
/// The author's `extensions` are merged over the connector's defaults rather
/// than replacing them, matching what `map_error` already does for
/// `@connect(errors:)`, so `code` is the author's while `service` and
/// `connector.coordinate` still identify where the error came from.
///
/// # On `path`
///
/// The error's `path` is the connector's response path followed by
/// [`ApplyToError::output_path`] — where inside the connector's output the
/// error was declared, so the whole thing resolves against the data the client
/// received.
///
/// This deliberately does not use `ApplyToError::path`, which records where the
/// mapping was *reading* in the API's JSON. The two diverge under any rename:
/// `balance: amount->withError(...)` reads `amount` and writes `balance`, and
/// `acct: { bal: amount->... }` reads `amount` and writes `acct.bal`. The read
/// path is still useful for debugging a mapping, so it is preserved under
/// `extensions.connector.selectionPath`.
///
/// One gap remains: a client that *aliases* a field sees the alias in its
/// response, while `output_path` carries the schema field name, since aliases
/// are applied later in `apply_operation`. Renames inside the mapping — the
/// common case, and the one the feedback was about — are handled.
fn declared_error_to_runtime_error(
    error: &ApplyToError,
    key: &ResponseKey,
    connector: &Connector,
) -> RuntimeError {
    let mut runtime_error = RuntimeError::new(error.message(), key);
    runtime_error.subgraph_name = Some(connector.id.subgraph_name.clone());
    runtime_error.coordinate = Some(connector.id.coordinate());

    // Descend from the connector's response path into the mapping's output.
    let mut path = vec![key.path_string()];
    path.extend(
        error
            .output_path()
            .iter()
            .filter_map(|segment| match segment {
                Value::String(name) => Some(name.as_str().to_string()),
                Value::Number(index) => Some(index.to_string()),
                _ => None,
            }),
    );
    runtime_error.path = path.join("/");

    let selection_path = selection_path(error);
    if !selection_path.is_empty() {
        runtime_error = runtime_error.merge_extension(
            "connector",
            Value::Object(Map::from_iter([(
                "selectionPath".into(),
                Value::String(selection_path.into()),
            )])),
        );
    }

    let mut code = None;
    if let Some(Value::Object(extensions)) = error.extensions() {
        if let Some(Value::String(author_code)) = extensions.get("code") {
            code = Some(author_code.as_str().to_string());
        }
        for (name, value) in extensions {
            runtime_error = runtime_error.merge_extension(name.clone(), value.clone());
        }
    }

    runtime_error.with_code(code.unwrap_or_else(|| DECLARED_ERROR_CODE.to_string()))
}

/// The code a declared error carries when its author did not supply one.
/// Distinct from `CONNECTORS_FETCH` because nothing failed to fetch: the
/// request succeeded and the mapping author chose to report something about
/// its contents.
const DECLARED_ERROR_CODE: &str = "CONNECTORS_MAPPING_ERROR";

/// Returns a response with data transformed by the selection mapping.
pub(super) fn map_response(
    connector: &Connector,
    data: &Value,
    key: ResponseKey,
    inputs: IndexMap<String, Value>,
    mut warnings: Vec<Problem>,
) -> MappedResponse {
    let (res, apply_to_errors) = key.selection().apply_with_vars(data, &inputs);

    // Declared errors are the author's, addressed to the client; diagnostics
    // are the language's, addressed to the author. The split decides what
    // *additionally* travels to the client — it does not decide what reaches
    // the debugger, which is why `warnings` below still receives both kinds.
    // `->withError` shipped as a debugger and telemetry feature, and mapping
    // problems are what both of those read, so removing declared errors from
    // them would be a regression dressed up as a feature.
    //
    // Selecting rather than partitioning, for the same reason: the errors are
    // needed twice, in two different shapes. Client-facing errors are built
    // from the untouched `ApplyToError`s, because aggregation discards both
    // the structured extensions and the array indices they carry.
    //
    // Deliberately not gated on a spec version. No mapping method is: method
    // availability is decided by `ArrowMethod::is_public`, and every method's
    // *behavior* is version-invariant — the rstest cases across V0_2..V0_5 in
    // the methods directory exist to assert exactly that. Gating this would
    // make `->withError` mean two different things depending on a connector's
    // `@link` URL, for a method that has never shipped and so has no earlier
    // behavior to preserve. Writing `->withError` is itself the opt-in.
    let declared = apply_to_errors
        .iter()
        .filter(|error| error.kind() == ApplyToErrorKind::Declared)
        .cloned()
        .collect::<Vec<_>>();

    warnings.extend(aggregate_apply_to_errors(
        apply_to_errors,
        ProblemLocation::Selection,
    ));

    // Every declared error is reported. The count is deliberately not capped:
    // nothing else in the router truncates a response's errors (a subgraph
    // returning thousands has them all passed through), and the feature exists
    // so an author can record every defect they find — handing a client "and
    // 400 more" would defeat that. The element count is already bounded
    // upstream by the `http_max_response_size` connector limit, which is where
    // an operator worried about response size sets a policy.
    let errors = declared
        .iter()
        .map(|error| declared_error_to_runtime_error(error, &key, connector))
        .collect::<Vec<_>>();

    MappedResponse::Data {
        key,
        data: res.unwrap_or_else(|| Value::Null),
        problems: warnings,
        errors,
    }
}

/// Returns a `MappedResponse` with a GraphQL error.
pub(super) fn map_error(
    connector: &Connector,
    data: &Value,
    parts: &Parts,
    key: ResponseKey,
    inputs: IndexMap<String, Value>,
    mut warnings: Vec<Problem>,
) -> MappedResponse {
    // Do we have an error message mapping set for this connector?
    let message = if let Some(message_selection) = &connector.error_settings.message {
        let (res, apply_to_errors) = message_selection.apply_with_vars(data, &inputs);
        warnings.extend(aggregate_apply_to_errors(
            apply_to_errors,
            ProblemLocation::ErrorsMessage,
        ));
        res.as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        "Request failed".to_string()
    };

    // Now we can create the error object using either the default message or the message calculated by the JSONSelection
    let mut error = RuntimeError::new(message, &key);
    error.subgraph_name = Some(connector.id.subgraph_name.clone());
    error.coordinate = Some(connector.id.coordinate());

    // First, we will apply defaults... these may get overwritten below by user configured extensions
    error = error.extension(
        "http",
        Value::Object(Map::from_iter([(
            "status".into(),
            Value::Number(parts.status.as_u16().into()),
        )])),
    );

    // If we have error extensions mapping set for this connector, we will need to grab the code + the remaining extensions and map them to the error object
    // We'll merge by applying the source and then the connect. User-supplied extensions deep-merge with the existing values, so a default like
    // `http: { status }` is preserved when the user sets a sibling field like `http: { myField }` (the docs at
    // https://www.apollographql.com/docs/graphos/connectors/responses/error-handling promise that defaults are retained alongside user fields).
    // Note: that we set the extension code in this if/else but don't actually set it on the error until after the if/else. This is because the compiler
    // can't make sense of it in the if/else due to how the builder is constructed.
    let mut extension_code = "CONNECTOR_FETCH".to_string();
    if let Some(extensions_selection) = &connector.error_settings.source_extensions {
        let (res, apply_to_errors) = extensions_selection.apply_with_vars(data, &inputs);
        warnings.extend(aggregate_apply_to_errors(
            apply_to_errors,
            ProblemLocation::SourceErrorsExtensions,
        ));

        // TODO: Currently this "fails silently". In the future, we probably add a warning to the debugger info.
        let extensions = res
            .and_then(|e| match e {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();

        if let Some(code) = extensions.get("code") {
            extension_code = code.as_str().unwrap_or_default().to_string();
        }

        for (key, value) in extensions {
            error = error.merge_extension(key, value);
        }
    }

    if let Some(extensions_selection) = &connector.error_settings.connect_extensions {
        let (res, apply_to_errors) = extensions_selection.apply_with_vars(data, &inputs);
        warnings.extend(aggregate_apply_to_errors(
            apply_to_errors,
            ProblemLocation::ConnectErrorsExtensions,
        ));

        // TODO: Currently this "fails silently". In the future, we probably add a warning to the debugger info.
        let extensions = res
            .and_then(|e| match e {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();

        if let Some(code) = extensions.get("code") {
            extension_code = code.as_str().unwrap_or_default().to_string();
        }

        for (key, value) in extensions {
            error = error.merge_extension(key, value);
        }
    }

    error = error.with_code(extension_code);

    MappedResponse::Error {
        error,
        key,
        problems: warnings,
    }
}
// --- MAPPED RESPONSE ---------------------------------------------------------
#[derive(Debug)]
pub enum MappedResponse {
    /// This is equivalent to RawResponse::Error, but it also represents errors
    /// when the request is semantically unsuccessful (e.g. 404, 500).
    Error {
        error: RuntimeError,
        key: ResponseKey,
        problems: Vec<Problem>,
    },
    /// The response data after applying the selection mapping.
    Data {
        data: Value,
        key: ResponseKey,
        problems: Vec<Problem>,
        /// Errors the mapping author declared with `->withError`, to be added
        /// to the GraphQL response's `errors` array alongside this data.
        ///
        /// Distinct from `problems`, which never leave the router: these are
        /// addressed to the client, and the field resolves normally in spite
        /// of them — that combination is the whole point of `->withError`, and
        /// is why they cannot ride along in the `Error` variant instead.
        errors: Vec<RuntimeError>,
    },
}

impl MappedResponse {
    /// Adds the response data to the `data` map or the error to the `errors`
    /// array. How data is added depends on the `ResponseKey`: it's either a
    /// property directly on the map, or stored in the `_entities` array.
    pub fn add_to_data(
        self,
        data: &mut Map<ByteString, Value>,
        errors: &mut Vec<RuntimeError>,
        count: usize,
    ) -> Result<(), HandleResponseError> {
        match self {
            Self::Error { error, key, .. } => {
                match key {
                    // add a null to the "_entities" array at the right index
                    ResponseKey::Entity { index, .. } | ResponseKey::EntityField { index, .. } => {
                        let entities = data
                            .entry(ENTITIES)
                            .or_insert(Value::Array(Vec::with_capacity(count)));
                        entities
                            .as_array_mut()
                            .ok_or_else(|| {
                                HandleResponseError::MergeError("_entities is not an array".into())
                            })?
                            .insert(index, Value::Null);
                    }
                    _ => {}
                };
                errors.push(error);
            }
            Self::Data {
                data: value,
                key,
                errors: declared,
                ..
            } => {
                // The data is still added below: a declared error accompanies
                // its field rather than replacing it, which is the difference
                // between `->withError` and failing the field.
                errors.extend(declared);

                match key {
                    ResponseKey::RootField { ref name, .. } => {
                        data.insert(name.clone(), value);
                    }
                    ResponseKey::Entity { index, .. } => {
                        let entities = data
                            .entry(ENTITIES)
                            .or_insert(Value::Array(Vec::with_capacity(count)));
                        entities
                            .as_array_mut()
                            .ok_or_else(|| {
                                HandleResponseError::MergeError("_entities is not an array".into())
                            })?
                            .insert(index, value);
                    }
                    ResponseKey::EntityField {
                        index,
                        ref field_name,
                        ref typename,
                        ..
                    } => {
                        let entities = data
                            .entry(ENTITIES)
                            .or_insert(Value::Array(Vec::with_capacity(count)))
                            .as_array_mut()
                            .ok_or_else(|| {
                                HandleResponseError::MergeError("_entities is not an array".into())
                            })?;

                        match entities.get_mut(index) {
                            Some(Value::Object(entity)) => {
                                entity.insert(field_name.clone(), value);
                            }
                            _ => {
                                let mut entity = Map::new();
                                if let Some(typename) = typename {
                                    entity
                                        .insert(TYPENAME, Value::String(typename.as_str().into()));
                                }
                                entity.insert(field_name.clone(), value);
                                entities.insert(index, Value::Object(entity));
                            }
                        };
                    }
                    ResponseKey::BatchEntity {
                        selection,
                        keys,
                        inputs,
                    } => {
                        let Value::Array(values) = value else {
                            return Err(HandleResponseError::MergeError(
                                "Response for a batch request does not map to an array".into(),
                            ));
                        };

                        let spec = selection.spec();
                        let key_selection = JSONSelection::parse_with_spec(
                            &keys.serialize().no_indent().to_string(),
                            spec,
                        )
                        .map_err(|e| HandleResponseError::MergeError(e.to_string()))?;

                        // Convert representations into keys for use in the map
                        let key_values = inputs.batch.iter().map(|v| {
                            key_selection
                                .apply_to(&Value::Object(v.clone()))
                                .0
                                .unwrap_or(Value::Null)
                        });

                        // Create a map of keys to entities
                        let mut map = values
                            .into_iter()
                            .filter_map(|v| key_selection.apply_to(&v).0.map(|key| (key, v)))
                            .collect::<HashMap<_, _>>();

                        // Make a list of entities that matches the representations list
                        let new_entities = key_values
                            .map(|key| map.remove(&key).unwrap_or(Value::Null))
                            .collect_vec();

                        // Because we may have multiple batch entities requests, we should add to ENTITIES as the requests come in so it is additive
                        let entities = data
                            .entry(ENTITIES)
                            .or_insert(Value::Array(Vec::with_capacity(count)));

                        entities
                            .as_array_mut()
                            .ok_or_else(|| {
                                HandleResponseError::MergeError("_entities is not an array".into())
                            })?
                            .extend(new_entities);
                    }
                }
            }
        }

        Ok(())
    }

    pub fn problems(&self) -> &[Problem] {
        match self {
            Self::Error { problems, .. } | Self::Data { problems, .. } => problems,
        }
    }

    /// Applies the given GraphQL operation (note: must be a single operation!)
    /// to the [`MappedResponse`] to produce a new [`MappedResponse`] with
    /// GraphQL transforms like alias renaming applied.
    ///
    /// The `operation_option` parameter is an [`Option<&ExecutableDocument>`]
    /// to simplify cases where you might not have an [`ExecutableDocument`]
    /// available (hence `None`). When `operation_option.is_none()`, note that
    /// `subtypes` is ignored.
    ///
    /// The `subtypes` parameter is necessary for handling abstract fragment
    /// type conditions, since that information is not preserved in
    /// [`ExecutableDocument`].
    pub fn apply_operation(
        self, // NOTE: Takes ownership of self!
        operation_option: Option<&ExecutableDocument>,
        subtypes: &IndexMap<String, IndexSet<String>>,
    ) -> Self {
        match (self, operation_option) {
            (
                Self::Data {
                    data,
                    key,
                    problems,
                    errors,
                },
                Some(operation),
            ) => {
                let single_op = operation
                    .operations
                    .anonymous
                    .as_ref()
                    .or_else(|| operation.operations.named.values().next());

                let data = if let Some(op) = single_op {
                    let mut new_sub = SelectionSet::new(op.selection_set.ty.clone());

                    match &key {
                        ResponseKey::RootField { name, .. } => {
                            for field in op.selection_set.selections.iter() {
                                if let Selection::Field(field) = field
                                    && field.alias.as_deref().unwrap_or(field.name.as_str())
                                        == name.as_str()
                                {
                                    // Use the field's selection set type so that
                                    // __typename resolves to the return type (e.g.
                                    // "UserMutations") rather than the root operation
                                    // type (e.g. "Mutation").
                                    new_sub.ty = field.selection_set.ty.clone();
                                    new_sub
                                        .selections
                                        .extend(field.selection_set.selections.iter().cloned());
                                }
                            }
                        }

                        ResponseKey::EntityField { field_name, .. } => {
                            let field_str = field_name.as_str();

                            for selection in op.selection_set.selections.iter() {
                                if let Selection::Field(field) = selection
                                    && field.name.as_str() == "_entities"
                                {
                                    for ent_sel in field.selection_set.selections.iter() {
                                        // Selection::InlineFragment is what we
                                        // actually expect, but we could handle
                                        // ::Field and ::FragmentSpread too if
                                        // necessary.
                                        match ent_sel {
                                            Selection::InlineFragment(frag) => {
                                                for field_sel in
                                                    frag.selection_set.selections.iter()
                                                {
                                                    if let Selection::Field(field) = field_sel
                                                        && field.name.as_str() == field_str
                                                    {
                                                        new_sub.selections.extend(
                                                            field
                                                                .selection_set
                                                                .selections
                                                                .iter()
                                                                .cloned(),
                                                        );
                                                    }
                                                }
                                            }

                                            Selection::Field(field) => {
                                                if field.name.as_str() == field_str {
                                                    new_sub.selections.extend(
                                                        field
                                                            .selection_set
                                                            .selections
                                                            .iter()
                                                            .cloned(),
                                                    );
                                                }
                                            }

                                            Selection::FragmentSpread(spread) => {
                                                if let Some(fragment) = operation
                                                    .fragments
                                                    .get(spread.fragment_name.as_str())
                                                {
                                                    for field_sel in
                                                        fragment.selection_set.selections.iter()
                                                    {
                                                        if let Selection::Field(field) = field_sel
                                                            && field.name.as_str() == field_str
                                                        {
                                                            new_sub.selections.extend(
                                                                field
                                                                    .selection_set
                                                                    .selections
                                                                    .iter()
                                                                    .cloned(),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        ResponseKey::Entity { .. } => {
                            for selection in op.selection_set.selections.iter() {
                                if let Selection::Field(field) = selection
                                    && field.name.as_str() == "_entities"
                                {
                                    new_sub
                                        .selections
                                        .extend(field.selection_set.selections.iter().cloned());
                                }
                            }
                        }

                        ResponseKey::BatchEntity { keys, .. } => {
                            new_sub
                                .selections
                                .extend(keys.selection_set.selections.iter().cloned());

                            for selection in op.selection_set.selections.iter() {
                                if let Selection::Field(field) = selection
                                    && field.name.as_str() == "_entities"
                                {
                                    new_sub
                                        .selections
                                        .extend(field.selection_set.selections.iter().cloned());
                                }
                            }
                        }
                    };

                    GraphQLDataMapper::new(operation, subtypes).map_data(&data, &new_sub)
                } else {
                    data
                };

                Self::Data {
                    data,
                    key,
                    problems,
                    errors,
                }
            }

            // We do not transform errors using the operation.
            (
                MappedResponse::Error {
                    error,
                    key,
                    problems,
                },
                Some(_),
            ) => MappedResponse::Error {
                error,
                key,
                problems,
            },

            // When operation_option.is_none(), return self unmodified.
            (mapped, None) => mapped,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use apollo_compiler::collections::IndexMap;
    use apollo_compiler::schema::Type;
    use http::HeaderMap;
    use http::HeaderValue;
    use http::StatusCode;
    use http::response::Parts;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;

    use super::MappedResponse;
    use super::deserialize_response;
    use super::is_success;
    use super::map_response;
    use crate::connectors::ConnectSpec;
    use crate::connectors::JSONSelection;
    use crate::connectors::runtime::inputs::RequestInputs;
    use crate::connectors::runtime::key::ResponseKey;

    fn make_parts(status: u16) -> Parts {
        http::Response::builder()
            .status(StatusCode::from_u16(status).unwrap())
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    // Regression test for CNN-1022: when isSuccess evaluates to a non-boolean,
    // a problem must be surfaced so the debugger can explain the failure.
    #[test]
    fn is_success_non_boolean_emits_warning() {
        let selection = JSONSelection::parse("$.status").unwrap();
        let data = json!({"status": "ok"});
        let parts = make_parts(200);

        let (success, problems) =
            is_success(Some(&selection), &data, &parts, &Default::default(), vec![]);

        assert!(!success, "non-boolean isSuccess should fail the request");
        assert_eq!(problems.len(), 1, "expected one problem, got: {problems:?}");
        assert!(
            problems[0].message.contains("string"),
            "problem message should mention the actual type, got: {:?}",
            problems[0].message
        );
    }

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn empty_body_no_content_length_returns_null() {
        // Spec-compliant 204: no Content-Length header, no body.
        let headers = HeaderMap::new();
        let result = deserialize_response(b"", &headers).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn empty_body_with_content_length_zero_returns_null() {
        // Non-compliant server that sends Content-Length: 0 on a 204.
        let headers = headers_with(&[("content-length", "0")]);
        let result = deserialize_response(b"", &headers).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_apply_operation_with_root_and_field_aliases() {
        let schema = Schema::parse_and_validate(
            r#"
            type Query {
                search_items(query: String): SearchResponse
            }
            type SearchResponse {
                results: [Item!]!
                metadata: Metadata!
            }
            type Item {
                id: ID!
                title: String!
                viewUri: String!
            }
            type Metadata {
                total: Int!
            }
            "#,
            "schema.graphql",
        )
        .unwrap();

        let query = r#"
            {
                items:search_items(query: "test") {
                    results {
                        id
                        title
                        link:viewUri
                    }
                    metadata {
                        total
                    }
                }
            }
            "#;

        let operation =
            ExecutableDocument::parse_and_validate(&schema, query, "op.graphql").unwrap();

        let mapped_data = json!({
            "results": [
                { "id": "1", "title": "First", "viewUri": "https://example.com/1" },
                { "id": "2", "title": "Second", "viewUri": "https://example.com/2" }
            ],
            "metadata": { "total": 2 }
        });

        let response = MappedResponse::Data {
            key: ResponseKey::RootField {
                name: "items".to_string(),
                inputs: RequestInputs::default(),
                selection: Arc::new(JSONSelection::parse("$").unwrap()),
            },
            data: mapped_data,
            problems: vec![],
            errors: vec![],
        };

        let result = response.apply_operation(Some(&*operation), &Default::default());

        let MappedResponse::Data { data, .. } = result else {
            panic!("expected Data variant");
        };

        let items = data["results"].as_array().expect("results should be array");
        assert_eq!(items.len(), 2);

        // `link` (alias for viewUri) must be present; `viewUri` must not appear under the alias name.
        assert_eq!(
            items[0]["link"].as_str(),
            Some("https://example.com/1"),
            "field alias 'link' should resolve to viewUri value"
        );
        assert_eq!(
            items[1]["link"].as_str(),
            Some("https://example.com/2"),
            "field alias 'link' should resolve to viewUri value"
        );
        assert!(
            items[0].get("viewUri").is_none(),
            "original field name should not appear in output when aliased"
        );
    }

    fn make_connector(
        output_type: Option<Type>,
        spec: ConnectSpec,
    ) -> crate::connectors::models::Connector {
        use apollo_compiler::name;

        use crate::connectors::ConnectId;
        use crate::connectors::models::Connector;
        use crate::connectors::models::ConnectorErrorsSettings;

        Connector {
            id: ConnectId::new(
                "subgraph".into(),
                None,
                name!("Query"),
                name!("posts"),
                None,
                0,
            ),
            transport: None,
            selection: JSONSelection::parse("$").unwrap(),
            config: None,
            max_requests: None,
            entity_resolver: None,
            spec,
            schema_subtypes_map: Default::default(),
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            batch_settings: None,
            error_settings: ConnectorErrorsSettings::default(),
            output_type,
            label: "test".into(),
        }
    }

    fn root_field_key(name: &str) -> ResponseKey {
        ResponseKey::RootField {
            name: name.to_string(),
            inputs: RequestInputs::default(),
            selection: Arc::new(JSONSelection::parse("$").unwrap()),
        }
    }

    /// Build a `ResponseKey` whose selection is `selection`, so a test can
    /// exercise a real mapping rather than the identity one.
    fn root_field_key_with_selection(name: &str, selection: &str) -> ResponseKey {
        ResponseKey::RootField {
            name: name.to_string(),
            inputs: RequestInputs::default(),
            selection: Arc::new(JSONSelection::parse(selection).unwrap()),
        }
    }

    /// The whole point of `->withError`, asserted where it actually has to
    /// hold: the field resolves with its default *and* the error reaches the
    /// GraphQL `errors` array, carrying the author's code and structured
    /// fields. Asserted through `add_to_data`, the function that builds the
    /// client-facing response, rather than on the intermediate value, because
    /// the gap this closes was precisely that the intermediate value was
    /// correct and nothing carried it any further.
    #[test]
    fn a_declared_error_reaches_the_response_errors_beside_its_resolved_field() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key_with_selection(
            "account",
            r#"balance: amount ?? $("<missing>")->withError({
                message: "Field 'amount' was not found"
                extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
            })"#,
        );

        let mapped = map_response(
            &connector,
            &json!({ "id": "acct-1" }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        let mut data = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut data, &mut errors, 1).unwrap();

        // The field resolved, with the default: nothing was nulled out.
        assert_eq!(
            data.get("account"),
            Some(&json!({ "balance": "<missing>" })),
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "Field 'amount' was not found");
        assert_eq!(errors[0].code(), "INTERNAL_SERVER_ERROR");
        assert_eq!(errors[0].extensions().get("number"), Some(&json!(210099)),);
        // The connector's own identity survives alongside the author's fields.
        assert_eq!(
            errors[0].extensions().get("service"),
            Some(&json!("subgraph")),
        );
        // The path names the GraphQL field the error is about, through the
        // mapping's *output* — `balance`, the field written, not `amount`, the
        // field read. This is the acceptance criterion the original feedback
        // raised about a path reading `["->withError"]`.
        assert_eq!(errors[0].path, "account/balance");
    }

    /// Surfacing a declared error to the client must not take it away from the
    /// author. `->withError` shipped as a debugger and telemetry feature, and
    /// mapping problems are what both of those read, so a declared error has
    /// to appear in *both* places — the split decides what additionally
    /// reaches the client, not what stops reaching the debugger.
    #[test]
    fn a_declared_error_reaches_the_debugger_as_well_as_the_client() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key_with_selection(
            "account",
            r#"balance: amount ?? $("<missing>")->withError("Field 'amount' was not found")"#,
        );

        let mapped = map_response(
            &connector,
            &json!({ "id": "acct-1" }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        assert!(
            mapped
                .problems()
                .iter()
                .any(|problem| problem.message == "Field 'amount' was not found"),
            "a declared error must still reach the debugger, got: {:?}",
            mapped.problems(),
        );

        let mut data = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut data, &mut errors, 1).unwrap();
        assert_eq!(errors.len(), 1, "and still reach the client");
    }

    /// The error path has to name the field the mapping *writes*, not the one
    /// it reads, and has to keep doing so through nesting and lists. Each case
    /// here reads a differently-named source field, so a path built from the
    /// read side would be visibly wrong rather than accidentally right.
    #[test]
    fn declared_error_paths_name_the_written_field() {
        let cases: &[(&str, Value, &str)] = &[
            // A plain rename.
            (
                r#"balance: amount->withError("x")"#,
                json!({ "amount": 1 }),
                "account/balance",
            ),
            // Nesting: the path is the full route through the output object,
            // which shares no segment with the input path (`amount`).
            (
                r#"acct: { bal: amount->withError("x") }"#,
                json!({ "amount": 1 }),
                "account/acct/bal",
            ),
            // A deep read collapsing to a shallow write.
            (
                r#"bal: a.b.c->withError("x")"#,
                json!({ "a": { "b": { "c": 1 } } }),
                "account/bal",
            ),
            // ->map is index-preserving, so the element is named.
            (
                r#"rows: items->map(@.code->withError("x"))"#,
                json!({ "items": [{ "code": 1 }] }),
                "account/rows/0",
            ),
            // Auto-mapping a subselection over an array, down to the field.
            (
                r#"rows: items { c: code->withError("x") }"#,
                json!({ "items": [{ "code": 1 }] }),
                "account/rows/0/c",
            ),
        ];

        for (selection, data, expected_path) in cases {
            let connector = make_connector(None, ConnectSpec::V0_5);
            let key = root_field_key_with_selection("account", selection);
            let mapped = map_response(&connector, data, key, IndexMap::default(), Vec::new());

            let mut out = Map::new();
            let mut errors = Vec::new();
            mapped.add_to_data(&mut out, &mut errors, 1).unwrap();

            assert_eq!(errors.len(), 1, "for selection `{selection}`");
            assert_eq!(
                &errors[0].path, expected_path,
                "for selection `{selection}`",
            );
        }
    }

    /// `->filter` renumbers: the element at input index 1 can land at output
    /// index 0. The mapping engine deliberately does not prepend an index
    /// there, so the path stops at the list rather than naming the wrong
    /// element. A coarse path that resolves beats a precise one that lies.
    #[test]
    fn a_declared_error_under_filter_does_not_claim_an_element_index() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key_with_selection(
            "account",
            r#"rows: items->filter(@.keep->withError("checking"))"#,
        );

        let mapped = map_response(
            &connector,
            &json!({ "items": [{ "keep": false }, { "keep": true }] }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        let mut out = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut out, &mut errors, 1).unwrap();

        for error in &errors {
            assert_eq!(
                error.path, "account/rows",
                "filter must not attribute an input index to an output element",
            );
        }
    }

    /// The counterpart: a mapping *diagnostic* describes the mapping's
    /// internals and must never reach a client. Both kinds ride the same
    /// `Vec<ApplyToError>` out of the selection, so this is the assertion that
    /// the split actually splits.
    #[test]
    fn a_mapping_diagnostic_stays_out_of_the_response_errors() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key_with_selection("account", "balance: nope");

        let mapped = map_response(
            &connector,
            &json!({ "id": "acct-1" }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        // The diagnostic was recorded for the author...
        assert!(
            mapped
                .problems()
                .iter()
                .any(|problem| problem.message.contains("not found")),
            "expected a diagnostic problem, got: {:?}",
            mapped.problems(),
        );

        // ...and stayed out of the client's response.
        let mut data = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut data, &mut errors, 1).unwrap();
        assert_eq!(errors.len(), 0);
    }

    /// `->withError` behaves the same at every spec version, like every other
    /// mapping method. Nothing about a method's behavior is version-dependent —
    /// the rstest cases spanning V0_2..V0_5 throughout the methods directory
    /// exist to assert that — so this is written the same way, to catch anyone
    /// reintroducing a gate here.
    #[rstest::rstest]
    #[case::v0_1(ConnectSpec::V0_1)]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    #[case::v0_4(ConnectSpec::V0_4)]
    #[case::v0_5(ConnectSpec::V0_5)]
    fn a_declared_error_reaches_the_response_at_every_spec_version(#[case] spec: ConnectSpec) {
        let connector = make_connector(None, spec);
        let key = root_field_key_with_selection(
            "account",
            r#"balance: amount ?? $("<missing>")->withError("Field 'amount' was not found")"#,
        );

        let mapped = map_response(
            &connector,
            &json!({ "id": "acct-1" }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        let problems = mapped.problems().to_vec();

        let mut data = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut data, &mut errors, 1).unwrap();

        assert_eq!(errors.len(), 1, "{spec:?} must surface declared errors");
        assert_eq!(errors[0].message, "Field 'amount' was not found");
        assert!(
            problems
                .iter()
                .any(|problem| problem.message == "Field 'amount' was not found"),
            "the message should still reach the debugger",
        );
    }

    /// Every declared error is reported, however many there are. The feature
    /// exists so an author can record every defect they find, so truncating
    /// would quietly defeat the thing it was asked for — and nothing else in
    /// the router truncates a response's errors either. Response size is an
    /// operator policy, set upstream via the `http_max_response_size` connector
    /// limit, not a constant hidden in the mapping layer.
    #[test]
    fn declared_errors_are_not_truncated() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key_with_selection(
            "rows",
            r#"$.rows->map(@.code->withError("bad code:", @))"#,
        );

        let row_count = 500;
        let rows = (0..row_count)
            .map(|index| json!({ "code": index }))
            .collect::<Vec<_>>();

        let mapped = map_response(
            &connector,
            &json!({ "rows": rows }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        let mut data = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut data, &mut errors, 1).unwrap();

        assert_eq!(errors.len(), row_count);
        // And each one still names its own element, rather than the last few
        // being summarized away.
        assert_eq!(errors[0].path, "rows/0");
        assert_eq!(
            errors[row_count - 1].path,
            format!("rows/{}", row_count - 1)
        );
    }

    /// Declared errors from a `->map` stay one-per-element rather than being
    /// collapsed by message the way mapping problems are, and each keeps the
    /// element it came from in `connector.selectionPath`. This is the reason
    /// they are capped rather than aggregated: aggregation groups array
    /// indices together under `@`, which would erase exactly this.
    #[test]
    fn declared_errors_from_a_list_are_not_collapsed_by_message() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key_with_selection(
            "rows",
            r#"$.rows->map(@.code->withError("bad code:", @))"#,
        );

        let mapped = map_response(
            &connector,
            &json!({ "rows": [{ "code": 7 }, { "code": 7 }] }),
            key,
            IndexMap::default(),
            Vec::new(),
        );

        let mut data = Map::new();
        let mut errors = Vec::new();
        mapped.add_to_data(&mut data, &mut errors, 1).unwrap();

        // Identical messages, still two errors — aggregation would have made
        // this one problem with a count of 2.
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].message, "bad code: 7");
        assert_eq!(errors[1].message, "bad code: 7");

        // And each one still says which element it came from.
        let selection_paths = errors
            .iter()
            .map(|error| {
                error
                    .extensions()
                    .get("connector")
                    .and_then(|connector| connector.as_object()?.get("selectionPath"))
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selection_paths,
            vec![Some(json!("rows.0.code")), Some(json!("rows.1.code"))],
        );
    }

    // CNN-564: when schema declares a list field but API returns a single object,
    // check_response_shape must return an error instead of silently passing through
    // the wrong-shaped data (which the router would null without explanation).
    #[test]
    fn list_field_with_object_response_produces_error() {
        let connector = make_connector(Some(apollo_compiler::ty!([Post])), ConnectSpec::V0_5);
        let key = root_field_key("posts");
        let response = MappedResponse::Data {
            data: json!({"id": 1, "title": "First"}),
            key,
            problems: vec![],
            errors: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        let MappedResponse::Error { error, .. } = result else {
            panic!("expected Error variant, got Data");
        };
        assert!(
            error.message.contains("not a list"),
            "error message should explain the mismatch, got: {:?}",
            error.message
        );
        assert_eq!(error.code(), "CONNECTORS_RESPONSE_SHAPE");
    }

    // CNN-564: when schema declares a single-object field but API returns an array,
    // check_response_shape must return an error.
    #[test]
    fn non_list_field_with_array_response_produces_error() {
        let connector = make_connector(Some(apollo_compiler::ty!(Post)), ConnectSpec::V0_5);
        let key = root_field_key("post");
        let response = MappedResponse::Data {
            data: json!([{"id": 1}, {"id": 2}]),
            key,
            problems: vec![],
            errors: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        let MappedResponse::Error { error, .. } = result else {
            panic!("expected Error variant, got Data");
        };
        assert!(
            error.message.contains("a list"),
            "error message should explain the mismatch, got: {:?}",
            error.message
        );
        assert_eq!(error.code(), "CONNECTORS_RESPONSE_SHAPE");
    }

    // CNN-564: when schema declares a nullable list field and API returns null,
    // do NOT emit a shape-mismatch error (null is a valid/separate failure mode).
    #[test]
    fn nullable_list_field_with_null_response_passes_through() {
        let connector = make_connector(Some(apollo_compiler::ty!([Post])), ConnectSpec::V0_5);
        let key = root_field_key("posts");
        let response = MappedResponse::Data {
            data: Value::Null,
            key,
            problems: vec![],
            errors: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        assert!(
            matches!(result, MappedResponse::Data { .. }),
            "null data should pass through without a shape-mismatch error"
        );
    }

    // CNN-564: when the field's arrayness is unknown (e.g. type-level
    // connectors with no field definition), skip the check entirely rather
    // than treating `None` as `false` and false-positiving on arrayness.
    #[test]
    fn unknown_arrayness_skips_check() {
        let connector = make_connector(None, ConnectSpec::V0_5);
        let key = root_field_key("posts");
        let response = MappedResponse::Data {
            data: json!([{"id": 1}, {"id": 2}]),
            key,
            problems: vec![],
            errors: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        assert!(
            matches!(result, MappedResponse::Data { .. }),
            "unknown arrayness should not produce a shape-mismatch error"
        );
    }

    // CNN-564: when schema declares a non-null field and the API returns
    // null, emit an actionable error instead of relying on the generic
    // null-bubbling machinery, which by default only surfaces via
    // `extensions.valueCompletion` rather than a top-level error.
    #[test]
    fn non_null_field_with_null_response_produces_error() {
        let connector = make_connector(Some(apollo_compiler::ty!([Post]!)), ConnectSpec::V0_5);
        let key = root_field_key("posts");
        let response = MappedResponse::Data {
            data: Value::Null,
            key,
            problems: vec![],
            errors: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        let MappedResponse::Error { error, .. } = result else {
            panic!("expected Error variant, got Data");
        };
        assert!(
            error.message.contains("non-nullable"),
            "error message should explain the non-null violation, got: {:?}",
            error.message
        );
        assert_eq!(error.code(), "CONNECTORS_RESPONSE_SHAPE");
    }

    // CNN-564: this check is a visible behavior change (null -> error), so it
    // must only run for connect/v0.5+; earlier connectors are unaffected when
    // they upgrade.
    #[test]
    fn pre_v0_5_connectors_are_unaffected() {
        let connector = make_connector(Some(apollo_compiler::ty!([Post]!)), ConnectSpec::V0_4);
        let key = root_field_key("posts");
        let response = MappedResponse::Data {
            data: json!({"id": 1, "title": "First"}),
            key,
            problems: vec![],
            errors: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        assert!(
            matches!(result, MappedResponse::Data { .. }),
            "v0.4 and earlier connectors must not be affected by this check"
        );
    }

    // CNN-564: when the response is already an Error, check_response_shape must
    // leave it untouched (don't double-error).
    #[test]
    fn error_response_is_unchanged_by_list_check() {
        use crate::connectors::runtime::errors::RuntimeError;

        let connector = make_connector(Some(apollo_compiler::ty!([Post])), ConnectSpec::V0_5);
        let key = root_field_key("posts");
        let error = RuntimeError::new("original error", &key);
        let response = MappedResponse::Error {
            error,
            key,
            problems: vec![],
        };

        let result = super::check_response_shape(&connector, response);

        let MappedResponse::Error { error, .. } = result else {
            panic!("expected Error variant");
        };
        assert_eq!(error.message, "original error");
    }
}
