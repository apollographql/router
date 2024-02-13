use std::cmp;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::task;
use std::task::Poll;

use bytes::Bytes;
use futures::stream::StreamExt;
use futures::FutureExt;
use futures::Stream;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use indexmap::IndexMap;
use indexmap::IndexSet;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use mediatype::MediaType;
use mediatype::ReadParams;
use multer::Multipart;
use rand::RngCore;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::config::FileUploadsConfig;
use self::config::MultipartRequestLimits;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::file_uploads::error::FileUploadError;
use crate::query_planner::FlattenNode;
use crate::query_planner::PlanNode;
use crate::register_private_plugin;
use crate::services::execution;
use crate::services::execution::QueryPlan;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

mod config;
mod error;

type UploadResult<T> = Result<T, error::FileUploadError>;

// FIXME: check if we need to hide docs
#[doc(hidden)] // Only public for integration tests
struct FileUploadsPlugin {
    enabled: bool,
    limits: MultipartRequestLimits,
}

register_private_plugin!("apollo", "preview_file_uploads", FileUploadsPlugin);

#[async_trait::async_trait]
impl PluginPrivate for FileUploadsPlugin {
    type Config = FileUploadsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let config = init.config;
        let enabled = config.enabled && config.protocols.multipart.enabled;
        let limits = config.protocols.multipart.limits;
        Ok(Self { enabled, limits })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: router::Request| {
                async {
                    let context = req.context.clone();
                    Ok(match extract_operations(req).await {
                        Ok(req) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            router::Response::error_builder()
                                .errors(vec![err.into()])
                                .context(context)
                                .build()?,
                        ),
                    })
                }
                .boxed()
            })
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: supergraph::Request| {
                async {
                    let context = req.context.clone();
                    Ok(match extract_map(req).await {
                        Ok(req) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            supergraph::Response::error_builder()
                                .errors(vec![err.into()])
                                .context(context)
                                .build()?,
                        ),
                    })
                }
                .boxed()
            })
            .service(service)
            .boxed()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .checkpoint(|req: execution::Request| {
                Ok(ControlFlow::Continue(rearange_execution_plan(req)))
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: subgraph::Request| {
                call_subgraph(req)
                    .boxed()
                    .map(|req| Ok(ControlFlow::Continue(req)))
                    .boxed()
            })
            .service(service)
            .boxed()
    }
}

fn get_multipart_mime(req: &router::Request) -> Option<MediaType> {
    req.router_request
        .headers()
        .get(CONTENT_TYPE)
        // Ignore parsing error, since they are reported by content_negotiation layer.
        .and_then(|header| header.to_str().ok())
        .and_then(|str| MediaType::parse(str).ok())
        .filter(|mime| mime.ty == MULTIPART && mime.subty == FORM_DATA)
}

async fn extract_operations(req: router::Request) -> UploadResult<router::Request> {
    if let Some(mime) = get_multipart_mime(&req) {
        let boundary = mime
            .get_param(BOUNDARY)
            .ok_or_else(|| FileUploadError::InvalidMultipartRequest(multer::Error::NoBoundary))?
            .to_string();

        let (mut request_parts, request_body) = req.router_request.into_parts();
        let mut multipart = Multipart::new(request_body, boundary);

        let operations_field = multipart
            .next_field()
            .await?
            .filter(|field| field.name() == Some("operations"))
            .ok_or_else(|| FileUploadError::MissingOperationsField)?;

        req.context
            .extensions()
            .lock()
            .insert(ServiceLayerResult { multipart });

        let content_type = operations_field
            .headers()
            .get(CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("application/json"));
        request_parts.headers.insert(CONTENT_TYPE, content_type);
        request_parts.headers.remove(CONTENT_LENGTH);

        return Ok(router::Request::from((
            http::Request::from_parts(request_parts, hyper::Body::wrap_stream(operations_field)),
            req.context,
        )));
    }

    Ok(req)
}

