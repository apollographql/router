use std::sync::Arc;

use apollo_compiler::executable::Selection;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_federation::sources::connect::ApplyToError;
use http::Method;
use http::Uri;
use itertools::Itertools;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tower::BoxError;

use super::connector::ConnectorKind;
use super::connector::ConnectorTransport;
use super::directives::KeyTypeMap;
use super::http_json_transport::HttpJsonTransportError;
use super::request_inputs::RequestInputs;
use super::response_formatting::execute;
use super::response_formatting::response::FormattingDiagnostic;
use super::response_formatting::JsonMap;
use super::Connector;
use crate::json_ext::Object;
use crate::plugins::telemetry::LOGGING_DISPLAY_BODY;
use crate::plugins::telemetry::LOGGING_DISPLAY_HEADERS;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::Context;

const REPRESENTATIONS_VAR: &str = "representations";
const ENTITIES: &str = "_entities";

#[derive(Debug)]
pub(crate) struct ResponseParams {
    key: ResponseKey,
    source_api_name: Arc<String>,
}

#[derive(Clone, Debug)]
enum ResponseKey {
    RootField {
        name: String,
        typename: ResponseTypeName,
    },
    Entity {
        index: usize,
        typename: ResponseTypeName,
    },
    EntityField {
        index: usize,
        field_name: String,
        typename: ResponseTypeName,
    },
}

#[derive(Clone, Debug)]
enum ResponseTypeName {
    Concrete(String),
    /// For interfaceObject support. We don't want to include __typename in the
    /// response because this subgraph doesn't know the concrete type
    Omitted,
}

pub(super) fn make_requests(
    request: SubgraphRequest,
    connector: &Connector,
    schema: Arc<Valid<Schema>>,
) -> Result<Vec<(http::Request<hyper::Body>, ResponseParams)>, MakeRequestError> {
    let parts = match connector.kind {
        ConnectorKind::RootField { .. } => root_fields(&request, schema)?,
        ConnectorKind::Entity { .. } => entities_from_request(&request, schema)?,
        ConnectorKind::EntityField { .. } => {
            entities_with_fields_from_request(&request, schema, connector)?
        }
    };

    request_params_to_requests(connector, parts, &request)
}

fn request_params_to_requests(
    connector: &Connector,
    from_request: Vec<(ResponseKey, RequestInputs)>,
    original_request: &SubgraphRequest,
) -> Result<Vec<(http::Request<hyper::Body>, ResponseParams)>, MakeRequestError> {
    from_request
        .into_iter()
        .map(|(response_key, inputs)| {
            let inputs = inputs.merge();

            let request = match connector.transport {
                ConnectorTransport::HttpJson(ref transport) => {
                    transport.make_request(inputs, original_request)?
                }
            };

            let response_params = ResponseParams {
                key: response_key,
                source_api_name: connector.transport.source_api_name().clone(),
            };

            Ok((request, response_params))
        })
        .collect::<Result<Vec<_>, _>>()
}

// --- DIAGNOSTICS -------------------------------------------------------------

#[derive(Debug)]
pub(super) enum Diagnostic {
    Response {
        connector: String,
        message: String,
        path: String,
    },
}

impl Diagnostic {
    fn from_response_selection(connector: &Connector, err: ApplyToError) -> Self {
        Self::Response {
            connector: connector.debug_name(),
            message: err
                .message()
                .unwrap_or("issue applying response selection")
                .to_string(),
            path: err.path().unwrap_or_default(),
        }
    }

    fn from_response_formatting(connector: &Connector, err: FormattingDiagnostic) -> Self {
        use super::response_formatting::response::ResponseDataPathElement::*;

        Self::Response {
            connector: connector.debug_name(),
            message: err.message,
            path: err
                .path
                .iter()
                .map(|p| match p {
                    Field(name) => name.as_str().to_string(),
                    ListIndex(i) => format!("{}", i),
                })
                .join("."),
        }
    }

    pub(super) fn log(&self) {
        match self {
            Diagnostic::Response {
                connector,
                message,
                path,
            } => {
                tracing::debug!(
                    connector = connector.as_str(),
                    message = message.as_str(),
                    path = path.as_str(),
                    "connector response/selection mismatch"
                );
            }
        }
    }
}

