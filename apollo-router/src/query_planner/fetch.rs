use std::fmt::Display;
use std::sync::Arc;

use indexmap::IndexSet;
use serde::Deserialize;
use serde::Serialize;
use tower::ServiceExt;
use tracing::instrument;
use tracing::Instrument;

use super::execution::ExecutionParameters;
use super::rewrites;
use super::selection::select_object;
use super::selection::Selection;
use crate::error::Error;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Request;
use crate::http_ext;
use crate::json_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::services::SubgraphRequest;
use crate::spec::Schema;

/// GraphQL operation type.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum OperationKind {
    #[default]
    Query,
    Mutation,
    Subscription,
}

impl Display for OperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl OperationKind {
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            OperationKind::Query => "Query",
            OperationKind::Mutation => "Mutation",
            OperationKind::Subscription => "Subscription",
        }
    }

    /// Only for apollo studio exporter
    pub(crate) const fn as_apollo_operation_type(&self) -> &'static str {
        match self {
            OperationKind::Query => "query",
            OperationKind::Mutation => "mutation",
            OperationKind::Subscription => "subscription",
        }
    }
}

/// A fetch node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    pub(crate) service_name: String,

    /// The data that is required for the subgraph fetch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub(crate) requires: Vec<Selection>,

    /// The variables that are used for the subgraph fetch.
    pub(crate) variable_usages: Vec<String>,

    /// The GraphQL subquery that is used for the fetch.
    pub(crate) operation: String,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<String>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    /// Optional id used by Deferred nodes
    pub(crate) id: Option<String>,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this fetch.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a fetch (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,
}

pub(crate) struct Variables {
    pub(crate) variables: Object,
    pub(crate) inverted_paths: Vec<Vec<Path>>,
}

impl Variables {
    #[instrument(skip_all, level = "debug", name = "make_variables")]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        requires: &[Selection],
        variable_usages: &[String],
        data: &Value,
        current_dir: &Path,
        request: &Arc<http::Request<Request>>,
        schema: &Schema,
        input_rewrites: &Option<Vec<rewrites::DataRewrite>>,
    ) -> Option<Variables> {
        let body = request.body();
        if !requires.is_empty() {
            let mut variables = Object::with_capacity(1 + variable_usages.len());

            variables.extend(variable_usages.iter().filter_map(|key| {
                body.variables
                    .get_key_value(key.as_str())
                    .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
            }));

            let mut inverted_paths: Vec<Vec<Path>> = Vec::new();
            let mut values: IndexSet<Value> = IndexSet::new();

            data.select_values_and_paths(schema, current_dir, |path, value| {
                if let Value::Object(content) = value {
                    if let Ok(Some(mut value)) = select_object(content, requires, schema) {
                        rewrites::apply_rewrites(schema, &mut value, input_rewrites);
                        match values.get_index_of(&value) {
                            Some(index) => {
                                inverted_paths[index].push(path.clone());
                            }
                            None => {
                                inverted_paths.push(vec![path.clone()]);
                                values.insert(value);
                                debug_assert!(inverted_paths.len() == values.len());
                            }
                        }
                    }
                }
            });

            if values.is_empty() {
                return None;
            }

            let representations = Value::Array(Vec::from_iter(values));

            variables.insert("representations", representations);

            Some(Variables {
                variables,
                inverted_paths,
            })
        } else {
            // with nested operations (Query or Mutation has an operation returning a Query or Mutation),
            // when the first fetch fails, the query plan will still execute up until the second fetch,
            // where `requires` is empty (not a federated fetch), the current dir is not emmpty (child of
            // the previous operation field) and the data is null. In that case, we recognize that we
            // should not perform the next fetch
            if !current_dir.is_empty()
                && data
                    .get_path(schema, current_dir)
                    .map(|value| value.is_null())
                    .unwrap_or(true)
            {
                return None;
            }

            Some(Variables {
                variables: variable_usages
                    .iter()
                    .filter_map(|key| {
                        body.variables
                            .get_key_value(key.as_str())
                            .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                    })
                    .collect::<Object>(),
                inverted_paths: Vec::new(),
            })
        }
    }
}

