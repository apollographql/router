use indexmap::IndexMap;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
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

use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;
use crate::services::http::HttpRequest;
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

register_private_plugin!("apollo", "experimental_file_uploads", FileUploadsPlugin);

#[async_trait::async_trait]
impl PluginPrivate for FileUploadsPlugin {
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

    fn http_client_service(
        &self,
        _subgraph_name: &str,
        service: crate::services::http::BoxService,
    ) -> crate::services::http::BoxService {
        ServiceBuilder::new()
            .oneshot_checkpoint_async(|req: HttpRequest| {
                send_multipart_request(req)
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

async fn extract_map(req: supergraph::Request) -> supergraph::Request {
    let service_layer_result = req
        .context
        .extensions()
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

        let mut map_per_variable = HashMap::<String, HashMap<String, String>>::new();
        for (file, paths) in map_field.iter() {
            for path in paths.iter() {
                let mut segments = path.splitn(3, ".");
                assert!(segments.next() == Some("variables"));
                // FIXME: validation error
                let var_name = segments.next().unwrap();
                let var_path = segments.next().unwrap_or("");

                map_per_variable
                    .entry(var_name.to_owned())
                    .or_insert_with(|| HashMap::new())
                    .insert(var_path.to_owned(), file.clone());
            }
        }
        println!("{:?}", map_per_variable);

        req.context
            .extensions()
            .lock()
            .insert(SupergraphLayerResult {
                multipart: Arc::new(Mutex::new(multipart)),
                map_field,
            });
    }
    req
}

type MapField = IndexMap<String, Vec<String>>;

#[derive(Clone)]
struct SupergraphLayerResult {
    multipart: Arc<Mutex<Multipart<'static>>>,
    map_field: MapField,
}

async fn call_subgraph(req: subgraph::Request) -> subgraph::Request {
    req
}

const APOLLO_REQUIRE_PREFLIGHT: http::HeaderName =
    HeaderName::from_static("apollo-require-preflight");
const TRUE: http::HeaderValue = HeaderValue::from_static("true");

async fn send_multipart_request(mut req: HttpRequest) -> HttpRequest {
    let supergraph_result = req
        .context
        .extensions()
        .lock()
        .get::<SupergraphLayerResult>()
        .cloned();
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult {
            multipart,
            map_field,
        } = supergraph_result;

        let (mut request_parts, request_body) = req.http_request.into_parts();

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
        let files = (MultipartFileStream { multipart }).map(move |field| {
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
        req.http_request = http::Request::from_parts(request_parts, request_body);
    }
    req
}

struct MultipartFileStream {
    multipart: OwnedMutexGuard<multer::Multipart<'static>>,
}

impl Stream for MultipartFileStream {
    type Item = multer::Result<multer::Field<'static>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.multipart.poll_next_field(cx) {
            Poll::Ready(Ok(None)) => Poll::Ready(None),
            Poll::Ready(Ok(Some(field))) => Poll::Ready(Some(Ok(field))),
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
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
