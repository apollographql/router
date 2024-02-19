use std::collections::HashMap;
use std::collections::HashSet;
use std::mem;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::task;
use std::task::Poll;

use bytes::Bytes;
use futures::FutureExt;
use futures::Stream;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::HeaderName;
use http::HeaderValue;
use indexmap::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use mediatype::MediaType;
use mediatype::ReadParams;
use multer::Constraints;
use multer::Multipart;
use multer::SizeLimit;
use pin_project_lite::pin_project;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::config::FileUploadsConfig;
use self::config::MultipartRequestLimits;
use self::error::FileUploadError;
use self::multipart_form_data::MultipartFormData;
use self::rearange_query_plan::rearange_query_plan;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

mod config;
mod error;
mod multipart_form_data;
mod rearange_query_plan;

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
        let max_file_size = self.limits.max_file_size.as_u64();
        let max_files = self.limits.max_files;
        ServiceBuilder::new()
            .oneshot_checkpoint_async(move |req: router::Request| {
                async move {
                    let context = req.context.clone();
                    Ok(match router_layer(req, max_files, max_file_size).await {
                        Ok(req) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            router::Response::error_builder()
                                .error(err)
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
            .oneshot_checkpoint_async(move |req: supergraph::Request| {
                async move {
                    let context = req.context.clone();
                    Ok(match supergraph_layer(req).await {
                        Ok(req) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            supergraph::Response::error_builder()
                                .error(err)
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
                let context = req.context.clone();
                Ok(match execution_layer(req) {
                    Ok(req) => ControlFlow::Continue(req),
                    Err(err) => ControlFlow::Break(
                        execution::Response::error_builder()
                            .errors(vec![err.into()])
                            .context(context)
                            .build()?,
                    ),
                })
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
                subgraph_layer(req)
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

async fn router_layer(
    req: router::Request,
    max_files: usize,
    max_file_size: u64,
) -> UploadResult<router::Request> {
    if let Some(mime) = get_multipart_mime(&req) {
        let boundary = mime
            .get_param(BOUNDARY)
            .ok_or_else(|| FileUploadError::InvalidMultipartRequest(multer::Error::NoBoundary))?
            .to_string();

        let (mut request_parts, request_body) = req.router_request.into_parts();

        let mut multipart = MultipartRequest::new(request_body, boundary, max_files, max_file_size);
        let operations_stream = multipart.operations_field().await?;

        req.context
            .extensions()
            .lock()
            .insert(RouterLayerResult { multipart });

        let content_type = operations_stream
            .headers()
            .get(CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("application/json"));
        request_parts.headers.insert(CONTENT_TYPE, content_type);
        request_parts.headers.remove(CONTENT_LENGTH);

        let request_body = hyper::Body::wrap_stream(operations_stream);
        return Ok(router::Request::from((
            http::Request::from_parts(request_parts, request_body),
            req.context,
        )));
    }

    Ok(req)
}

struct RouterLayerResult {
    multipart: MultipartRequest,
}

async fn supergraph_layer(mut req: supergraph::Request) -> UploadResult<supergraph::Request> {
    let service_layer_result = req
        .context
        .extensions()
        .lock()
        .remove::<RouterLayerResult>();

    if let Some(RouterLayerResult { mut multipart }) = service_layer_result {
        let map_field = multipart.map_field().await?;
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
                let variable_path: Vec<String> = segments.map(str::to_owned).collect();

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
                multipart,
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
type MapPerVariable = HashMap<String, MapPerFile>;
type MapPerFile = HashMap<String, Vec<Vec<String>>>;

#[derive(Clone)]
struct SupergraphLayerResult {
    multipart: MultipartRequest,
    files_order: IndexSet<String>,
    map_per_variable: MapPerVariable,
}

fn execution_layer(req: execution::Request) -> UploadResult<execution::Request> {
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

        let query_plan = Arc::new(rearange_query_plan(
            &req.query_plan,
            &files_order,
            &map_per_variable,
        )?);
        return Ok(execution::Request { query_plan, ..req });
    }
    Ok(req)
}

async fn subgraph_layer(mut req: subgraph::Request) -> subgraph::Request {
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
    multipart: MultipartRequest,
    map_field: MapField,
}

const APOLLO_REQUIRE_PREFLIGHT: http::HeaderName =
    HeaderName::from_static("apollo-require-preflight");
const TRUE: http::HeaderValue = HeaderValue::from_static("true");

pub(crate) async fn http_request_wrapper(
    mut req: http::Request<hyper::Body>,
) -> http::Request<hyper::Body> {
    // This is a noop if the file upload extensions are not present.
    let supergraph_result = req.extensions_mut().remove();
    if let Some(supergraph_result) = supergraph_result {
        let SubgraphHttpRequestExtensions {
            multipart,
            map_field,
        } = supergraph_result;

        let (mut request_parts, operations) = req.into_parts();
        let form = MultipartFormData::new(operations, map_field, multipart);
        request_parts
            .headers
            .insert(CONTENT_TYPE, form.content_type());
        request_parts.headers.insert(APOLLO_REQUIRE_PREFLIGHT, TRUE);
        return http::Request::from_parts(
            request_parts,
            hyper::Body::wrap_stream(form.into_stream().await),
        );
    }
    req
}

#[derive(Clone)]
struct MultipartRequest {
    state: Arc<Mutex<MultipartRequestState>>,
}

struct MultipartRequestState {
    multer: multer::Multipart<'static>,
    max_files: usize,
    files_counter: usize,
}

impl MultipartRequest {
    fn new(
        request_body: hyper::Body,
        boundary: String,
        max_files: usize,
        max_file_size: u64,
    ) -> Self {
        let multer = Multipart::with_constraints(
            request_body,
            boundary,
            Constraints::new().size_limit(
                SizeLimit::new()
                    .for_field("operations", u64::MAX) // no limit
                    .for_field("map", 10 * 1024) // hardcoded to 10kb
                    .per_field(max_file_size),
            ),
        );
        Self {
            state: Arc::new(Mutex::new(MultipartRequestState {
                multer,
                max_files,
                files_counter: 0,
            })),
        }
    }

    async fn operations_field(&mut self) -> UploadResult<multer::Field<'static>> {
        Ok(self
            .state
            .lock()
            .await
            .multer
            .next_field()
            .await?
            .filter(|field| field.name() == Some("operations"))
            .ok_or_else(|| FileUploadError::MissingOperationsField)?)
    }

    async fn map_field(&mut self) -> UploadResult<MapField> {
        let mut state = self.state.lock().await;
        let bytes = state
            .multer
            .next_field()
            .await?
            .filter(|field| field.name() == Some("map"))
            .ok_or_else(|| FileUploadError::MissingMapField)?
            .bytes()
            .await?;

        let map_field: MapField = serde_json::from_slice(&bytes)
            .map_err(|e| FileUploadError::InvalidJsonInMapField(e))?;
        if map_field.len() > state.max_files {
            return Err(FileUploadError::MaxFilesLimitExceeded(state.max_files));
        }
        Ok(map_field)
    }

    async fn subgraph_stream<BeforeFn, AfterFn>(
        &mut self,
        before_bytes_fn: BeforeFn,
        file_names: HashSet<String>,
        after_bytes_fn: AfterFn,
    ) -> SubgraphFileProxyStream<BeforeFn, AfterFn> {
        let state = self.state.clone().lock_owned().await;
        SubgraphFileProxyStream::new(state, before_bytes_fn, file_names, after_bytes_fn)
    }
}

pin_project! {
    struct SubgraphFileProxyStream<BeforeFn, AfterFn> {
        state: OwnedMutexGuard<MultipartRequestState>,
        before_bytes_fn: BeforeFn,
        file_names: HashSet<String>,
        after_bytes_fn: AfterFn,
        #[pin]
        current_field: Option<multer::Field<'static>>,
    }
}

impl<BeforeFn, AfterFn> SubgraphFileProxyStream<BeforeFn, AfterFn> {
    fn new(
        state: OwnedMutexGuard<MultipartRequestState>,
        before_bytes_fn: BeforeFn,
        file_names: HashSet<String>,
        after_bytes_fn: AfterFn,
    ) -> Self {
        Self {
            state,
            before_bytes_fn,
            file_names,
            after_bytes_fn,
            current_field: None,
        }
    }
}

impl<BeforeFn, AfterFn> Stream for SubgraphFileProxyStream<BeforeFn, AfterFn>
where
    BeforeFn: Fn(&multer::Field<'static>) -> Bytes,
    AfterFn: Fn() -> Bytes,
{
    type Item = UploadResult<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(field) = &mut self.current_field {
            let stream = Pin::new(field);
            match stream.poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => {
                    self.current_field = None;
                    Poll::Ready(Some(Ok((self.after_bytes_fn)())))
                }
                Poll::Ready(Some(Ok(item))) => Poll::Ready(Some(Ok(item))),
                Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(match e {
                    multer::Error::FieldSizeExceeded { limit, field_name } => {
                        FileUploadError::MaxFileSizeLimitExceeded {
                            limit,
                            file_name: field_name
                                .map(|name| format!("'{0}'", name))
                                .unwrap_or("unnamed".to_owned()),
                        }
                    }
                    e => FileUploadError::InvalidMultipartRequest(e),
                }))),
            }
        } else {
            if self.file_names.is_empty() {
                return Poll::Ready(None);
            }
            loop {
                match self.state.multer.poll_next_field(cx) {
                    Poll::Ready(Ok(None)) => {
                        if !self.file_names.is_empty() {
                            let files = mem::replace(&mut self.file_names, HashSet::new());
                            return Poll::Ready(Some(Err(FileUploadError::MissingFiles(
                                files
                                    .into_iter()
                                    .map(|file| format!("'{}'", file))
                                    .join(", "),
                            ))));
                        }
                        return Poll::Ready(None);
                    }
                    Poll::Ready(Ok(Some(mut field))) => {
                        if self.state.files_counter == self.state.max_files {
                            return Poll::Ready(Some(Err(FileUploadError::MaxFilesLimitExceeded(
                                self.state.max_files,
                            ))));
                        } else {
                            self.state.files_counter += 1;

                            if let Some(name) = field.name() {
                                if self.file_names.remove(name) {
                                    let prefix = (self.before_bytes_fn)(&mut field);
                                    self.current_field = Some(field);
                                    return Poll::Ready(Some(Ok(prefix)));
                                }
                            }

                            // The file is extraneous, but the rest can still be processed.
                            // Just ignore it and don’t exit with an error.
                            // Matching https://github.com/jaydenseric/graphql-upload/blob/f24d71bfe5be343e65d084d23073c3686a7f4d18/processRequest.mjs#L231-L236
                        }
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                    Poll::Pending => return Poll::Pending,
                };
            }
        }
    }
}
