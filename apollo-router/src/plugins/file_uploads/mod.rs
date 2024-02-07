use indexmap::IndexMap;
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

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;

use crate::layers::ServiceBuilderExt;
use crate::services::subgraph;
use crate::services::supergraph;

use self::config::FileUploadsConfig;

mod config;

// FIXME: check if we need to hide docs
#[doc(hidden)] // Only public for integration tests
struct FileUploadsPlugin {
    config: FileUploadsConfig,
}

register_plugin!("apollo", "preview_file_uploads", FileUploadsPlugin);

#[async_trait::async_trait]
impl Plugin for FileUploadsPlugin {
    type Config = FileUploadsConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            config: init.config,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: router::Request| {
                extract_operations(req)
                    .map(|req| Ok(ControlFlow::Continue(req)))
                    .boxed()
            })
            .service(service)
            .boxed()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: supergraph::Request| {
                extract_map(req)
                    .map(|req| Ok(ControlFlow::Continue(req)))
                    .boxed()
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxService,
    ) -> subgraph::BoxService {
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

async fn extract_operations(req: router::Request) -> router::Request {
    if let Some(mime) = get_multipart_mime(&req) {
        // FIXME: remove unwrap, "No boundary found in `Content-Type` header"
        let boundary = mime.get_param(BOUNDARY).unwrap().to_string();

        let (mut request_parts, request_body) = req.router_request.into_parts();
        let mut multipart = Multipart::new(request_body, boundary);

        // FIXME: unwrap
        let operations_field = multipart.next_field().await.unwrap().unwrap();
        // FIXME
        assert!(
            operations_field.name() == Some("operations"),
            "Missing multipart field ‘operations’, please see GRAPHQL_MULTIPART_REQUEST_SPEC_URL.",
        );

        req.context
            .private_entries
            .lock()
            .insert(ServiceLayerResult { multipart });

        let content_type = operations_field
            .headers()
            .get(CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("application/json"));
        request_parts.headers.insert(CONTENT_TYPE, content_type);
        request_parts.headers.remove(CONTENT_LENGTH);

        return router::Request::from((
            http::Request::from_parts(request_parts, hyper::Body::wrap_stream(operations_field)),
            req.context,
        ));
    }
    req
}

struct ServiceLayerResult {
    multipart: Multipart<'static>,
}

async fn extract_map(mut req: supergraph::Request) -> supergraph::Request {
    let service_layer_result = req
        .context
        .private_entries
        .lock()
        .remove::<ServiceLayerResult>();

    if let Some(ServiceLayerResult { mut multipart }) = service_layer_result {
        // FIXME: unwrap
        let map_field = multipart.next_field().await.unwrap().unwrap();
        // FIXME
        assert!(
            map_field.name() == Some("map"),
            "Missing multipart field ‘map’, please see GRAPHQL_MULTIPART_REQUEST_SPEC_URL.",
        );
        // FIXME: apply some limit on size of map field

        let map_field = map_field.bytes().await.unwrap();
        // FIXME: unwrap
        let map_field: MapField = serde_json::from_slice(&map_field).unwrap();
        // FIXME: check number of files
        // assert!(map_field.len());

        let variables = &mut req.supergraph_request.body_mut().variables;
        let mut map_per_variable: MapPerVariable = HashMap::new();
        for (file, paths) in map_field.iter() {
            for path in paths.iter() {
                let mut segments = path.split('.');
                let first_segment = segments.next();
                if first_segment != Some("variables") {
                    if first_segment
                        .and_then(|str| str.parse::<usize>().ok())
                        .is_some()
                    {
                        assert!(false, "batch requests are not supported");
                    }
                    assert!(false, "invalid path inside 'map' field, it should start with 'variables.'.");
                }
                // FIXME: validation error
                let variable_name = segments.next().unwrap();
                let variable_path: Vec<String> = segments.map(|str| str.to_owned()).collect();

                // patch variables to pass validation
                let json_value = variables
                    .get_mut(variable_name)
                    .and_then(|root| try_path(root, &variable_path));
                // FIXME: validation error
                let json_value = json_value.unwrap();
                drop(core::mem::replace(
                    json_value,
                    serde_json_bytes::Value::String(
                        format!("<Placeholder for file '{}'>", file).into(),
                    ),
                ));

                map_per_variable
                    .entry(variable_name.to_owned())
                    .or_insert_with(|| HashMap::new())
                    .entry(file.clone())
                    .or_insert_with(|| Vec::new())
                    .push(variable_path);
            }
        }

        req.context
            .private_entries
            .lock()
            .insert(SupergraphLayerResult {
                multipart: Arc::new(Mutex::new(multipart)),
                map_per_variable,
            });
    }
    req
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
    map_per_variable: MapPerVariable,
}

async fn call_subgraph(mut req: subgraph::Request) -> subgraph::Request {
    let supergraph_result = req
        .context
        .private_entries
        .lock()
        .get::<SupergraphLayerResult>()
        .cloned();
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult {
            multipart,
            map_per_variable,
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
use tower_http::decompression::DecompressionBody;

use crate::services::subgraph_service::HTTPClientService;

const APOLLO_REQUIRE_PREFLIGHT: http::HeaderName =
    HeaderName::from_static("apollo-require-preflight");
const TRUE: http::HeaderValue = HeaderValue::from_static("true");

pub(crate) async fn wrap_http_client_call(
    mut client: HTTPClientService,
    mut request: http::Request<hyper::Body>,
) -> Result<http::Response<DecompressionBody<hyper::Body>>, hyper::Error> {
    let supergraph_result = request.extensions_mut().remove();
    if let Some(supergraph_result) = supergraph_result {
        let SubgraphHttpRequestExtensions {
            multipart,
            map_field,
        } = supergraph_result;

        let (mut request_parts, request_body) = request.into_parts();

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
        // FIXME: skip unussed files
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
        let request = http::Request::from_parts(request_parts, request_body);
        return client.call(request).await;
    }
    client.call(request).await
}

struct MultipartFileStream {
    file_names: HashSet<String>,
    multipart: OwnedMutexGuard<multer::Multipart<'static>>,
}

impl Stream for MultipartFileStream {
    type Item = multer::Result<multer::Field<'static>>;

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
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e))),
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
