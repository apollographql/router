use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use futures::Future;
use http::Uri;
use static_assertions::assert_impl_all;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tracing::instrument;
use tracing::Instrument;

use super::http::HttpClientServiceFactory;
use super::SubgraphRequest;
use super::SubgraphServiceFactory;
use crate::error::FetchError;
use crate::graphql;
use crate::graphql::Error;
use crate::http_ext;
use crate::json_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::query_planner;
use crate::query_planner::fetch::FetchNode;
use crate::query_planner::fetch::Variables;
use crate::query_planner::rewrites;
use crate::services::http::HttpRequest;
use crate::spec::Schema;
use crate::Context;

pub(crate) type BoxService = tower::util::BoxService<Request, Response, BoxError>;

assert_impl_all!(Request: Send);
#[non_exhaustive]
pub(crate) struct Request {
    pub(crate) fetch_node: FetchNode,

    pub(crate) context: Context,

    pub(crate) schema: Arc<Schema>,

    pub(crate) supergraph_request: Arc<http::Request<graphql::Request>>,

    pub(crate) data: Value,

    pub(crate) current_dir: Path,
}

pub(crate) type Response = (Value, Vec<crate::graphql::Error>);

#[derive(Clone)]
pub(crate) struct FetchService {
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,

    pub(crate) http_client_service_factory: Arc<HttpClientServiceFactory>,
}

impl Service<Request> for FetchService {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let clone = self.clone();
        let mut this = std::mem::replace(self, clone);

        let fut = async move { Ok(this.call_inner(req).await) }.in_current_span();
        Box::pin(fut)
    }
}

impl FetchService {
    async fn call_inner(&mut self, req: Request) -> Response {
        use query_planner::fetch::sources::SourceId;
        match req.fetch_node.source_id {
            SourceId::Graphql(_) => self.handle_graphql(req).await,
            SourceId::Connect(_) => {
                // THIS IS WHERE THE MAGIC HAPPENS
                let http_client = self.http_client_service_factory.create("TODO");

                let _response = http_client
                    .oneshot(HttpRequest {
                        http_request: http::Request::builder()
                            .method(http::Method::GET)
                            .uri("http://localhost:8080".parse::<Uri>().unwrap())
                            .body(Default::default())
                            .unwrap(),
                        context: req.context.clone(),
                    })
                    .await;

                todo!()
            }
        }
    }

    async fn handle_graphql(&mut self, req: Request) -> Response {
        println!("FetchService::call_inner");
        let FetchNode {
            operation,
            operation_kind,
            operation_name,
            service_name,
            requires,
            variable_usages,
            input_rewrites,
            ..
        } = &req.fetch_node;

        let Variables {
            variables,
            inverted_paths: paths,
        } = match Variables::new(
            requires,
            variable_usages,
            &req.data,
            &req.current_dir,
            // Needs the original request here
            &req.supergraph_request,
            &req.schema,
            input_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return (Value::Object(Object::default()), Vec::new());
            }
        };

        let mut subgraph_request = SubgraphRequest::builder()
            .supergraph_request(req.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(
                        req
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
                        graphql::Request::builder()
                            .query(operation.as_serialized())
                            .and_operation_name(operation_name.as_ref().map(|n| n.to_string()))
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .subgraph_name(service_name.to_string())
            .operation_kind(*operation_kind)
            .context(req.context.clone())
            .build();
        subgraph_request.query_hash = req.fetch_node.schema_aware_hash.clone();
        subgraph_request.authorization = req.fetch_node.authorization.clone();

        let service = self
            .subgraph_service_factory
            .create(service_name)
            .expect("we already checked that the service exists during planning; qed");

        let (_parts, response) = match service
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
            }) {
            Err(e) => {
                return (
                    Value::default(),
                    vec![e.to_graphql_error(Some(req.current_dir.to_owned()))],
                );
            }
            Ok(res) => res.response.into_parts(),
        };

        query_planner::log::trace_subfetch(
            service_name,
            operation.as_serialized(),
            &variables,
            &response,
        );

        if !response.is_primary() {
            return (
                Value::default(),
                vec![FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_string(),
                }
                .to_graphql_error(Some(req.current_dir.to_owned()))],
            );
        }

        self.response_at_path(
            &req.fetch_node,
            &req.schema,
            &req.current_dir,
            paths,
            response,
        )
    }

    #[instrument(skip_all, level = "debug", name = "response_insert")]
    fn response_at_path<'a>(
        &'a self,
        fetch_node: &FetchNode,
        schema: &Schema,
        current_dir: &'a Path,
        inverted_paths: Vec<Vec<Path>>,
        response: graphql::Response,
    ) -> (Value, Vec<Error>) {
        if !fetch_node.requires.is_empty() {
            let entities_path = Path(vec![json_ext::PathElement::Key(
                "_entities".to_string(),
                None,
            )]);

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
                            rewrites::apply_rewrites(
                                schema,
                                &mut entity,
                                &fetch_node.output_rewrites,
                            );

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
                    fetch_node.service_name
                );
            }

            (Value::Null, errors)
        } else {
            let current_slice =
                if matches!(current_dir.last(), Some(&json_ext::PathElement::Flatten(_))) {
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
            rewrites::apply_rewrites(schema, &mut data, &fetch_node.output_rewrites);
            (Value::from_path(current_dir, data), errors)
        }
    }
}

pub(crate) struct FetchServiceFactory {
    pub(crate) subgraph_service_factory: Arc<SubgraphServiceFactory>,

    pub(crate) http_client_service_factory: Arc<HttpClientServiceFactory>,
}

impl FetchServiceFactory {
    pub(crate) fn create(&self) -> BoxService {
        ServiceBuilder::new()
            .service(FetchService {
                subgraph_service_factory: self.subgraph_service_factory.clone(),
                http_client_service_factory: self.http_client_service_factory.clone(),
            })
            .boxed()
    }
}

// impl ServiceFactory<Request> for FetchServiceFactory {
//     type Service = BoxService;

//     fn create(&self) -> Self::Service {
//         ServiceBuilder::new()
//             .service(FetchService {
//                 subgraph_service_factory: self.subgraph_service_factory.clone(),
//             })
//             .boxed()
//     }
// }
