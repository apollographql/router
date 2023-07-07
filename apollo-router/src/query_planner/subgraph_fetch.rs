use std::collections::{HashMap};
use std::sync::Arc;

use indexmap::IndexSet;
use serde::Deserialize;
use serde::Serialize;
use tower::ServiceExt;
use tracing::instrument;
use tracing::Instrument;

use super::fetch::OperationKind;
use super::execution::ExecutionParameters;
use super::rewrites;
use super::selection::select_object;
use super::selection::Selection;
use crate::error::Error;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::{Request,SingleRequest};
use crate::http_ext;
use crate::json_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::services::SubgraphRequest;
use crate::spec::Schema;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchInput {
    pub(crate) selected_type: String,
    pub(crate) selections: Vec<Selection>,
    pub(crate) variable_name: String,
}

/// A fetch node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubgraphFetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    pub(crate) service_name: String,

    /// The variables that are used for the subgraph fetch.
    pub(crate) variable_usages: Vec<String>,

    /// The GraphQL subquery that is used for the fetch.
    pub(crate) operation: String,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<String>,

    /// The GraphQL operation kind that is used for the fetch.
    #[serde(default)]
    pub(crate) operation_kind: OperationKind,

    /// Optional id used by Deferred nodes
    pub(crate) id: Option<String>,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this fetch.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a fetch (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,
    
    /// Inputs to the finder fetch
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub(crate) inputs: Vec<FetchInput>,
}

pub(crate) struct SubgraphVariables {
    pub(crate) variables: IndexSet<Object>,
    pub(crate) paths: HashMap<Path, usize>,
}