// --- ERRORS ------------------------------------------------------------------

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(super) enum MakeRequestError {
    /// Invalid request operation: {0}
    InvalidOperation(String),

    /// Unsupported request operation: {0}
    UnsupportedOperation(String),

    /// Invalid request arguments: {0}
    InvalidArguments(String),

    /// Invalid entity representation: {0}
    InvalidRepresentations(String),

    /// Cannot create HTTP request: {0}
    TransportError(#[from] HttpJsonTransportError),
}

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub(super) enum HandleResponseError {
    /// Missing response params
    MissingResponseParams,

    /// Invalid response body: {0}
    InvalidResponseBody(String),

    /// Cannot map response: {0}
    MapResponseError(#[from] HttpJsonTransportError),

    /// Merge error: {0}
    MergeError(String),

    /// ResponseFormattingError: {0}
    ResponseFormattingError(String),
}

// --- ROOT FIELDS -------------------------------------------------------------

/// Given a query, find the root fields and return a list of requests.
/// The connector subgraph must have only a single root field, but it could be
/// used multiple times with aliases.
fn root_fields(
    request: &SubgraphRequest,
    schema: Arc<Valid<Schema>>,
) -> Result<Vec<(ResponseKey, RequestInputs)>, MakeRequestError> {
    let query = request
        .subgraph_request
        .body()
        .query
        .clone()
        .ok_or_else(|| MakeRequestError::InvalidOperation("missing query".into()))?;

    let doc = ExecutableDocument::parse(&schema, query, "op.graphql").map_err(|_| {
        MakeRequestError::InvalidOperation("cannot parse operation document".into())
    })?;

    let op = doc
        .get_operation(request.subgraph_request.body().operation_name.as_deref())
        .map_err(|_| MakeRequestError::InvalidOperation("no operation found".into()))?;

    op.selection_set
        .selections
        .iter()
        .map(|s| match s {
            Selection::Field(field) => {
                let response_key = ResponseKey::RootField {
                    name: field.response_key().to_string(),
                    typename: ResponseTypeName::Concrete(field.ty().inner_named_type().to_string()),
                };

                let arguments = graphql_utils::field_arguments_map(
                    field,
                    &request.subgraph_request.body().variables,
                )
                .map_err(|_| {
                    MakeRequestError::InvalidArguments(
                        "cannot get inputs from field arguments".into(),
                    )
                })?;

                let request_inputs = RequestInputs {
                    arguments,
                    parent: Default::default(),
                };

                Ok((response_key, request_inputs))
            }

            // The query planner removes fragments at the root so we don't have
            // to worry these branches
            Selection::FragmentSpread(_) => Err(MakeRequestError::UnsupportedOperation(
                "top-level fragments in query planner nodes should not happen".into(),
            )),
            Selection::InlineFragment(_) => Err(MakeRequestError::UnsupportedOperation(
                "top-level inline fragments in query planner nodes should not happen".into(),
            )),
        })
        .collect::<Result<Vec<_>, MakeRequestError>>()
}

// --- ENTITIES ----------------------------------------------------------------

/// Given entity representations:
///
/// variables: { representations: [{ __typename: "User", id: "1" }] }
///
/// Return a list of requests to make, as well as the response key (index in list) for each.
fn entities_from_request(
    request: &SubgraphRequest,
    _schema: Arc<Valid<Schema>>,
) -> Result<Vec<(ResponseKey, RequestInputs)>, MakeRequestError> {
    use MakeRequestError::InvalidRepresentations;

    let (_, typename_requested) = graphql_utils::get_entity_fields(
        request
            .subgraph_request
            .body()
            .query
            .as_ref()
            .ok_or_else(|| MakeRequestError::InvalidOperation("missing query".into()))?,
    )?;

    request
        .subgraph_request
        .body()
        .variables
        .get(REPRESENTATIONS_VAR)
        .ok_or_else(|| InvalidRepresentations("missing representations variable".into()))?
        .as_array()
        .ok_or_else(|| InvalidRepresentations("representations is not an array".into()))?
        .iter()
        .enumerate()
        .map(|(i, rep)| {
            // TODO abstract types?
            let typename = rep
                .as_object()
                .ok_or_else(|| InvalidRepresentations("representation is not an object".into()))?
                .get("__typename")
                .ok_or_else(|| {
                    InvalidRepresentations("representation is missing __typename".into())
                })?
                .as_str()
                .ok_or_else(|| InvalidRepresentations("__typename is not a string".into()))?
                .to_string();

            let typename = if typename_requested {
                ResponseTypeName::Concrete(typename)
            } else {
                ResponseTypeName::Omitted
            };

            Ok((
                ResponseKey::Entity { index: i, typename },
                RequestInputs {
                    arguments: Default::default(),
                    parent: rep
                        .as_object()
                        .ok_or_else(|| {
                            InvalidRepresentations("representation is not an object".into())
                        })?
                        .clone(),
                },
            ))
        })
        .collect::<Result<Vec<_>, _>>()
}

// --- ENTITY FIELDS -----------------------------------------------------------

/// Given an entities query and variables:
///
/// query: "{ _entities(representations: $representations) { ... on User { name } } }"
/// variables: { representations: [{ __typename: "User", id: "1" }] }
///
/// Return a list of requests to make, as well as the response key (index in list and name/alias of field) for each.
fn entities_with_fields_from_request(
    request: &SubgraphRequest,
    _schema: Arc<Valid<Schema>>,
    connector: &Connector,
) -> Result<Vec<(ResponseKey, RequestInputs)>, MakeRequestError> {
    // TODO this is the fallback when using the magic finder field, which means
    // we won't have a type condition in the query
    let typename = match connector.kind {
        ConnectorKind::EntityField { ref type_name, .. } => type_name,
        _ => unreachable!(),
    };

    let (entities_field, typename_requested) = graphql_utils::get_entity_fields(
        request
            .subgraph_request
            .body()
            .query
            .as_ref()
            .ok_or_else(|| MakeRequestError::InvalidOperation("missing query".into()))?,
    )?;

    let types_and_fields = entities_field
        .selection_set
        .iter()
        .map(|selection| match selection {
            apollo_compiler::ast::Selection::Field(f) => {
                // allow __typename outside of the type condition
                if f.name == "__typename" {
                    Ok(vec![])
                } else {
                    // if we're using the magic finder field, the query planner doesn't use an inline fragment
                    // (because the output type in not an interface)
                    Ok(vec![(typename.to_string(), f)])
                }
            }

            apollo_compiler::ast::Selection::FragmentSpread(_) => {
                Err(MakeRequestError::InvalidOperation(
                    "_entities selection can't be a named fragment".into(),
                ))
            }

            apollo_compiler::ast::Selection::InlineFragment(frag) => {
                let type_name = frag.type_condition.as_ref().ok_or_else(|| {
                    MakeRequestError::InvalidOperation("missing type condition".into())
                })?;
                Ok(frag
                    .selection_set
                    .iter()
                    .map(|sel| {
                        let field = match sel {
                            apollo_compiler::ast::Selection::Field(f) => f,
                            apollo_compiler::ast::Selection::FragmentSpread(_) => todo!(),
                            apollo_compiler::ast::Selection::InlineFragment(_) => todo!(),
                        };
                        (type_name.to_string(), field)
                    })
                    .collect::<Vec<_>>())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let representations = request
        .subgraph_request
        .body()
        .variables
        .get(REPRESENTATIONS_VAR)
        .ok_or_else(|| {
            MakeRequestError::InvalidRepresentations("missing representations variable".into())
        })?
        .as_array()
        .ok_or_else(|| {
            MakeRequestError::InvalidRepresentations("representations is not an array".into())
        })?
        .iter()
        .enumerate()
        .collect::<Vec<_>>();

    // if we have multiple fields (because of aliases, we'll flatten that list)
    // and generate requests for each field/representation pair
    types_and_fields
        .into_iter()
        .flatten()
        .flat_map(|(typename, field)| {
            representations.iter().map(move |(i, representation)| {
                let arguments = graphql_utils::ast_field_arguments_map(
                    field,
                    &request.subgraph_request.body().variables,
                )
                .map_err(|_| {
                    MakeRequestError::InvalidArguments(
                        "cannot build inputs from field arguments".into(),
                    )
                })?;

                let typename = if typename_requested {
                    ResponseTypeName::Concrete(typename.to_string())
                } else {
                    ResponseTypeName::Omitted
                };

                Ok::<_, MakeRequestError>((
                    ResponseKey::EntityField {
                        index: *i,
                        field_name: field.response_name().to_string(),
                        typename,
                    },
                    RequestInputs {
                        arguments,
                        parent: representation
                            .as_object()
                            .ok_or_else(|| {
                                MakeRequestError::InvalidRepresentations(
                                    "representation is not an object".into(),
                                )
                            })?
                            .clone(),
                    },
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

// --- RESPONSES ---------------------------------------------------------------

pub(super) async fn handle_responses(
    schema: &Valid<Schema>,
    document: Option<String>,
    context: Context,
    connector: &Connector,
    responses: Vec<http::Response<hyper::Body>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SubgraphResponse, HandleResponseError> {
    use HandleResponseError::*;

    // If the query plan fetch used the "magic finder" field, we'll stick the
    // response data under that key so we can apply the GraphQL selection
    // set to handle aliases and type names.
    let (document, magic_response_key, result_uses_entities) =
        handle_magic_finder(document, connector, schema)
            .map_err(|e| ResponseFormattingError(e.to_string()))?;
    let original_magic_response_key = magic_response_key.clone();
    let entity_response_key = magic_response_key.unwrap_or(ENTITIES.into());

    let mut data = serde_json_bytes::Map::new();
    let mut errors = Vec::new();

    let display_body = context
        .get(LOGGING_DISPLAY_BODY)
        .unwrap_or_default()
        .unwrap_or_default();
    let display_headers = context
        .get(LOGGING_DISPLAY_HEADERS)
        .unwrap_or_default()
        .unwrap_or_default();

    for response in responses {
        let (parts, body) = response.into_parts();

        let response_params = parts
            .extensions
            .get::<ResponseParams>()
            .ok_or_else(|| MissingResponseParams)?;

        let (url, method) = (display_headers || display_body)
            .then(|| {
                let extensions = &parts.extensions;
                (
                    extensions
                        .get::<Uri>()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    extensions
                        .get::<Method>()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                )
            })
            .unwrap_or_default();

        if display_headers {
            tracing::info!(http.response.headers = ?parts.headers, url.full = %url, method = %method, "Response headers received from REST endpoint");
        }

        let api_name = Arc::clone(&response_params.source_api_name);
        u64_counter!(
            "apollo.router.operations.source.rest",
            "rest source api calls",
            1,
            rest.response.api = api_name.to_string(),
            rest.response.status_code = parts.status.as_u16() as i64
        );

        let body = &hyper::body::to_bytes(body)
            .await
            .map_err(|_| InvalidResponseBody("couldn't retrieve http response body".into()))?;

        if display_body {
            tracing::info!(http.response.body = ?String::from_utf8_lossy(body), url.full = %url, method = %method, "Response body received from REST endpoint");
        }

        if parts.status.is_success() {
            let json_data: Value = serde_json::from_slice(body)
                .map_err(|_| InvalidResponseBody("couldn't deserialize response body".into()))?;

            let mut res_data = match connector.transport {
                ConnectorTransport::HttpJson(ref transport) => {
                    let mut selection_diagnostics = Vec::new();
                    let res = transport
                        .map_response(json_data, &mut selection_diagnostics)
                        .map_err(MapResponseError)?;

                    for diagnostic in selection_diagnostics {
                        diagnostics
                            .push(Diagnostic::from_response_selection(connector, diagnostic));
                    }
                    res
                }
            };

            match response_params.key {
                // add the response to the "data" using the root field name or alias
                ResponseKey::RootField {
                    ref name,
                    ref typename,
                } => {
                    if let ResponseTypeName::Concrete(typename) = typename {
                        inject_typename(&mut res_data, typename, &None);
                    }

                    data.insert(name.clone(), res_data);
                }

                // add the response to the "_entities" array at the right index
                ResponseKey::Entity {
                    index,
                    ref typename,
                } => {
                    if let ResponseTypeName::Concrete(typename) = typename {
                        inject_typename(&mut res_data, typename, &connector.key_type_map);
                    }

                    let entities = data
                        .entry(entity_response_key.clone())
                        .or_insert(Value::Array(vec![]));
                    entities
                        .as_array_mut()
                        .ok_or_else(|| MergeError("entities is not an array".into()))?
                        .insert(index, res_data);
                }

                // make an entity object and assign the response to the appropriate field or aliased field,
                // then add the object to the _entities array at the right index (or add the field to an existing object)
                ResponseKey::EntityField {
                    index,
                    ref field_name,
                    ref typename,
                } => {
                    let entities = data
                        .entry(entity_response_key.clone())
                        .or_insert(Value::Array(vec![]))
                        .as_array_mut()
                        .ok_or_else(|| MergeError("entities is not an array".into()))?;

                    match entities.get_mut(index) {
                        Some(Value::Object(entity)) => {
                            entity.insert(field_name.clone(), res_data);
                        }
                        _ => {
                            let mut entity = serde_json_bytes::Map::new();
                            if let ResponseTypeName::Concrete(typename) = typename {
                                entity.insert("__typename", Value::String(typename.clone().into()));
                            }
                            entity.insert(field_name.clone(), res_data);
                            entities.insert(index, Value::Object(entity));
                        }
                    };
                }
            }
        } else {
            errors.push(
                crate::graphql::Error::builder()
                    .message(format!("http error: {}", parts.status))
                    // todo path: ["_entities", i, "???"]
                    .extension_code(format!("{}", parts.status.as_u16()))
                    .extension("connector", connector.display_name())
                    .build(),
            );
        }
    }

    // Apply the GraphQL operation's selection set to the result to handle
    // aliases and type names. Don't bother formatting if the response is
    // empty because that may produce confusing diagnostics.
    let data = if !data.is_empty() {
        let mut data = format_response(schema, &document, data, connector, diagnostics);

        // Before we return the response, we ensure that the entity data is under
        // the `_entities` key. The execution service knows how to merge that data
        // correctly (it doesn't know anything about "magic finder" fields).
        if result_uses_entities {
            if let Some(response_key) = original_magic_response_key.clone() {
                let entities = data.remove(&response_key).ok_or(ResponseFormattingError(
                    "could not handle _entities response key, this shouldn't happen".into(),
                ))?;
                data.insert(ENTITIES, entities);
            }
        }
        Some(Value::Object(data))
    } else {
        None
    };

    let response = SubgraphResponse::builder()
        .and_data(data)
        .errors(errors)
        .context(context)
        // .headers(parts.headers)
        .extensions(Object::default())
        .build();

    Ok(response)
}

/// Special handling for "magic_finder" fields:
///
/// The "inner" supergraph doesn't expose an `_entities` field. Instead we inject
/// fields like `_EntityName_finder` to query plan against. This
/// transformation happens in [`FetchNode::generate_connector_plan`].
///
/// In order to format the response according to the incoming client
/// operation, we need the operation to be valid against the inner supergraph.
/// This code does a simple string replacement if necessary and passes the
/// response key to the response formatting code.
fn handle_magic_finder(
    document: Option<String>,
    connector: &Connector,
    schema: &Valid<Schema>,
) -> Result<(Valid<ExecutableDocument>, Option<ByteString>, bool), BoxError> {
    let mut document = document.ok_or("missing document")?;
    let mut magic_response_key = None;
    let mut result_uses_entities = false;

    if let Some(finder_field_name) = connector.finder_field_name() {
        if document.contains("_entities") {
            result_uses_entities = true;
            document = document.replace("_entities", finder_field_name.as_str());
            magic_response_key = Some(finder_field_name);
        } else if document.contains(finder_field_name.as_str()) {
            magic_response_key = Some(finder_field_name);
        }
    }

    let document = ExecutableDocument::parse_and_validate(schema, document, "document.graphql")
        .map_err(|_| "failed to parse document")?;

    Ok((document, magic_response_key, result_uses_entities))
}

fn format_response(
    schema: &Valid<Schema>,
    document: &Valid<ExecutableDocument>,
    data: JsonMap,
    connector: &Connector,
    diagnostics: &mut Vec<Diagnostic>,
) -> JsonMap {
    let mut new_diagnostics = vec![];
    let result = execute(schema, document, new_diagnostics.as_mut(), data);
    for diagnostic in new_diagnostics {
        diagnostics.push(Diagnostic::from_response_formatting(connector, diagnostic));
    }
    result
}

fn inject_typename(data: &mut Value, typename: &str, key_type_map: &Option<KeyTypeMap>) {
    match data {
        Value::Array(data) => {
            for data in data {
                inject_typename(data, typename, key_type_map);
            }
        }
        Value::Object(data) => {
            if let Some(key_type_map) = key_type_map {
                let key = ByteString::from(key_type_map.key.clone());
                let discriminator = data
                    .get(&key)
                    .and_then(|val| val.as_str())
                    .map(|val| val.to_string())
                    .unwrap_or_default();

                for (typename, value) in key_type_map.type_map.iter() {
                    if value == &discriminator {
                        data.insert(
                            ByteString::from("__typename"),
                            Value::String(ByteString::from(typename.as_str())),
                        );
                    }
                }
            } else {
                data.insert(
                    ByteString::from("__typename"),
                    Value::String(ByteString::from(typename)),
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_compiler::validation::Valid;
    use apollo_compiler::Schema;
    use apollo_federation::sources::connect::JSONSelection;
    use apollo_federation::sources::connect::URLPathTemplate;
    use insta::assert_debug_snapshot;

    use crate::plugins::connectors::connector::Connector;
    use crate::plugins::connectors::directives::HTTPSource;
    use crate::plugins::connectors::directives::HTTPSourceAPI;
    use crate::plugins::connectors::directives::SourceAPI;
    use crate::plugins::connectors::directives::SourceField;
    use crate::Context;

    #[test]
    fn root_fields() {
        let schema = Arc::new(Schema::parse_and_validate(
            r#"
            scalar JSON
            type Query {
              a: String
              b(var: String): String
              c(var1: Int, var2: Boolean, var3: Float, var4: ID, var5: JSON, var6: [String], var7: String): String
            }
          "#,
            "test.graphql",
        ).unwrap());

        let req = crate::services::SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(
                        crate::graphql::Request::builder()
                            .query("query { a a2: a }")
                            .build(),
                    )
                    .expect("request builder"),
            )
            .build();

        assert_debug_snapshot!(super::root_fields(&req, schema.clone()), @r###"
        Ok(
            [
                (
                    RootField {
                        name: "a",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    RequestInputs {
                        arguments: {},
                        parent: {},
                    },
                ),
                (
                    RootField {
                        name: "a2",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    RequestInputs {
                        arguments: {},
                        parent: {},
                    },
                ),
            ],
        )
        "###);

        let req = crate::services::SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(
                        crate::graphql::Request::builder()
                            .query("query($var: String) { b(var: \"inline\") b2: b(var: $var) }")
                            .variables(
                                serde_json_bytes::json!({ "var": "variable" })
                                    .as_object()
                                    .unwrap()
                                    .clone(),
                            )
                            .build(),
                    )
                    .expect("request builder"),
            )
            .build();

        assert_debug_snapshot!(super::root_fields(&req, schema.clone()), @r###"
        Ok(
            [
                (
                    RootField {
                        name: "b",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    RequestInputs {
                        arguments: {
                            "var": String(
                                "inline",
                            ),
                        },
                        parent: {},
                    },
                ),
                (
                    RootField {
                        name: "b2",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    RequestInputs {
                        arguments: {
                            "var": String(
                                "variable",
                            ),
                        },
                        parent: {},
                    },
                ),
            ],
        )
        "###);

        let req = crate::services::SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(
                        crate::graphql::Request::builder()
                            .query(r#"
                              query(
                                $var1: Int, $var2: Bool, $var3: Float, $var4: ID, $var5: JSON, $var6: [String], $var7: String
                              ) {
                                c(var1: $var1, var2: $var2, var3: $var3, var4: $var4, var5: $var5, var6: $var6, var7: $var7)
                                c2: c(
                                  var1: 1,
                                  var2: true,
                                  var3: 0.9,
                                  var4: "123",
                                  var5: { a: 42 },
                                  var6: ["item"],
                                  var7: null
                                )
                              }
                            "#)
                            .variables(
                                serde_json_bytes::json!({
                                  "var1": 1, "var2": true, "var3": 0.9,
                                  "var4": "123", "var5": { "a": 42 }, "var6": ["item"],
                                  "var7": null
                                })
                                .as_object()
                                .unwrap()
                                .clone(),
                            )
                            .build(),
                    )
                    .expect("request builder"),
            )
            .build();

        assert_debug_snapshot!(super::root_fields(&req, schema.clone()), @r###"
        Ok(
            [
                (
                    RootField {
                        name: "c",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    RequestInputs {
                        arguments: {
                            "var1": Number(1),
                            "var2": Bool(
                                true,
                            ),
                            "var3": Number(0.9),
                            "var4": String(
                                "123",
                            ),
                            "var5": Object({
                                "a": Number(42),
                            }),
                            "var6": Array([
                                String(
                                    "item",
                                ),
                            ]),
                            "var7": Null,
                        },
                        parent: {},
                    },
                ),
                (
                    RootField {
                        name: "c2",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    RequestInputs {
                        arguments: {
                            "var1": Number(1),
                            "var2": Bool(
                                true,
                            ),
                            "var3": Number(0.9),
                            "var4": String(
                                "123",
                            ),
                            "var5": Object({
                                "a": Number(42),
                            }),
                            "var6": Array([
                                String(
                                    "item",
                                ),
                            ]),
                            "var7": Null,
                        },
                        parent: {},
                    },
                ),
            ],
        )
        "###);
    }

    #[test]
    fn entities_with_fields_from_request() {
        let partial_sdl = r#"
        type Query {
          field: String
        }

        type Entity {
          field: String
        }
        "#;
        let schema = Arc::new(Schema::parse_and_validate(partial_sdl, "test.graphql").unwrap());

        let req = crate::services::SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .body(
                        crate::graphql::Request::builder()
                            .query(
                                r#"
                              query($representations: [_Any!]!) {
                                _entities(representations: $representations) {
                                  __typename
                                  ... on Entity {
                                    field
                                    alias: field
                                  }
                                }
                              }
                            "#,
                            )
                            .variables(
                                serde_json_bytes::json!({
                                  "representations": [
                                      { "__typename": "User", "id": "1" },
                                      { "__typename": "User", "id": "2" },
                                  ]
                                })
                                .as_object()
                                .unwrap()
                                .clone(),
                            )
                            .build(),
                    )
                    .expect("request builder"),
            )
            .build();

        let api = SourceAPI {
            graph: "B".to_string(),
            name: Arc::new("API".to_string()),
            http: Some(HTTPSourceAPI {
                base_url: "http://localhost/api".to_string(),
                default: true,
                headers: vec![],
            }),
        };

        let directive = SourceField {
            graph: Arc::new("B".to_string()),
            parent_type_name: name!("Entity"),
            field_name: name!("field"),
            output_type_name: name!("String"),
            api: "API".to_string(),
            http: Some(HTTPSource {
                method: http::Method::GET,
                path_template: URLPathTemplate::parse("/path").unwrap(),
                body: None,
                headers: vec![],
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            on_interface_object: false,
        };

        let connector = Connector::new_from_source_field(
            Arc::new("CONNECTOR_QUERY_FIELDB".to_string()),
            &api,
            directive,
        )
        .unwrap();

        assert_debug_snapshot!(super::entities_with_fields_from_request(&req, schema.clone(), &connector).unwrap(), @r###"
        [
            (
                EntityField {
                    index: 0,
                    field_name: "field",
                    typename: Concrete(
                        "Entity",
                    ),
                },
                RequestInputs {
                    arguments: {},
                    parent: {
                        "__typename": String(
                            "User",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 1,
                    field_name: "field",
                    typename: Concrete(
                        "Entity",
                    ),
                },
                RequestInputs {
                    arguments: {},
                    parent: {
                        "__typename": String(
                            "User",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 0,
                    field_name: "alias",
                    typename: Concrete(
                        "Entity",
                    ),
                },
                RequestInputs {
                    arguments: {},
                    parent: {
                        "__typename": String(
                            "User",
                        ),
                        "id": String(
                            "1",
                        ),
                    },
                },
            ),
            (
                EntityField {
                    index: 1,
                    field_name: "alias",
                    typename: Concrete(
                        "Entity",
                    ),
                },
                RequestInputs {
                    arguments: {},
                    parent: {
                        "__typename": String(
                            "User",
                        ),
                        "id": String(
                            "2",
                        ),
                    },
                },
            ),
        ]
        "###);
    }

    #[test]
    fn make_requests() {
        let req = crate::services::SubgraphRequest::fake_builder()
            .subgraph_name("CONNECTOR_0")
            .subgraph_request(
                http::Request::builder()
                    .body(
                        crate::graphql::Request::builder()
                            .query("query { a: hello }")
                            .build(),
                    )
                    .expect("request builder"),
            )
            .build();

        let schema = Schema::parse_and_validate(
            r#"
              type Query {
                hello: String
              }
            "#,
            "test.graphql",
        )
        .unwrap();

        let api = SourceAPI {
            graph: "B".to_string(),
            name: Arc::new("API".to_string()),
            http: Some(HTTPSourceAPI {
                base_url: "http://localhost/api".to_string(),
                default: true,
                headers: vec![],
            }),
        };

        let directive = SourceField {
            graph: Arc::new("B".to_string()),
            parent_type_name: name!("Query"),
            field_name: name!("field"),
            output_type_name: name!("String"),
            api: "API".to_string(),
            http: Some(HTTPSource {
                method: http::Method::GET,
                path_template: URLPathTemplate::parse("/path").unwrap(),
                body: None,
                headers: vec![],
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            on_interface_object: false,
        };

        let connector =
            Connector::new_from_source_field(Arc::new("CONNECTOR_0".to_string()), &api, directive)
                .unwrap();

        let requests = super::make_requests(req, &connector, Arc::new(schema)).unwrap();

        assert_debug_snapshot!(requests, @r###"
        [
            (
                Request {
                    method: GET,
                    uri: http://localhost/api/path,
                    version: HTTP/1.1,
                    headers: {
                        "content-type": "application/json",
                    },
                    body: Body(
                        Empty,
                    ),
                },
                ResponseParams {
                    key: RootField {
                        name: "a",
                        typename: Concrete(
                            "String",
                        ),
                    },
                    source_api_name: "API",
                },
            ),
        ]
        "###);

        assert_debug_snapshot!(requests.first().unwrap().1, @r###"
        ResponseParams {
            key: RootField {
                name: "a",
                typename: Concrete(
                    "String",
                ),
            },
            source_api_name: "API",
        }
        "###);
    }

    #[tokio::test]
    async fn handle_requests() {
        let api = SourceAPI {
            graph: "B".to_string(),
            name: Arc::new("API".to_string()),
            http: Some(HTTPSourceAPI {
                base_url: "http://localhost/api".to_string(),
                default: true,
                headers: vec![],
            }),
        };

        let source_api_name = Arc::new("API".to_string());

        let directive = SourceField {
            graph: Arc::new("B".to_string()),
            parent_type_name: name!("Query"),
            field_name: name!("field"),
            output_type_name: name!("String"),
            api: "API".to_string(),
            http: Some(HTTPSource {
                method: http::Method::GET,
                path_template: URLPathTemplate::parse("/path").unwrap(),
                body: None,
                headers: vec![],
            }),
            selection: JSONSelection::parse(".data").unwrap().1,
            on_interface_object: false,
        };

        let connector = Connector::new_from_source_field(
            Arc::new("CONNECTOR_QUERY_FIELDB".to_string()),
            &api,
            directive,
        )
        .unwrap();

        let response1 = http::Response::builder()
            .extension(super::ResponseParams {
                key: super::ResponseKey::RootField {
                    name: "hello".to_string(),
                    typename: super::ResponseTypeName::Concrete("String".to_string()),
                },
                source_api_name: Arc::clone(&source_api_name),
            })
            .body(hyper::Body::from(r#"{"data":"world"}"#))
            .expect("response builder");

        let response2 = http::Response::builder()
            .extension(super::ResponseParams {
                key: super::ResponseKey::RootField {
                    name: "hello2".to_string(),
                    typename: super::ResponseTypeName::Concrete("String".to_string()),
                },
                source_api_name: Arc::clone(&source_api_name),
            })
            .body(hyper::Body::from(r#"{"data":"world"}"#))
            .expect("response builder");

        let schema = Schema::parse_and_validate(
            "
            type Query {
                hello: String
            }
            ",
            "schema.graphql",
        )
        .unwrap();

        let res = super::handle_responses(
            &schema,
            Some("{hello hello2: hello}".to_string()),
            Context::default(),
            &connector,
            vec![response1, response2],
            Vec::new().as_mut(),
        )
        .await
        .unwrap();

        assert_debug_snapshot!(res.response.body(), @r###"
        Response {
            label: None,
            data: Some(
                Object({
                    "hello": String(
                        "world",
                    ),
                    "hello2": String(
                        "world",
                    ),
                }),
            ),
            path: None,
            errors: [],
            extensions: {},
            has_next: None,
            subscribed: None,
            created_at: None,
            incremental: [],
        }
        "###);
    }

    async fn handle_requests_helper(
        schema: &Valid<Schema>,
        operation: String,
        selection: &str,
        responses: Vec<http::Response<hyper::Body>>,
    ) -> (crate::services::SubgraphResponse, Vec<super::Diagnostic>) {
        let connector = Connector::new_from_source_field(
            Arc::new("CONNECTOR_QUERY_FIELDB".to_string()),
            &SourceAPI {
                graph: "B".to_string(),
                name: Arc::from("API".to_string()),
                http: Some(HTTPSourceAPI {
                    base_url: "http://localhost/api".to_string(),
                    default: true,
                    headers: vec![],
                }),
            },
            SourceField {
                graph: Arc::new("B".to_string()),
                parent_type_name: name!("Query"),
                field_name: name!("field"),
                output_type_name: name!("String"),
                api: "API".to_string(),
                http: Some(HTTPSource {
                    method: http::Method::GET,
                    path_template: URLPathTemplate::parse("/path").unwrap(),
                    body: None,
                    headers: vec![],
                }),
                selection: JSONSelection::parse(selection).unwrap().1,
                on_interface_object: false,
            },
        )
        .unwrap();

        let mut diagnostics = Vec::new();
        let res = super::handle_responses(
            schema,
            Some(operation),
            Context::default(),
            &connector,
            responses,
            &mut diagnostics,
        )
        .await
        .unwrap();

        (res, diagnostics)
    }

    #[tokio::test]
    async fn test_response_diagnostics() {
        let (_, diagnostics) = handle_requests_helper(
            &Schema::parse_and_validate(
                "type Query { field: [T] }
                type T { a: Int b: Int c: Int d: D f: Int i: Int j: Int }
                type D { e: Int }",
                "schema.graphql",
            )
            .unwrap(),
            "query { field { a b c d { e } f i j } }".to_string(),
            "a
            b
            c             # missing
            d { e }       # wrong type
            f             # wrong type
            # h           # unused — TODO does not result in a diagnostic
            i: .i.ii.iii  # missing
            j: .j.jj.jjj  # wrong type",
            vec![http::Response::builder()
                .extension(super::ResponseParams {
                    key: super::ResponseKey::RootField {
                        name: "field".to_string(),
                        typename: super::ResponseTypeName::Concrete("T".to_string()),
                    },
                    source_api_name: Arc::from("API".to_string()),
                })
                .body(hyper::Body::from(
                    r#"[
                        {"a": 1, "b": 2, "d": 4, "f": { "g": 7 }, "h": 8, "i": { "ii": { "iii": 9 } }, "j": { "jj": { "jjj": 10 } } },
                        {"a": 1, "b": 2, "d": 4, "f": { "g": 7 }, "h": 8, "i": { "ii": { "xxx": 9 } }, "j": 10 }
                    ]"#,
                ))
                .expect("response builder")],
        )
        .await;

        assert_debug_snapshot!(diagnostics, @r###"
        [
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Property .c not found in object",
                path: "c",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Property .e not found in number",
                path: "d.e",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Property .c not found in object",
                path: "c",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Property .e not found in number",
                path: "d.e",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Property .iii not found in object",
                path: "i.ii.iii",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Property .jj not found in number",
                path: "j.jj",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Type Int is not a composite type",
                path: "field.0.f",
            },
            Response {
                connector: "[B] Query.field @sourceField(api: API, http: { GET: /path })",
                message: "Type Int is not a composite type",
                path: "field.1.f",
            },
        ]
        "###);
    }
}

mod graphql_utils {
    use apollo_compiler::ast;
    use apollo_compiler::ast::Definition;
    use apollo_compiler::executable::Field;
    use apollo_compiler::schema::Value;
    use apollo_compiler::Node;
    use serde_json::Number;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Map;
    use serde_json_bytes::Value as JSONValue;
    use tower::BoxError;

    use super::MakeRequestError;

    pub(super) fn field_arguments_map(
        field: &Node<Field>,
        variables: &Map<ByteString, JSONValue>,
    ) -> Result<Map<ByteString, JSONValue>, BoxError> {
        let mut arguments = Map::new();
        for argument in field.arguments.iter() {
            match &*argument.value {
                apollo_compiler::schema::Value::Variable(name) => {
                    if let Some(value) = variables.get(name.as_str()) {
                        arguments.insert(argument.name.as_str(), value.clone());
                    }
                }
                _ => {
                    arguments.insert(
                        argument.name.as_str(),
                        argument_value_to_json(&argument.value)?,
                    );
                }
            }
        }
        Ok(arguments)
    }

    pub(super) fn ast_field_arguments_map(
        field: &apollo_compiler::Node<apollo_compiler::ast::Field>,
        variables: &Map<ByteString, JSONValue>,
    ) -> Result<Map<ByteString, JSONValue>, BoxError> {
        let mut arguments = Map::new();
        for argument in field.arguments.iter() {
            match &*argument.value {
                apollo_compiler::schema::Value::Variable(name) => {
                    if let Some(value) = variables.get(name.as_str()) {
                        arguments.insert(argument.name.as_str(), value.clone());
                    }
                }
                _ => {
                    arguments.insert(
                        argument.name.as_str(),
                        argument_value_to_json(&argument.value)?,
                    );
                }
            }
        }
        Ok(arguments)
    }

    pub(super) fn argument_value_to_json(
        value: &apollo_compiler::ast::Value,
    ) -> Result<JSONValue, BoxError> {
        match value {
            Value::Null => Ok(JSONValue::Null),
            Value::Enum(e) => Ok(JSONValue::String(e.as_str().into())),
            Value::Variable(_) => Err(BoxError::from("variables not supported")),
            Value::String(s) => Ok(JSONValue::String(s.as_str().into())),
            Value::Float(f) => Ok(JSONValue::Number(
                Number::from_f64(
                    f.try_to_f64()
                        .map_err(|_| BoxError::from("try_to_f64 failed"))?,
                )
                .ok_or_else(|| BoxError::from("Number::from_f64 failed"))?,
            )),
            Value::Int(i) => Ok(JSONValue::Number(Number::from(
                i.try_to_i32().map_err(|_| "invalid int")?,
            ))),
            Value::Boolean(b) => Ok(JSONValue::Bool(*b)),
            Value::List(l) => Ok(JSONValue::Array(
                l.iter()
                    .map(|v| argument_value_to_json(v))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Value::Object(o) => Ok(JSONValue::Object(
                o.iter()
                    .map(|(k, v)| argument_value_to_json(v).map(|v| (k.as_str().into(), v)))
                    .collect::<Result<Map<_, _>, _>>()?,
            )),
        }
    }

    pub(super) fn get_entity_fields(
        query: &str,
    ) -> Result<(Node<ast::Field>, bool), MakeRequestError> {
        use MakeRequestError::*;

        // Use the AST because the `_entities` field is not actually present in the supergraph
        let doc = apollo_compiler::ast::Document::parse(query, "op.graphql")
            .map_err(|_| InvalidOperation("cannot parse operation document".into()))?;

        // Assume a single operation (because this is from a query plan)
        let op = doc
            .definitions
            .into_iter()
            .find_map(|d| match d {
                Definition::OperationDefinition(op) => Some(op),
                _ => None,
            })
            .ok_or_else(|| InvalidOperation("missing operation".into()))?;

        let root_field = op
            .selection_set
            .iter()
            .find_map(|s| match s {
                apollo_compiler::ast::Selection::Field(f) => Some(f),
                _ => None,
            })
            .ok_or_else(|| InvalidOperation("missing entities root field".into()))?;

        let mut typename_requested = false;

        for selection in root_field.selection_set.iter() {
            match selection {
                apollo_compiler::ast::Selection::Field(f) => {
                    if f.name == "__typename" {
                        typename_requested = true;
                    }
                }
                apollo_compiler::ast::Selection::FragmentSpread(_) => {
                    return Err(UnsupportedOperation("fragment spread not supported".into()))
                }
                apollo_compiler::ast::Selection::InlineFragment(f) => {
                    for selection in f.selection_set.iter() {
                        match selection {
                            apollo_compiler::ast::Selection::Field(f) => {
                                if f.name == "__typename" {
                                    typename_requested = true;
                                }
                            }
                            apollo_compiler::ast::Selection::FragmentSpread(_) => {
                                return Err(UnsupportedOperation(
                                    "fragment spread not supported".into(),
                                ))
                            }
                            apollo_compiler::ast::Selection::InlineFragment(_) => {
                                return Err(UnsupportedOperation(
                                    "inline fragment not supported".into(),
                                ))
                            }
                        }
                    }
                }
            }
        }

        Ok((root_field.clone(), typename_requested))
    }
}
