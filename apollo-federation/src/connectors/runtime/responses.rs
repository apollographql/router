use std::cell::LazyCell;
use std::sync::Arc;

use apollo_compiler::collections::HashMap;
use http::HeaderMap;
use http::HeaderValue;
use itertools::Itertools;
use parking_lot::Mutex;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;

use crate::connectors::Connector;
use crate::connectors::JSONSelection;
use crate::connectors::ProblemLocation;
use crate::connectors::runtime::debug::ConnectorContext;
use crate::connectors::runtime::debug::ConnectorDebugHttpRequest;
use crate::connectors::runtime::debug::SelectionData;
use crate::connectors::runtime::errors::Error;
use crate::connectors::runtime::errors::RuntimeError;
use crate::connectors::runtime::http_json_transport::TransportResponse;
use crate::connectors::runtime::inputs::ContextReader;
use crate::connectors::runtime::key::ResponseKey;
use crate::connectors::runtime::mapping::Problem;
use crate::connectors::runtime::mapping::aggregate_apply_to_errors;

const ENTITIES: &str = "_entities";
const TYPENAME: &str = "__typename";

#[derive(Debug, thiserror::Error)]
pub enum HandleResponseError {
    #[error("Merge error: {0}")]
    MergeError(String),
}

// --- RAW RESPONSE ------------------------------------------------------------

pub enum RawResponse {
    /// This error type is used if:
    /// 1. We didn't even make the request (we hit the request limit)
    /// 2. We couldn't deserialize the response body
    Error {
        error: RuntimeError,
        key: ResponseKey,
    },
    /// Contains the response data directly from the HTTP response. We'll apply
    /// a selection to convert this into either `data` or `errors` based on
    /// whether it's successful or not.
    Data {
        parts: http::response::Parts,
        data: Value,
        key: ResponseKey,
        debug_request: (
            Option<Box<ConnectorDebugHttpRequest>>,
            Vec<(ProblemLocation, Problem)>,
        ),
    },
}

impl RawResponse {
    /// Returns a response with data transformed by the selection mapping.
    ///
    /// As a side effect, this will also write to the debug context.
    pub fn map_response(
        self,
        result: Result<TransportResponse, Error>,
        connector: &Connector,
        context: impl ContextReader,
        debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
        client_headers: &HeaderMap<HeaderValue>,
    ) -> (MappedResponse, Result<TransportResponse, Error>) {
        let mapped_response = match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                data,
                key,
                parts,
                debug_request,
            } => {
                let inputs = key
                    .inputs()
                    .clone()
                    .merger(&connector.response_variables)
                    .config(connector.config.as_ref())
                    .context(context)
                    .status(parts.status.as_u16())
                    .request(&connector.response_headers, client_headers)
                    .response(&connector.response_headers, Some(&parts))
                    .env(&connector.env)
                    .merge();

                let (res, apply_to_errors) = key.selection().apply_with_vars(&data, &inputs);

                let mapping_problems: Vec<Problem> =
                    aggregate_apply_to_errors(apply_to_errors).collect();

                if let Some(debug) = debug_context {
                    let mut debug_problems: Vec<(ProblemLocation, Problem)> = mapping_problems
                        .iter()
                        .map(|problem| (ProblemLocation::Selection, problem.clone()))
                        .collect();
                    debug_problems.extend(debug_request.1);

                    debug.lock().push_response(
                        debug_request.0,
                        &parts,
                        &data,
                        Some(SelectionData {
                            source: connector.selection.to_string(),
                            transformed: key.selection().to_string(),
                            result: res.clone(),
                        }),
                        &connector.error_settings,
                        debug_problems,
                    );
                }

                MappedResponse::Data {
                    key,
                    data: res.unwrap_or_else(|| Value::Null),
                    problems: mapping_problems,
                }
            }
        };

        (mapped_response, result)
    }

    /// Returns a `MappedResponse` with a GraphQL error.
    ///
    /// As a side effect, this will also write to the debug context.
    pub fn map_error(
        self,
        result: Result<TransportResponse, Error>,
        connector: &Connector,
        context: impl ContextReader,
        debug_context: &Option<Arc<Mutex<ConnectorContext>>>,
        client_headers: &HeaderMap<HeaderValue>,
    ) -> (MappedResponse, Result<TransportResponse, Error>) {
        use serde_json_bytes::*;

        let mapped_response = match self {
            RawResponse::Error { error, key } => MappedResponse::Error { error, key },
            RawResponse::Data {
                key,
                parts,
                debug_request,
                data,
            } => {
                let mut warnings = Vec::new();

                let inputs = LazyCell::new(|| {
                    key.inputs()
                        .clone()
                        .merger(&connector.response_variables)
                        .config(connector.config.as_ref())
                        .context(context)
                        .status(parts.status.as_u16())
                        .request(&connector.response_headers, client_headers)
                        .response(&connector.response_headers, Some(&parts))
                        .merge()
                });

                // Do we have a error message mapping set for this connector?
                let message = if let Some(message_selection) = &connector.error_settings.message {
                    let (res, apply_to_errors) = message_selection.apply_with_vars(&data, &inputs);
                    warnings.extend(
                        aggregate_apply_to_errors(apply_to_errors)
                            .map(|problem| (ProblemLocation::ErrorsMessage, problem)),
                    );

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
                    let (res, apply_to_errors) =
                        extensions_selection.apply_with_vars(&data, &inputs);
                    warnings.extend(
                        aggregate_apply_to_errors(apply_to_errors)
                            .map(|problem| (ProblemLocation::SourceErrorsExtensions, problem)),
                    );

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
                        error = error.extension(key.clone(), value.clone());
                    }
                }

                if let Some(extensions_selection) = &connector.error_settings.connect_extensions {
                    let (res, apply_to_errors) =
                        extensions_selection.apply_with_vars(&data, &inputs);
                    warnings.extend(
                        aggregate_apply_to_errors(apply_to_errors)
                            .map(|problem| (ProblemLocation::ConnectErrorsExtensions, problem)),
                    );

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
                        error = error.extension(key.clone(), value.clone());
                    }
                }

                error = error.with_code(extension_code);

                if let Some(debug) = debug_context {
                    debug.lock().push_response(
                        debug_request.0.clone(),
                        &parts,
                        &data,
                        None,
                        &connector.error_settings,
                        [debug_request.1, warnings]
                            .iter()
                            .flatten()
                            .cloned()
                            .collect(),
                    );
                }

                MappedResponse::Error { error, key }
            }
        };

        (mapped_response, result)
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
        data: &mut serde_json_bytes::Map<ByteString, Value>,
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
                            let mut entity = serde_json_bytes::Map::new();
                            if let Some(typename) = typename {
                                entity.insert(TYPENAME, Value::String(typename.as_str().into()));
                            }
                            entity.insert(field_name.clone(), value);
                            entities.insert(index, Value::Object(entity));
                        }
                    };
                }
                ResponseKey::BatchEntity { keys, inputs, .. } => {
                    let Value::Array(values) = value else {
                        return Err(HandleResponseError::MergeError(
                            "Response for a batch request does not map to an array".into(),
                        ));
                    };

                    let key_selection: Result<JSONSelection, _> = keys.try_into();
                    let key_selection = key_selection
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
}