impl SubgraphVariables {
    #[instrument(skip_all, level = "debug", name = "make_variables")]
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn new(
        input: &FetchInput,
        variable_usages: &[String],
        data: &Value,
        current_dir: &Path,
        request: &Arc<http::Request<Request>>,
        schema: &Schema,
        input_rewrites: &Option<Vec<rewrites::DataRewrite>>,
    ) -> Option<SubgraphVariables> {
        let body = request.body();

        let mut variables: IndexSet<Object> = IndexSet::new();
        let mut paths: HashMap<Path, usize> = HashMap::new();
        
        // let mut variables = Object::with_capacity(1 + variable_usages.len());

        // variables.extend(variable_usages.iter().filter_map(|key| {
        //     body.variables
        //         .get_key_value(key.as_str())
        //         .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
        // }));

        data.select_values_and_paths(schema, current_dir, |path, value| {
            if let serde_json_bytes::Value::Object(content) = value {
                if let Ok(Some(mut value)) = select_object(content, input.selections.as_ref(), schema) {
                    rewrites::apply_rewrites(schema, &mut value, input_rewrites);
                    match value {
                        serde_json_bytes::Value::Object(obj) => {
                            let mut vars = Object::with_capacity(1);
                            if let Some(v) = obj.get(input.variable_name.as_str()).cloned() {
                                paths.insert(path.clone(), variables.len());
                                vars.insert(input.variable_name.as_str(), v);
                                variables.insert(vars.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        });

        Some(SubgraphVariables { variables, paths })
    }
}

impl SubgraphFetchNode {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_node<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        data: &'a Value,
        current_dir: &'a Path,
    ) -> Result<(Value, Vec<Error>), FetchError> {
        let SubgraphFetchNode {
            operation,
            operation_kind,
            operation_name,
            service_name,
            ..
        } = self;
        
        assert!(*operation_kind == OperationKind::Query);
        assert!(self.inputs.len() == 1);
        let input = self.inputs
            .get(0)
            .unwrap();
        
        let SubgraphVariables { variables, paths } = match SubgraphVariables::new(
            input,
            self.variable_usages.as_ref(),
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request,
            parameters.schema,
            &self.input_rewrites,
        )
        .await
        {
            Some(variables) => variables,
            None => {
                return Ok((Value::Object(Object::default()), Vec::new()));
            }
        };
            
        let zz = Request::BatchRequest([SingleRequest::builder()
            .query(operation)
            .and_operation_name(operation_name.clone())
            .variables(variables.get_index(0).unwrap().clone())
            .build(),
        ].to_vec());
        
        let subgraph_request = SubgraphRequest::builder()
            .supergraph_request(parameters.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(
                        parameters
                            .schema
                            .subgraphs()
                            .find_map(|(name, url)| (name == service_name).then_some(url))
                            .unwrap_or_else(|| {
                                panic!(
                                    "schema uri for subgraph '{service_name}' should already have been checked"
                                )
                            })
                            .clone(),
                    )
                    .body(zz)
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

        super::log::trace_subfetch(service_name, operation, &variables.get_index(0).unwrap(), &response);

        if !response.is_primary() {
            return Err(FetchError::SubrequestUnexpectedPatchResponse {
                service: service_name.to_owned(),
            });
        }
        let (value, errors) =
            self.response_at_path(parameters.schema, current_dir, paths, response);
        if let Some(id) = &self.id {
            if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                }
            }
        }
        Ok((value, errors))
    }

    // #[instrument(skip_all, level = "debug", name = "response_insert")]
    // fn response_at_path<'a>(
    //     &'a self,
    //     schema: &Schema,
    //     current_dir: &'a Path,
    //     paths: HashMap<Path, usize>,
    //     response: graphql::Response,
    // ) -> (Value, Vec<Error>) {
    //     // for each entity in the response, find out the path where it must be inserted
    //     let mut inverted_paths: HashMap<usize, Vec<&Path>> = HashMap::new();
    //     for (path, index) in paths.iter() {
    //         (*inverted_paths.entry(*index).or_default()).push(path);
    //     }

    //     let current_slice = if current_dir.last() == Some(&json_ext::PathElement::Flatten) {
    //         &current_dir.0[..current_dir.0.len() - 1]
    //     } else {
    //         &current_dir.0[..]
    //     };

    //     let errors: Vec<Error> = response
    //         .errors
    //         .into_iter()
    //         .map(|error| {
    //             let path = error.path.as_ref().map(|path| {
    //                 Path::from_iter(current_slice.iter().chain(path.iter()).cloned())
    //             });

    //             Error {
    //                 locations: error.locations,
    //                 path,
    //                 message: error.message,
    //                 extensions: error.extensions,
    //             }
    //         })
    //         .collect();
    //     let mut data = response.data.unwrap_or_default().get("getUser").unwrap().clone();
    //     rewrites::apply_rewrites(schema, &mut data, &self.output_rewrites);
    //     (Value::from_path(current_dir, data), errors)
    // }
    #[instrument(skip_all, level = "debug", name = "response_insert")]
    fn response_at_path<'a>(
        &'a self,
        schema: &Schema,
        current_dir: &'a Path,
        paths: HashMap<Path, usize>,
        response: graphql::Response,
    ) -> (Value, Vec<Error>) {
        // for each entity in the response, find out the path where it must be inserted
        let mut inverted_paths: HashMap<usize, Vec<&Path>> = HashMap::new();
        for (path, index) in paths.iter() {
            (*inverted_paths.entry(*index).or_default()).push(path);
        }
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
                                inverted_paths.get(i).iter().flat_map(|v| v.iter())
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
            if let Some(entities) = map.remove("getUser") {
                tracing::trace!("received entities: {:?}", &entities);

                let mut value = Value::default();
                for (path, entity_idx) in paths {
                    let mut data = entities.clone();
                    rewrites::apply_rewrites(schema, &mut data, &self.output_rewrites);
                    let _ = value.insert(&path, data);
                }
                return (value, errors);
            }
        }

        errors.push(
            Error::builder()
                .path(current_dir.clone())
                .message(format!(
                    "Subgraph response from '{}' was missing key `_entities`",
                    self.service_name
                ))
                .extension_code("PARSE_ERROR")
                .build(),
        );

        (Value::Null, errors)

    }


    #[cfg(test)]
    pub(crate) fn service_name(&self) -> &str {
        &self.service_name
    }

    pub(crate) fn operation_kind(&self) -> &OperationKind {
      &self.operation_kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello() {
        let plan_str =
        r#"
        { "kind": "SubgraphFetch", "serviceName": "user", "operationName": "ExampleQuery__user__1", "operation": "query ExampleQuery__user__1($id:ID!){getUser(id:$id){...on User{firstName lastName}}}", "variableUsages": ["getUser"], "inputs": [ { "selectedType": "ID!", "selections": [ { "kind": "InlineFragment", "typeCondition": "User", "selections": [ { "kind": "Field", "name": "__typename" }, { "kind": "Field", "name": "id" } ] } ], "variableName": "id" } ] }
        "#;
        let result = serde_json::from_str::<SubgraphFetchNode>(plan_str);
        
        assert_eq!(2 + 2, 4);
    }
}