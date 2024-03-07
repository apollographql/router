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
use self::multipart_form_data::MultipartFormData;
use self::multipart_request::MultipartRequest;
use self::rearrange_query_plan::rearrange_query_plan;
use crate::json_ext;
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

type Result<T> = std::result::Result<T, error::FileUploadError>;

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

    async fn new(init: PluginInit<Self::Config>) -> std::result::Result<Self, BoxError> {
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
) -> Result<router::Request> {
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

        // override Content-Type to content type of 'operations' field
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

async fn supergraph_layer(mut req: supergraph::Request) -> Result<supergraph::Request> {
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
        for variable_map in map_field.per_variable.values() {
            for (filename, paths) in variable_map.iter() {
                for variable_path in paths.iter() {
                    replace_value_at_path(
                        variables,
                        variable_path,
                        serde_json_bytes::Value::String(
                            format!("<Placeholder for file '{}'>", filename).into(),
                        ),
                    )
                    .map_err(|path| FileUploadError::InputValueNotFound(path.join(".")))?;
                }
            }
        }

        req.context
            .extensions()
            .lock()
            .insert(SupergraphLayerResult {
                multipart,
                map: Arc::new(map_field),
            });
    }
    Ok(req)
}

// Replaces value at path with the provided one.
// Returns the provided path if the path is not valid for the given object
fn replace_value_at_path<'a>(
    variables: &'a mut json_ext::Object,
    path: &'a [String],
    value: serde_json_bytes::Value,
) -> std::result::Result<(), &'a [String]> {
    if let Some(v) = get_value_at_path(variables, path) {
        *v = value;
        Ok(())
    } else {
        Err(path)
    }
}

// Removes value at path.
fn remove_value_at_path<'a>(variables: &'a mut json_ext::Object, path: &'a [String]) {
    let _ = get_value_at_path(variables, path).take();
}

fn get_value_at_path<'a>(
    variables: &'a mut json_ext::Object,
    path: &'a [String],
) -> Option<&'a mut serde_json_bytes::Value> {
    let mut iter = path.iter();
    let variable_name = iter.next();
    if let Some(variable_name) = variable_name {
        let root = variables.get_mut(variable_name.as_str());
        if let Some(root) = root {
            return iter.try_fold(root, |parent, segment| match parent {
                serde_json_bytes::Value::Object(map) => map.get_mut(segment.as_str()),
                serde_json_bytes::Value::Array(list) => segment
                    .parse::<usize>()
                    .ok()
                    .and_then(move |x| list.get_mut(x)),
                _ => None,
            });
        }
    }
    None
}

#[test]
fn it_works_with_one_segment() {
    let mut stuff = serde_json_bytes::json! {{
        "file1": null,
        "file2": null
    }};

    let variables = stuff.as_object_mut().unwrap();

    let path = &["file1".to_string()];

    assert_eq!(
        &mut serde_json_bytes::Value::Null,
        get_value_at_path(variables, path).unwrap()
    );
}
#[derive(Clone)]
struct SupergraphLayerResult {
    multipart: MultipartRequest,
    map: Arc<MapField>,
}

fn execution_layer(req: execution::Request) -> Result<execution::Request> {
    let supergraph_result = req
        .context
        .extensions()
        .lock()
        .get::<SupergraphLayerResult>()
        .cloned();
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult { map, .. } = supergraph_result;

        let query_plan = Arc::new(rearrange_query_plan(&req.query_plan, &map)?);
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
        let subgraph_map = map.sugraph_map(variables.keys());
        if !subgraph_map.is_empty() {
            for variable_map in map.per_variable.values() {
                for paths in variable_map.values() {
                    for path in paths {
                        remove_value_at_path(variables, path);
                    }
                }
            }

            req.subgraph_request
                .extensions_mut()
                .insert(MultipartFormData::new(subgraph_map, multipart));
        }
    }
    req
}

static APOLLO_REQUIRE_PREFLIGHT: HeaderName = HeaderName::from_static("apollo-require-preflight");
static TRUE: http::HeaderValue = HeaderValue::from_static("true");

pub(crate) async fn http_request_wrapper(
    mut req: http::Request<hyper::Body>,
) -> http::Request<hyper::Body> {
    let form = req.extensions_mut().get::<MultipartFormData>().cloned();
    if let Some(form) = form {
        let (mut request_parts, operations) = req.into_parts();
        request_parts
            .headers
            .insert(APOLLO_REQUIRE_PREFLIGHT.clone(), TRUE.clone());

        // override Content-Type to be 'multipart/form-data'
        request_parts
            .headers
            .insert(CONTENT_TYPE, form.content_type());
        let body = hyper::Body::wrap_stream(form.into_stream(operations).await);
        return http::Request::from_parts(request_parts, body);
    }
    req
}
