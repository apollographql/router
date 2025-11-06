use apollo_compiler::collections::HashMap;
use apollo_compiler::collections::IndexMap;
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
    // If the body is obviously empty, don't try to parse it
    if headers
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
    let (success, warnings) = is_success(connector, data, parts, &inputs, warnings);
    if success {
        map_response(data, key, inputs, warnings)
    } else {
        map_error(connector, data, parts, key, inputs, warnings)
    }
}

// If the user has set a custom success condition selector, resolve that expression,
// otherwise default to checking status code is 2XX
fn is_success(
    connector: &Connector,
    data: &Value,
    parts: &Parts,
    inputs: &IndexMap<String, Value>,
    mut warnings: Vec<Problem>,
) -> (bool, Vec<Problem>) {
    let Some(is_success_selection) = &connector.error_settings.connect_is_success else {
        return (parts.status.is_success(), warnings);
    };
    let (res, apply_to_errors) = is_success_selection.apply_with_vars(data, inputs);
    warnings.extend(aggregate_apply_to_errors(
        apply_to_errors,
        ProblemLocation::IsSuccess,
    ));

    (
        res.as_ref().and_then(Value::as_bool).unwrap_or_default(),
        warnings,
    )
}

/// Returns a response with data transformed by the selection mapping.
pub(super) fn map_response(
    data: &Value,
    key: ResponseKey,
    inputs: IndexMap<String, Value>,
    mut warnings: Vec<Problem>,
) -> MappedResponse {
    let (res, apply_to_errors) = key.selection().apply_with_vars(data, &inputs);
    warnings.extend(aggregate_apply_to_errors(
        apply_to_errors,
        ProblemLocation::Selection,
    ));
    MappedResponse::Data {
        key,
        data: res.unwrap_or_else(|| Value::Null),
        problems: warnings,
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
    // We'll merge by applying the source and then the connect. Keep in mind that these will override defaults if the key names are the same.
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
            error = error.extension(key, value);
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
            error = error.extension(key, value);
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
                data: value, key, ..
            } => match key {
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
                                entity.insert(TYPENAME, Value::String(typename.as_str().into()));
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
            },
        }

        Ok(())
    }

    pub fn problems(&self) -> &[Problem] {
        match self {
            Self::Error { problems, .. } | Self::Data { problems, .. } => problems,
        }
    }
}