struct ServiceLayerResult {
    multipart: Multipart<'static>,
}

async fn extract_map(mut req: supergraph::Request) -> UploadResult<supergraph::Request> {
    let service_layer_result = req
        .context
        .extensions()
        .lock()
        .remove::<ServiceLayerResult>();

    if let Some(ServiceLayerResult { mut multipart }) = service_layer_result {
        let map_field = multipart
            .next_field()
            .await?
            .filter(|field| field.name() == Some("map"))
            .ok_or_else(|| FileUploadError::MissingMapField)?;

        // FIXME: apply some limit on size of map field
        let map_field = map_field.bytes().await?;
        let map_field: MapField = serde_json::from_slice(&map_field)
            .map_err(|e| FileUploadError::InvalidJsonInMapField(e))?;
        // FIXME: check number of files
        // assert!(map_field.len());

        let variables = &mut req.supergraph_request.body_mut().variables;
        let mut files_order = IndexSet::new();
        let mut map_per_variable: MapPerVariable = HashMap::new();
        for (filename, paths) in map_field.into_iter() {
            for path in paths.into_iter() {
                let mut segments = path.split('.');
                let first_segment = segments.next();
                if first_segment != Some("variables") {
                    if first_segment
                        .and_then(|str| str.parse::<usize>().ok())
                        .is_some()
                    {
                        return Err(FileUploadError::BatchRequestAreNotSupported);
                    }
                    return Err(FileUploadError::InvalidPathInsideMapField(path));
                }
                let variable_name = segments.next().ok_or_else(|| {
                    FileUploadError::MissingVariableNameInsideMapField(path.clone())
                })?;
                let variable_path: Vec<String> = segments.map(|str| str.to_owned()).collect();

                // patch variables to pass validation
                let json_value = variables
                    .get_mut(variable_name)
                    .and_then(|root| try_path(root, &variable_path))
                    .ok_or_else(|| FileUploadError::InputValueNotFound(path.clone()))?;
                drop(core::mem::replace(
                    json_value,
                    serde_json_bytes::Value::String(
                        format!("<Placeholder for file '{}'>", filename).into(),
                    ),
                ));

                map_per_variable
                    .entry(variable_name.to_owned())
                    .or_insert_with(|| HashMap::new())
                    .entry(filename.clone())
                    .or_insert_with(|| Vec::new())
                    .push(variable_path);
            }
            files_order.insert(filename);
        }

        req.context
            .extensions()
            .lock()
            .insert(SupergraphLayerResult {
                multipart: Arc::new(Mutex::new(multipart)),
                map_per_variable,
                files_order,
            });
    }
    Ok(req)
}

fn try_path<'a>(
    root: &'a mut serde_json_bytes::Value,
    path: &'a Vec<String>,
) -> Option<&'a mut serde_json_bytes::Value> {
    path.iter().try_fold(root, |parent, segment| match parent {
        serde_json_bytes::Value::Object(map) => map.get_mut(segment.as_str()),
        serde_json_bytes::Value::Array(list) => segment
            .parse::<usize>()
            .ok()
            .and_then(move |x| list.get_mut(x)),
        _ => None,
    })
}

type MapField = IndexMap<String, Vec<String>>;
type MapPerVariable = HashMap<String, HashMap<String, Vec<Vec<String>>>>;

#[derive(Clone)]
struct SupergraphLayerResult {
    multipart: Arc<Mutex<Multipart<'static>>>,
    files_order: IndexSet<String>,
    map_per_variable: MapPerVariable,
}

