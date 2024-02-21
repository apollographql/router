use std::ops::ControlFlow;
use std::sync::Arc;

use futures::FutureExt;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use http::HeaderName;
use http::HeaderValue;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use mediatype::MediaType;
use mediatype::ReadParams;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::config::FileUploadsConfig;
use self::config::MultipartRequestLimits;
use self::error::FileUploadError;
use self::map_field::MapField;
use self::map_field::MapFieldRaw;
use self::multipart_form_data::MultipartFormData;
use self::multipart_request::MultipartRequest;
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
mod map_field;
mod multipart_form_data;
mod multipart_request;
mod rearrange_query_plan;

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

        // patch variables to pass validation
        for (variable_name, map) in map_field.map_per_variable.iter() {
            for (filename, paths) in map.iter() {
                for variable_path in paths.iter() {
                    let json_value = variables
                        .get_mut(variable_name.as_str())
                        .and_then(|root| try_path(root, variable_path));

                    if let Some(json_value) = json_value {
                        drop(core::mem::replace(
                            json_value,
                            serde_json_bytes::Value::String(
                                format!("<Placeholder for file '{}'>", filename).into(),
                            ),
                        ));
                    } else {
                        let path = format!("{}.{}", variable_name, variable_path.join("."));
                        return Err(FileUploadError::InputValueNotFound(path));
                    }
                }
            }
        }

        req.context.extensions().lock().insert(multipart);
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

#[derive(Clone)]
struct SupergraphLayerResult {
    multipart: MultipartRequest,
    map: Arc<MapField>,
}

fn execution_layer(req: execution::Request) -> UploadResult<execution::Request> {
    let supergraph_result = req
        .context
        .extensions()
        .lock()
        .get::<SupergraphLayerResult>()
        .cloned();
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult { map, .. } = supergraph_result;

        let query_plan = Arc::new(rearange_query_plan(&req.query_plan, &map)?);
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
        let SupergraphLayerResult { multipart, map } = supergraph_result;

        let variables = &mut req.subgraph_request.body_mut().variables;
        for (variable_name, variable_value) in variables.iter_mut() {
            if let Some(variable_map) = map.map_per_variable.get(variable_name.as_str()) {
                for paths in variable_map.values() {
                    for path in paths {
                        if let Some(json_value) = try_path(variable_value, path) {
                            json_value.take();
                        }
                    }
                }
            }
        }

        let subgraph_map = map.sugraph_map(variables.keys());
        if !subgraph_map.is_empty() {
            req.subgraph_request
                .extensions_mut()
                .insert(SubgraphHttpRequestExtensions {
                    multipart,
                    map_field: subgraph_map,
                });
        }
    }
    req
}

struct SubgraphHttpRequestExtensions {
    multipart: MultipartRequest,
    map_field: MapFieldRaw,
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
