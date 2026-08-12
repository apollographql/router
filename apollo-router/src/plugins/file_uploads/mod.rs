use std::ops::ControlFlow;
use std::sync::Arc;

use apollo_json::JsonKind;
use apollo_json::NewValue;
use apollo_json::PathSegment;
use futures::FutureExt;
use http::HeaderValue;
use http::StatusCode;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use mediatype::MediaType;
use mediatype::ReadParams;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::config::FileUploadsConfig;
use self::config::MultipartRequestLimits;
use self::error::FileUploadError;
use self::file_upload_layer::FileUploadLayer;
use self::map_field::MapField;
use self::multipart_form_data::MultipartFormData;
use self::multipart_request::MultipartRequest;
use self::rearrange_query_plan::rearrange_query_plan;
use crate::graphql;
use crate::json_ext;
use crate::json_ext::ObjectExt;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::limits::BodyLimitControl;
use crate::services::execution;
use crate::services::router;
use crate::services::subgraph;
use crate::services::supergraph;

mod config;
mod error;
mod file_upload_layer;
mod map_field;
mod multipart_form_data;
mod multipart_request;
mod rearrange_query_plan;

type Result<T> = std::result::Result<T, error::FileUploadError>;

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

    fn router_service(&self, service: router::BoxCloneService) -> router::BoxCloneService {
        if !self.enabled {
            return service;
        }
        let limits = self.limits;
        let operation_body_timeout = limits.operation_body_timeout;
        ServiceBuilder::new()
            .checkpoint_async(move |req: router::Request| {
                async move {
                    let context = req.context.clone();
                    let layer_task = router_layer(req, limits);
                    let layer_result = if let Some(timeout) = operation_body_timeout {
                        match tokio::time::timeout(timeout, layer_task).await {
                            Ok(result) => result,
                            Err(_elapsed) => {
                                return Ok(ControlFlow::Break(operation_body_timeout_error(
                                    context,
                                )?));
                            }
                        }
                    } else {
                        layer_task.await
                    };
                    Ok(match layer_result {
                        Ok(req) => ControlFlow::Continue(req),
                        Err(err) => ControlFlow::Break(
                            router::Response::error_builder()
                                .status_code(err.http_status_code())
                                .errors(vec![err.into()])
                                .context(context)
                                .build()?,
                        ),
                    })
                }
                .boxed()
            })
            .service(service)
            .boxed_clone()
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneService,
    ) -> supergraph::BoxCloneService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .checkpoint_async(move |req: supergraph::Request| {
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
            .boxed_clone()
    }

    fn execution_service(&self, service: execution::BoxCloneService) -> execution::BoxCloneService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .checkpoint_async(|req: execution::Request| async move {
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
            .boxed_clone()
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .checkpoint_async(|req: subgraph::Request| {
                subgraph_layer(req)
                    .boxed()
                    .map(|req| Ok(ControlFlow::Continue(req)))
                    .boxed()
            })
            .service(service)
            .boxed_clone()
    }

    fn http_client_service(
        &self,
        _subgraph_name: &str,
        service: crate::services::http::BoxCloneService,
    ) -> crate::services::http::BoxCloneService {
        if !self.enabled {
            return service;
        }
        ServiceBuilder::new()
            .layer(FileUploadLayer)
            .service(service)
            .boxed_clone()
    }
}

fn get_multipart_mime(req: &router::Request) -> Option<MediaType<'_>> {
    req.router_request
        .headers()
        .get(CONTENT_TYPE)
        // Ignore parsing error, since they are reported by content_negotiation layer.
        .and_then(|header| header.to_str().ok())
        .and_then(|str| MediaType::parse(str).ok())
        .filter(|mime| mime.ty == MULTIPART && mime.subty == FORM_DATA)
}

fn operation_body_timeout_error(
    context: crate::Context,
) -> std::result::Result<router::Response, tower::BoxError> {
    router::Response::error_builder()
        .status_code(StatusCode::GATEWAY_TIMEOUT)
        .errors(vec![
            graphql::Error::builder()
                .message("The file upload operation body took too long to arrive")
                .extension_code("GATEWAY_TIMEOUT")
                .build(),
        ])
        .context(context)
        .build()
}