fn rearange_execution_plan(mut req: execution::Request) -> execution::Request {
    let supergraph_result = req
        .context
        .extensions()
        .lock()
        .get::<SupergraphLayerResult>()
        .cloned();
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult {
            files_order,
            map_per_variable,
            ..
        } = supergraph_result;

        let root = &req.query_plan.root;
        let (_, root) = rearrange_plan_node(root, &files_order, &map_per_variable);
        req = execution::Request {
            query_plan: Arc::new(QueryPlan {
                root,
                usage_reporting: req.query_plan.usage_reporting.clone(),
                formatted_query_plan: req.query_plan.formatted_query_plan.clone(),
                query: req.query_plan.query.clone(),
            }),
            ..req
        };
    }
    req
}

// Recursive, and recursion is safe here since query plan is executed recursively.
fn rearrange_plan_node<'a>(
    node: &PlanNode,
    files_order: &IndexSet<String>,
    map_per_variable: &'a MapPerVariable,
) -> (HashSet<&'a str>, PlanNode) {
    match node {
        PlanNode::Condition {
            condition,
            if_clause,
            else_clause,
        } => {
            let mut files = HashSet::new();
            let if_clause = if_clause.as_ref().map(|node| {
                let (node_files, node) = rearrange_plan_node(node, files_order, map_per_variable);
                files.extend(node_files);
                Box::new(node)
            });
            let else_clause = else_clause.as_ref().map(|node| {
                let (node_files, node) = rearrange_plan_node(node, files_order, map_per_variable);
                files.extend(node_files);
                Box::new(node)
            });

            (
                files,
                PlanNode::Condition {
                    condition: condition.clone(),
                    if_clause,
                    else_clause,
                },
            )
        }
        PlanNode::Fetch(fetch) => {
            let files: HashSet<&str> = fetch
                .variable_usages
                .iter()
                .filter_map(|name| {
                    map_per_variable
                        .get(name)
                        .map(|map| map.keys().map(|key| key.as_str()))
                })
                .flatten()
                .collect();
            (files, PlanNode::Fetch(fetch.clone()))
        }
        PlanNode::Subscription { primary, rest } => {
            let files: HashSet<&str> = primary
                .variable_usages
                .iter()
                .filter_map(|name| {
                    map_per_variable
                        .get(name)
                        .map(|map| map.keys().map(|key| key.as_str()))
                })
                .flatten()
                .collect();
            // FIXME: error if rest contain files
            (
                files,
                PlanNode::Subscription {
                    primary: primary.clone(),
                    rest: rest.clone(),
                },
            )
        }
        PlanNode::Defer { primary, deferred } => {
            let mut primary = primary.clone();
            let deferred = deferred.clone();

            // FIXME: error if deferred contain files
            if let Some(node) = primary.node {
                let (files, node) = rearrange_plan_node(&node, files_order, map_per_variable);
                primary.node = Some(Box::new(node));
                (files, PlanNode::Defer { primary, deferred })
            } else {
                (HashSet::new(), PlanNode::Defer { primary, deferred })
            }
        }
        PlanNode::Flatten(flatten_node) => {
            let (files, node) =
                rearrange_plan_node(&flatten_node.node, &files_order, map_per_variable);
            let node = PlanNode::Flatten(FlattenNode {
                node: Box::new(node),
                path: flatten_node.path.clone(),
            });
            (files, node)
        }
        PlanNode::Sequence { nodes } => {
            let mut files = HashSet::new();
            let mut sequence = Vec::new();
            let mut last_file = None;
            for node in nodes.iter() {
                let (node_files, node) = rearrange_plan_node(node, &files_order, map_per_variable);
                for file in node_files.into_iter() {
                    let index = files_order.get_index_of(file);
                    // FIXME: errors
                    assert!(!files.contains(file));
                    assert!(index > last_file);
                    last_file = index;
                    files.insert(file);
                }
                sequence.push(node);
            }
            (files, PlanNode::Sequence { nodes: sequence })
        }
        PlanNode::Parallel { nodes } => {
            let mut files = HashSet::new();
            let mut parallel = Vec::new();
            let mut sequence = BTreeMap::new();

            for node in nodes.iter() {
                let (node_files, node) = rearrange_plan_node(node, &files_order, map_per_variable);
                if node_files.is_empty() {
                    parallel.push(node);
                    continue;
                }

                let mut first_file = None;
                let mut last_file = None;
                for file in node_files.into_iter() {
                    let seen = files.insert(file);
                    // FIXME: error
                    assert!(!seen);
                    let index = files_order.get_index_of(file);
                    // FIXME: check min?
                    first_file = match first_file {
                        None => index,
                        Some(first_file) => cmp::min(Some(first_file), index),
                    };
                    last_file = cmp::max(last_file, index);
                }
                sequence.insert(first_file, (node, last_file));
            }

            if !sequence.is_empty() {
                let mut nodes = Vec::new();
                let mut sequence_last_file = None;
                for (first_file, (node, last_file)) in sequence.into_iter() {
                    // FIXME: error
                    assert!(first_file > sequence_last_file);
                    sequence_last_file = last_file;
                    nodes.push(node);
                }

                parallel.push(PlanNode::Sequence { nodes });
            }

            (files, PlanNode::Parallel { nodes: parallel })
        }
    }
}