impl FetchNode {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_node<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        data: &'a Value,
        current_dir: &'a Path,
    ) -> Result<(Value, Vec<Error>), FetchError> {
        let FetchNode {
            operation,
            operation_kind,
            operation_name,
            service_name,
            ..
        } = self;

        let Variables {
            variables,
            inverted_paths: paths,
        } = match Variables::new(
            &self.requires,
            self.variable_usages.as_ref(),
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request,
            parameters.schema,
            &self.input_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return Ok((Value::Object(Object::default()), Vec::new()));
            }
        };

        let subgraph_request = SubgraphRequest::builder()
            .supergraph_request(parameters.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(
                        parameters
                            .schema
                            .subgraph_url(service_name)
                            .unwrap_or_else(|| {
                                panic!(
                                    "schema uri for subgraph '{service_name}' should already have been checked"
                                )
                            })
                            .clone(),
                    )
                    .body(
                        Request::builder()
                            .query(operation)
                            .and_operation_name(operation_name.clone())
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .operation_kind(*operation_kind)
            .context(parameters.context.clone())
            .build();

        let service = parameters
            .service_factory
            .create(service_name)
            .expect("we already checked that the service exists during planning; qed");

        // TODO not sure if we need a RouterReponse here as we don't do anything with it
        let (_parts, response) = service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subfetch_stream"))
            .await
            // TODO this is a problem since it restores details about failed service
            // when errors have been redacted in the include_subgraph_errors module.
            // Unfortunately, not easy to fix here, because at this point we don't
            // know if we should be redacting errors for this subgraph...
            .map_err(|e| match e.downcast::<FetchError>() {
                Ok(inner) => match *inner {
                    FetchError::SubrequestHttpError { .. } => *inner,
                    _ => FetchError::SubrequestHttpError {
                        status_code: None,
                        service: service_name.to_string(),
                        reason: inner.to_string(),
                    },
                },
                Err(e) => FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.to_string(),
                    reason: e.to_string(),
                },
            })?
            .response
            .into_parts();

        super::log::trace_subfetch(service_name, operation, &variables, &response);

        if !response.is_primary() {
            return Err(FetchError::SubrequestUnexpectedPatchResponse {
                service: service_name.to_owned(),
            });
        }

        let (value, errors) =
            self.response_at_path(parameters.schema, current_dir, paths, response);
        if let Some(id) = &self.id {
            if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
                tracing::info!(monotonic_counter.apollo.router.operations.defer.fetch = 1u64);
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                }
            }
        }
        Ok((value, errors))
    }

    #[instrument(skip_all, level = "debug", name = "response_insert")]
    fn response_at_path<'a>(
        &'a self,
        schema: &Schema,
        current_dir: &'a Path,
        inverted_paths: Vec<Vec<Path>>,
        response: graphql::Response,
    ) -> (Value, Vec<Error>) {
        if !self.requires.is_empty() {
            let entities_path = Path(vec![json_ext::PathElement::Key("_entities".to_string())]);

            let mut errors: Vec<Error> = vec![];
            for mut error in response.errors {
                // the locations correspond to the subgraph query and cannot be linked to locations
                // in the client query, so we remove them
                error.locations = Vec::new();

                // errors with path should be updated to the path of the entity they target
                if let Some(ref path) = error.path {
                    if path.starts_with(&entities_path) {
                        // the error's path has the format '/_entities/1/other' so we ignore the
                        // first element and then get the index
                        match path.0.get(1) {
                            Some(json_ext::PathElement::Index(i)) => {
                                for values_path in
                                    inverted_paths.get(*i).iter().flat_map(|v| v.iter())
                                {
                                    errors.push(Error {
                                        locations: error.locations.clone(),
                                        // append to the entitiy's path the error's path without
                                        //`_entities` and the index
                                        path: Some(Path::from_iter(
                                            values_path.0.iter().chain(&path.0[2..]).cloned(),
                                        )),
                                        message: error.message.clone(),
                                        extensions: error.extensions.clone(),
                                    })
                                }
                            }
                            _ => {
                                error.path = Some(current_dir.clone());
                                errors.push(error)
                            }
                        }
                    } else {
                        error.path = Some(current_dir.clone());
                        errors.push(error);
                    }
                } else {
                    errors.push(error);
                }
            }

            // we have to nest conditions and do early returns here
            // because we need to take ownership of the inner value
            if let Some(Value::Object(mut map)) = response.data {
                if let Some(entities) = map.remove("_entities") {
                    tracing::trace!("received entities: {:?}", &entities);

                    if let Value::Array(array) = entities {
                        let mut value = Value::default();

                        for (index, mut entity) in array.into_iter().enumerate() {
                            rewrites::apply_rewrites(schema, &mut entity, &self.output_rewrites);

                            if let Some(paths) = inverted_paths.get(index) {
                                if paths.len() > 1 {
                                    for path in &paths[1..] {
                                        let _ = value.insert(path, entity.clone());
                                    }
                                }

                                if let Some(path) = paths.first() {
                                    let _ = value.insert(path, entity);
                                }
                            }
                        }
                        return (value, errors);
                    }
                }
            }

            // if we get here, it means that the response was missing the `_entities` key
            // This can happen if the subgraph failed during query execution e.g. for permissions checks.
            // In this case we should add an additional error because the subgraph should have returned an error that will be bubbled up to the client.
            // However, if they have not then print a warning to the logs.
            if errors.is_empty() {
                tracing::warn!(
                    "Subgraph response from '{}' was missing key `_entities` and had no errors. This is likely a bug in the subgraph.",
                    self.service_name
                );
            }

            (Value::Null, errors)
        } else {
            let current_slice = if current_dir.last() == Some(&json_ext::PathElement::Flatten) {
                &current_dir.0[..current_dir.0.len() - 1]
            } else {
                &current_dir.0[..]
            };

            let errors: Vec<Error> = response
                .errors
                .into_iter()
                .map(|error| {
                    let path = error.path.as_ref().map(|path| {
                        Path::from_iter(current_slice.iter().chain(path.iter()).cloned())
                    });

                    Error {
                        locations: error.locations,
                        path,
                        message: error.message,
                        extensions: error.extensions,
                    }
                })
                .collect();
            let mut data = response.data.unwrap_or_default();
            rewrites::apply_rewrites(schema, &mut data, &self.output_rewrites);
            (Value::from_path(current_dir, data), errors)
        }
    }

    #[cfg(test)]
    pub(crate) fn service_name(&self) -> &str {
        &self.service_name
    }

    pub(crate) fn operation_kind(&self) -> &OperationKind {
        &self.operation_kind
    }
}
