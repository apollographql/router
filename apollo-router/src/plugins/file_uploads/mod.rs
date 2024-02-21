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
use http::HeaderMap;
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
use self::rearrange_query_plan::rearange_query_plan;
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
mod rearrange_query_plan;

type UploadResult<T> = Result<T, error::FileUploadError>;

// The limit to set for the map field in the multipart request.
// We don't expect this to ever be reached, but we can always add a config option if needed later.
const MAP_SIZE_LIMIT: u64 = 10 * 1024;

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
        let limits = self.limits;
        ServiceBuilder::new()
            .oneshot_checkpoint_async(move |req: router::Request| {
                async move {
                    let context = req.context.clone();
                    Ok(match router_layer(req, limits).await {
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
            .oneshot_checkpoint_async(move |req: supergraph::Request| {
                async move {
                    let context = req.context.clone();
                    Ok(match supergraph_layer(req).await {
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
    limits: MultipartRequestLimits,
) -> UploadResult<router::Request> {
    if let Some(mime) = get_multipart_mime(&req) {
        let boundary = mime
            .get_param(BOUNDARY)
            .ok_or_else(|| FileUploadError::InvalidMultipartRequest(multer::Error::NoBoundary))?
            .to_string();

        let (mut request_parts, request_body) = req.router_request.into_parts();

        let mut multipart = MultipartRequest::new(request_body, boundary, limits);
        let operations_stream = multipart.operations_field().await?;

        req.context.extensions().lock().insert(multipart);

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

async fn supergraph_layer(mut req: supergraph::Request) -> UploadResult<supergraph::Request> {
    let multipart = req
        .context
        .extensions()
        .lock()
        .get::<MultipartRequest>()
        .cloned();

    if let Some(mut multipart) = multipart {
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
                    .or_default()
                    .entry(filename.clone())
                    .or_default()
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
    path: &'a [String],
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

static APOLLO_REQUIRE_PREFLIGHT: HeaderName = HeaderName::from_static("apollo-require-preflight");
static TRUE: http::HeaderValue = HeaderValue::from_static("true");

pub(crate) async fn http_request_wrapper(
    mut req: http::Request<hyper::Body>,
) -> http::Request<hyper::Body> {
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
        request_parts
            .headers
            .insert(APOLLO_REQUIRE_PREFLIGHT.clone(), TRUE.clone());
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
    limits: MultipartRequestLimits,
    read_files_counter: usize,
    file_sizes: Vec<usize>,
    max_files_exceeded: bool,
    max_files_size_exceeded: bool,
}

impl Drop for MultipartRequestState {
    fn drop(&mut self) {
        u64_counter!(
            "apollo.router.operations.file_uploads",
            "file uploads",
            1,
            "file_uploads.limits.max_file_size.exceeded" = self.max_files_size_exceeded,
            "file_uploads.limits.max_files.exceeded" = self.max_files_exceeded
        );

        for file_size in &self.file_sizes {
            u64_histogram!(
                "apollo.router.operations.file_uploads.file_size",
                "file upload sizes",
                (*file_size) as u64
            );
        }
        u64_histogram!(
            "apollo.router.operations.file_uploads.files",
            "number of files per request",
            self.read_files_counter as u64
        );
    }
}

impl MultipartRequest {
    fn new(request_body: hyper::Body, boundary: String, limits: MultipartRequestLimits) -> Self {
        let multer = Multipart::with_constraints(
            request_body,
            boundary,
            Constraints::new().size_limit(SizeLimit::new().for_field("map", MAP_SIZE_LIMIT)),
        );
        Self {
            state: Arc::new(Mutex::new(MultipartRequestState {
                multer,
                limits,
                read_files_counter: 0,
                file_sizes: Vec::new(),
                max_files_exceeded: false,
                max_files_size_exceeded: false,
            })),
        }
    }

    async fn operations_field(&mut self) -> UploadResult<multer::Field<'static>> {
        self.state
            .lock()
            .await
            .multer
            .next_field()
            .await?
            .filter(|field| field.name() == Some("operations"))
            .ok_or_else(|| FileUploadError::MissingOperationsField)
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

        let map_field: MapField =
            serde_json::from_slice(&bytes).map_err(FileUploadError::InvalidJsonInMapField)?;

        let limit = state.limits.max_files;
        if map_field.len() > limit {
            state.max_files_exceeded = true;
            return Err(FileUploadError::MaxFilesLimitExceeded(limit));
        }
        Ok(map_field)
    }

    async fn subgraph_stream<FilePrefixFn>(
        &mut self,
        file_names: HashSet<String>,
        file_prefix_fn: FilePrefixFn,
    ) -> SubgraphFileProxyStream<FilePrefixFn>
    where
        FilePrefixFn: Fn(&HeaderMap) -> Bytes,
    {
        let state = self.state.clone().lock_owned().await;
        SubgraphFileProxyStream::new(state, file_names, file_prefix_fn)
    }
}

pin_project! {
    struct SubgraphFileProxyStream<FilePrefixFn> {
        state: OwnedMutexGuard<MultipartRequestState>,
        file_names: HashSet<String>,
        file_prefix_fn: FilePrefixFn,
        #[pin]
        current_field: Option<multer::Field<'static>>,
        current_field_bytes: usize,
    }
}

impl<FilePrefixFn> SubgraphFileProxyStream<FilePrefixFn>
where
    FilePrefixFn: Fn(&HeaderMap) -> Bytes,
{
    fn new(
        state: OwnedMutexGuard<MultipartRequestState>,
        file_names: HashSet<String>,
        file_prefix_fn: FilePrefixFn,
    ) -> Self {
        Self {
            state,
            file_names,
            file_prefix_fn,
            current_field: None,
            current_field_bytes: 0,
        }
    }

    fn poll_current_field(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<UploadResult<Bytes>>> {
        if let Some(field) = &mut self.current_field {
            let filename = field
                .file_name()
                .or_else(|| field.name())
                .map(|name| format!("'{}'", name))
                .unwrap_or_else(|| "unknown".to_owned());

            let field = Pin::new(field);
            match field.poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => {
                    self.current_field = None;
                    let file_size = self.current_field_bytes;
                    self.state.file_sizes.push(file_size);
                    Poll::Ready(None)
                }
                Poll::Ready(Some(Ok(bytes))) => {
                    self.current_field_bytes += bytes.len();
                    let limit = self.state.limits.max_file_size;
                    if self.current_field_bytes > (limit.as_u64() as usize) {
                        self.current_field = None;
                        self.state.max_files_size_exceeded = true;
                        Poll::Ready(Some(Err(FileUploadError::MaxFileSizeLimitExceeded {
                            limit,
                            filename,
                        })))
                    } else {
                        Poll::Ready(Some(Ok(bytes)))
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    Poll::Ready(Some(Err(FileUploadError::InvalidMultipartRequest(e))))
                }
            }
        } else {
            Poll::Ready(None)
        }
    }

    fn poll_next_field(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<UploadResult<Bytes>>> {
        if self.file_names.is_empty() {
            return Poll::Ready(None);
        }
        loop {
            match self.state.multer.poll_next_field(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(None)) => {
                    if self.file_names.is_empty() {
                        return Poll::Ready(None);
                    }

                    let files = mem::take(&mut self.file_names);
                    return Poll::Ready(Some(Err(FileUploadError::MissingFiles(
                        files
                            .into_iter()
                            .map(|file| format!("'{}'", file))
                            .join(", "),
                    ))));
                }
                Poll::Ready(Ok(Some(field))) => {
                    let limit = self.state.limits.max_files;
                    if self.state.read_files_counter == limit {
                        self.state.max_files_exceeded = true;
                        return Poll::Ready(Some(Err(FileUploadError::MaxFilesLimitExceeded(
                            limit,
                        ))));
                    } else {
                        self.state.read_files_counter += 1;

                        if let Some(name) = field.name() {
                            if self.file_names.remove(name) {
                                let prefix = (self.file_prefix_fn)(field.headers());
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
            }
        }
    }
}

impl<FilePrefixFn> Stream for SubgraphFileProxyStream<FilePrefixFn>
where
    FilePrefixFn: Fn(&HeaderMap) -> Bytes,
{
    type Item = UploadResult<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let field_result = self.poll_current_field(cx);
        match field_result {
            Poll::Ready(None) => self.poll_next_field(cx),
            _ => field_result,
        }
    }
}