async fn call_subgraph(mut req: subgraph::Request) -> subgraph::Request {
    let supergraph_result = req
        .context
        .extensions()
        .lock()
        .get::<SupergraphLayerResult>()
        .cloned();
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult {
            multipart,
            map_per_variable,
            files_order,
        } = supergraph_result;

        let variables = &mut req.subgraph_request.body_mut().variables;
        let mut map_field: MapField = IndexMap::new();
        for (variable_name, variable_value) in variables.iter_mut() {
            let variable_name = variable_name.as_str();
            if let Some(variable_map) = map_per_variable.get(variable_name) {
                for (file, paths) in variable_map.iter() {
                    map_field.insert(
                        file.clone(),
                        paths
                            .iter()
                            .map(|path| {
                                if path.is_empty() {
                                    format!("variables.{}", variable_name)
                                } else {
                                    format!("variables.{}.{}", variable_name, path.join("."))
                                }
                            })
                            .collect(),
                    );
                    for path in paths {
                        if let Some(json_value) = try_path(variable_value, path) {
                            json_value.take();
                        }
                    }
                }
            }
        }
        if !map_field.is_empty() {
            map_field.sort_by_cached_key(|file, _| files_order.get_index_of(file));
            req.subgraph_request
                .extensions_mut()
                .insert(SubgraphHttpRequestExtensions {
                    multipart,
                    map_field,
                });
        }
    }
    req
}

struct SubgraphHttpRequestExtensions {
    multipart: Arc<Mutex<Multipart<'static>>>,
    map_field: MapField,
}

use tower::Service;

const APOLLO_REQUIRE_PREFLIGHT: http::HeaderName =
    HeaderName::from_static("apollo-require-preflight");
const TRUE: http::HeaderValue = HeaderValue::from_static("true");

pub(crate) async fn wrap_http_client_call(
    mut client: crate::services::http::BoxService,
    mut http_request: crate::services::http::HttpRequest,
) -> Result<crate::services::http::HttpResponse, BoxError> {
    let supergraph_result = http_request.http_request.extensions_mut().remove();
    if let Some(supergraph_result) = supergraph_result {
        let SubgraphHttpRequestExtensions {
            multipart,
            map_field,
        } = supergraph_result;

        let crate::services::http::HttpRequest {
            http_request,
            context,
        } = http_request;
        let (mut request_parts, request_body) = http_request.into_parts();

        let form = MultipartFormData::new();
        request_parts
            .headers
            .insert(CONTENT_TYPE, form.content_type());
        request_parts.headers.insert(APOLLO_REQUIRE_PREFLIGHT, TRUE);

        let last = tokio_stream::once(Ok(format!("--{}--\r\n", form.boundary).into()));
        let map_stream = tokio_stream::once(Ok(Bytes::from(
            serde_json::to_vec(&map_field).expect("map should be serializable to JSON"),
        )));

        let multipart = multipart.lock_owned().await;
        let file_names = map_field.into_keys().collect();
        let files = (MultipartFileStream {
            multipart,
            file_names,
        })
        .map(move |field| {
            // FIXME
            let file = field.unwrap();
            (file.headers().clone(), hyper::Body::wrap_stream(file))
        });
        // FIXME: check that operation is not compressed
        let new_body = form
            .field("operations", request_body)
            .chain(form.field("map", map_stream))
            .chain(
                files
                    .map(move |(headers, body)| form.file(headers, body))
                    .flatten(),
            )
            .chain(last);

        let request_body = hyper::Body::wrap_stream(new_body);
        let http_request = http::Request::from_parts(request_parts, request_body);
        return client
            .call(crate::services::http::HttpRequest {
                http_request,
                context,
            })
            .await;
    }
    client.call(http_request).await
}