/// Takes in multipart request bodies, and turns them into serialized JSON bodies that the rest of the router
/// pipeline can understand.
///
/// # Context
/// Adds a [`MultipartRequest`] value to context.
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

        // Disable the global stream-level limit before multer reads anything.
        // limited::poll_frame fires when a single HTTP frame exceeds the remaining budget, but
        // hyper can deliver large frames on the first read (e.g. the entire multipart body). That
        // would trigger a spurious 413 before multer has extracted the small operations field.
        // Instead, we enforce http_max_request_bytes via multer's per-field SizeLimit on the
        // "operations" field (passed as operations_size_limit below), which counts content bytes
        // incrementally during streaming.
        let operations_size_limit = request_parts
            .extensions
            .get::<BodyLimitControl>()
            .and_then(|control| {
                let limit = control.limit();
                // update_limit asserts new > current, so skip if already at usize::MAX.
                if limit < usize::MAX {
                    control.update_limit(usize::MAX);
                }
                u64::try_from(limit).ok()
            })
            .unwrap_or(u64::MAX);

        let mut multipart =
            MultipartRequest::new(request_body, boundary, limits, operations_size_limit);
        let operations_stream = multipart.operations_field().await?;

        req.context
            .extensions()
            .with_lock(|lock| lock.insert(multipart));

        let content_type = operations_stream
            .headers()
            .get(CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("application/json"));

        // override Content-Type to content type of 'operations' field
        request_parts.headers.insert(CONTENT_TYPE, content_type);
        request_parts.headers.remove(CONTENT_LENGTH);

        let operations_bytes = operations_stream
            .bytes()
            .await
            .map_err(FileUploadError::InvalidMultipartRequest)?;

        let request_body = router::body::from_bytes(operations_bytes);
        return Ok(router::Request::from((
            http::Request::from_parts(request_parts, request_body),
            req.context,
        )));
    }

    Ok(req)
}

/// Patch up the variable values in file upload requests.
///
/// File uploads do something funky: They use *required* GraphQL field arguments (`file: Upload!`),
/// but then pass `null` as the variable value. This is invalid GraphQL, but it is how the file
/// uploads spec works.
///
/// To make all this work in the router, we stick some placeholder value in the variables used for
/// file uploads, and then remove them before we pass on the files to subgraphs.
async fn supergraph_layer(mut req: supergraph::Request) -> Result<supergraph::Request> {
    let multipart = req
        .context
        .extensions()
        .with_lock(|lock| lock.get::<MultipartRequest>().cloned());

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
                        format!("<Placeholder for file '{filename}'>"),
                    )
                    .map_err(|path| FileUploadError::InputValueNotFound(path.join(".")))?;
                }
            }
        }

        req.context.extensions().with_lock(|lock| {
            lock.insert(SupergraphLayerResult {
                multipart,
                map: Arc::new(map_field),
            })
        });
    }
    Ok(req)
}

// Replaces value at path with the provided one.
// Returns the provided path if the path is not valid for the given object
fn replace_value_at_path<'v, 'a>(
    variables: &mut json_ext::Value,
    path: &'a [String],
    value: impl Into<NewValue<'v>>,
) -> std::result::Result<(), &'a [String]> {
    match resolve_path(variables, path) {
        Some(segments) => {
            write_at_path(variables, &segments, value);
            Ok(())
        }
        None => Err(path),
    }
}

// Removes value at path.
fn remove_value_at_path(variables: &mut json_ext::Value, path: &[String]) {
    if let Some(segments) = resolve_path(variables, path) {
        write_at_path(variables, &segments, NewValue::Null);
    }
}

/// The mutation path addressing `path` inside `variables`, or `None` when a
/// segment names a member that is absent or a container that is not there.
fn resolve_path<'a>(
    variables: &json_ext::Value,
    path: &'a [String],
) -> Option<Vec<PathSegment<'a>>> {
    let (variable_name, rest) = path.split_first()?;
    let mut current = variables.get(variable_name.as_str())?;
    let mut segments = Vec::with_capacity(path.len());
    segments.push(PathSegment::Key(variable_name.as_str()));

    for segment in rest {
        current = match current.kind() {
            JsonKind::Object => {
                segments.push(PathSegment::Key(segment.as_str()));
                current.get(segment.as_str())
            }
            JsonKind::Array => {
                let index = segment.parse::<usize>().ok()?;
                segments.push(PathSegment::Index(index));
                current.index(index)
            }
            _ => None,
        }?;
    }

    Some(segments)
}

fn write_at_path<'v>(
    variables: &mut json_ext::Value,
    segments: &[PathSegment<'_>],
    value: impl Into<NewValue<'v>>,
) {
    let mut builder = variables.detach().edit();
    builder
        .set_path(segments, value)
        .expect("the segments resolved against this object");
    *variables = builder.seal().root_handle();
}

#[test]
fn it_works_with_one_segment() {
    let mut variables = json_ext::from_legacy(&serde_json_bytes::json! {{
        "file1": null,
        "file2": null
    }});

    replace_value_at_path(&mut variables, &["file1".to_string()], "placeholder")
        .expect("file1 is a member of the variables");

    assert_eq!(
        variables
            .get("file1")
            .and_then(|v| v.as_str().map(|s| s.to_string())),
        Some("placeholder".to_string())
    );
    assert!(variables.get("file2").expect("file2 is present").is_null());
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
        .with_lock(|lock| lock.get::<SupergraphLayerResult>().cloned());
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
        .with_lock(|lock| lock.get::<SupergraphLayerResult>().cloned());
    if let Some(supergraph_result) = supergraph_result {
        let SupergraphLayerResult { multipart, map } = supergraph_result;

        let variables = &mut req.subgraph_request.body_mut().variables;
        let variable_names: Vec<serde_json_bytes::ByteString> =
            variables.object_keys().into_iter().map(Into::into).collect();
        let subgraph_map = map.sugraph_map(&variable_names);
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