struct MultipartFileStream {
    file_names: HashSet<String>,
    multipart: OwnedMutexGuard<multer::Multipart<'static>>,
}

impl Stream for MultipartFileStream {
    type Item = UploadResult<multer::Field<'static>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.multipart.poll_next_field(cx) {
                Poll::Ready(Ok(None)) => {
                    // FIXME: validation error
                    assert!(self.file_names.is_empty());
                    return Poll::Ready(None);
                }
                Poll::Ready(Ok(Some(field))) => {
                    if let Some(name) = field.name() {
                        if self.file_names.remove(name) {
                            return Poll::Ready(Some(Ok(field)));
                        }
                    }
                    // The file is extraneous.
                    // As the rest can still be processed, just ignore it and don’t exit with an error.
                    // Matching https://github.com/jaydenseric/graphql-upload/blob/f24d71bfe5be343e65d084d23073c3686a7f4d18/processRequest.mjs#L231-L236
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug, Clone)]
struct MultipartFormData {
    boundary: String,
}

impl MultipartFormData {
    fn new() -> Self {
        let boundary = format!(
            "------------------------{:016x}",
            rand::thread_rng().next_u64()
        );
        Self { boundary }
    }

    fn content_type(&self) -> HeaderValue {
        let boundary =
            mediatype::Value::new(&self.boundary).expect("boundary should be valid value");
        let params = [(BOUNDARY, boundary)];
        let mime = MediaType::from_parts(MULTIPART, FORM_DATA, None, &params);
        mime.to_string()
            .try_into()
            .expect("mime should be valid header value")
    }

    fn field(
        &self,
        name: &str,
        value_stream: impl Stream<Item = hyper::Result<Bytes>>,
    ) -> impl Stream<Item = hyper::Result<Bytes>> {
        let prefix = format!(
            "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n",
            self.boundary, name
        );
        let prefix = tokio_stream::once(Ok(Bytes::from(prefix)));
        prefix
            .chain(value_stream)
            .chain(tokio_stream::once(Ok("\r\n".into())))
    }

    fn file(
        &self,
        headers: HeaderMap,
        value_stream: impl Stream<Item = hyper::Result<Bytes>>,
    ) -> impl Stream<Item = hyper::Result<Bytes>> {
        let mut prefix = Vec::new();
        prefix.extend_from_slice(b"--");
        prefix.extend_from_slice(self.boundary.as_bytes());
        prefix.extend_from_slice(b"\r\n");
        for (k, v) in headers.iter() {
            prefix.extend_from_slice(k.as_str().as_bytes());
            prefix.extend_from_slice(b": ");
            prefix.extend_from_slice(v.as_bytes());
            prefix.extend_from_slice(b"\r\n");
        }
        prefix.extend_from_slice(b"\r\n");

        let prefix = tokio_stream::once(Ok(Bytes::from(prefix)));
        prefix
            .chain(value_stream)
            .chain(tokio_stream::once(Ok("\r\n".into())))
    }
}
